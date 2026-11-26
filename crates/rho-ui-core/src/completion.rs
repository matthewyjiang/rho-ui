//! Completing what the user types into a prompt: a command or skill name,
//! an MCP resource to attach, or the value of an MCP prompt's argument.
//!
//! A prompt that starts with a single `/` runs the slash command its first
//! word names; a `$name` word anywhere in it mentions a skill, which the turn
//! loads first. The session lists both ([`crate::Session::commands`]) and the
//! daemon reads them in the prompt as typed, so completing a name only edits
//! the text ([`Completion`]).
//!
//! The session's MCP servers offer resources and prompts
//! ([`crate::Daemon::mcp_catalog`]). An `@` word names a resource
//! ([`Mention`]), which goes with the prompt as an attachment rather than as
//! text. A prompt runs as the slash command `mcp:<server>:<name>`, and the
//! text after it holds its arguments: the whole of it for a prompt of one
//! argument, else `name=value` words. A server may suggest values for the
//! argument being typed ([`ArgumentValue`]). The rules are those of rho's
//! terminal UI.

use std::ops::Range;

use rho_protocol::{SessionMcpPrompt, SessionMcpResource};

use crate::{CommandInfo, CommandKind};

/// The command or skill name being typed at the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    kind: CommandKind,
    /// The whitespace-delimited word holding the cursor, sigil included, in
    /// bytes. Completing replaces all of it.
    word: Range<usize>,
    /// What is typed of the name: from after the sigil to the cursor.
    typed: String,
}

/// A change to a prompt's text: replace the bytes in `range` with `text`,
/// then put the cursor at byte `cursor` of the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    /// The bytes to replace.
    pub range: Range<usize>,
    /// What replaces them.
    pub text: String,
    /// Where the cursor goes, in the edited text.
    pub cursor: usize,
}

impl Completion {
    /// The name being typed at byte `cursor` of `text`, if the cursor sits
    /// after the sigil of a name: in the first word of a prompt that starts
    /// with a single `/`, or in a word that starts with `$` and holds only
    /// what a skill's name can.
    #[must_use]
    pub fn at(text: &str, cursor: usize) -> Option<Self> {
        let Range { start, end } = word_at(text, cursor)?;
        let word = &text[start..end];
        let kind = if start == 0 && word.starts_with('/') && !word.starts_with("//") {
            CommandKind::Slash
        } else {
            let name = word.strip_prefix('$')?;
            if !name.chars().all(is_skill_name_char) {
                return None;
            }
            CommandKind::Skill
        };
        // Both sigils are one byte.
        let name_start = start + 1;
        (cursor >= name_start).then(|| Self {
            kind,
            word: start..end,
            typed: text[name_start..cursor].to_owned(),
        })
    }

    /// Whether a slash command or a skill is being typed.
    pub fn kind(&self) -> CommandKind {
        self.kind
    }

    /// What is typed of the name, without its sigil.
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// The `commands` of this kind whose names start with the typed name,
    /// ignoring case, in the order the session lists them.
    pub fn matches<'a>(
        &'a self,
        commands: &'a [CommandInfo],
    ) -> impl Iterator<Item = &'a CommandInfo> + 'a {
        commands.iter().filter(|command| {
            command.kind() == self.kind && starts_with_ignoring_case(command.name(), &self.typed)
        })
    }

    /// The edit that completes the word to `command`: its sigil and name,
    /// then a space unless one follows already, with the cursor after the
    /// space, ready for the command's input or the next word.
    #[must_use]
    pub fn edit(&self, text: &str, command: &CommandInfo) -> Edit {
        let mut insert = format!("{}{}", self.kind.sigil(), command.name());
        let cursor = self.word.start + insert.len() + 1;
        if !text[self.word.end..].starts_with(' ') {
            insert.push(' ');
        }
        Edit {
            range: self.word.clone(),
            text: insert,
            cursor,
        }
    }

    /// Whether `command`'s name is typed in full, so there is nothing left
    /// to complete.
    pub fn is_typed_out(&self, command: &CommandInfo) -> bool {
        command.name().eq_ignore_ascii_case(&self.typed)
    }

    /// Whether what is typed reads as the start of a name. rho reads `$20`,
    /// `$20k` and `$1e6` as amounts, not skills, so a skill's name needs a
    /// letter and must not read as one; `$` alone is not a name yet either.
    pub fn reads_as_name(&self) -> bool {
        match self.kind {
            CommandKind::Slash => true,
            CommandKind::Skill => {
                self.typed.bytes().any(|byte| byte.is_ascii_alphabetic())
                    && !reads_as_amount(&self.typed)
            }
        }
    }
}

/// The whitespace-delimited word holding byte `cursor` of `text`, in
/// bytes; `None` when `cursor` is not a character boundary.
fn word_at(text: &str, cursor: usize) -> Option<Range<usize>> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let start = text[..cursor]
        .char_indices()
        .rfind(|(_, ch)| ch.is_whitespace())
        .map_or(0, |(at, ch)| at + ch.len_utf8());
    let end = text[cursor..]
        .find(char::is_whitespace)
        .map_or(text.len(), |at| cursor + at);
    Some(start..end)
}

/// The MCP resource being named at the cursor: a word that starts with `@`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// The word, `@` included, in bytes.
    word: Range<usize>,
    /// What is typed after the `@`, up to the cursor.
    typed: String,
}

impl Mention {
    /// The mention at byte `cursor` of `text`, if the cursor sits after the
    /// `@` that starts the word holding it.
    #[must_use]
    pub fn at(text: &str, cursor: usize) -> Option<Self> {
        let word = word_at(text, cursor)?;
        if !text[word.clone()].starts_with('@') || cursor == word.start {
            return None;
        }
        Some(Self {
            typed: text[word.start + 1..cursor].to_owned(),
            word,
        })
    }

    /// What is typed after the `@`.
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// Where the mention starts, in bytes: another start is another mention.
    pub fn start(&self) -> usize {
        self.word.start
    }

    /// The `resources` whose URI, name or title holds what is typed,
    /// ignoring case, in the order the session lists them.
    pub fn matches<'r>(
        &self,
        resources: &'r [SessionMcpResource],
    ) -> impl Iterator<Item = &'r SessionMcpResource> + 'r {
        let typed = self.typed.to_lowercase();
        resources.iter().filter(move |resource| {
            [
                Some(&resource.uri),
                Some(&resource.name),
                resource.title.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|key| key.to_lowercase().contains(&typed))
        })
    }

    /// The edit that takes the mention out, for a resource that goes as an
    /// attachment: the word, and one space after it if there is one, so
    /// the words around it stay one space apart.
    #[must_use]
    pub fn remove(&self, text: &str) -> Edit {
        let mut end = self.word.end;
        if text[end..].starts_with(' ') {
            end += 1;
        }
        Edit {
            range: self.word.start..end,
            text: String::new(),
            cursor: self.word.start,
        }
    }

    /// The edit that writes `uri` as the mention, with a space after it
    /// unless one follows: a resource template, whose placeholders the user
    /// fills in, goes as text.
    #[must_use]
    pub fn edit(&self, text: &str, uri: &str) -> Edit {
        let mut insert = format!("@{uri}");
        let cursor = self.word.start + insert.len() + 1;
        if !text[self.word.end..].starts_with(' ') {
            insert.push(' ');
        }
        Edit {
            range: self.word.clone(),
            text: insert,
            cursor,
        }
    }
}

/// What to ask a server for: values of `argument` of its prompt `prompt`,
/// given what is typed of it, as `rho/session/mcp/complete` asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentAsk {
    /// The server's qualified identity.
    pub server: String,
    /// The prompt's name.
    pub prompt: String,
    /// The argument's name.
    pub argument: String,
    /// What is typed of its value; may be empty.
    pub value: String,
}

/// The value of an MCP prompt's argument being typed at the cursor, for a
/// prompt whose server suggests values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentValue {
    ask: ArgumentAsk,
    /// The value, in bytes. Completing replaces all of it.
    value: Range<usize>,
}

impl ArgumentValue {
    /// The argument value at byte `cursor` of `text`, a prompt that starts
    /// with `/mcp:<server>:<name>`, the command of one of `prompts` whose
    /// server suggests values. A prompt of one argument takes all the
    /// text after the command, trimmed; any other, the `name=value` word
    /// holding the cursor, past its `=`, for an argument it declares.
    #[must_use]
    pub fn at(text: &str, cursor: usize, prompts: &[SessionMcpPrompt]) -> Option<Self> {
        let token_end = mcp_command(text)?.len();
        // Within the command the command's own name is being typed.
        if cursor <= token_end || !text.is_char_boundary(cursor) {
            return None;
        }
        let (server, name) = prompt_command(&text[..token_end])?;
        let prompt = prompts.iter().find(|prompt| {
            prompt.completes_arguments && prompt.server == server && prompt.name == name
        })?;
        let (argument, value) = if let [only] = prompt.arguments.as_slice() {
            let after = &text[token_end..];
            let start = token_end + (after.len() - after.trim_start().len());
            (only.name.clone(), start..start + after.trim().len())
        } else {
            let word = word_at(text, cursor)?;
            let equals = word.start + text[word.clone()].find('=')?;
            // Before the `=`, the argument's name is being typed.
            if cursor <= equals {
                return None;
            }
            let argument = &text[word.start..equals];
            prompt
                .arguments
                .iter()
                .find(|declared| declared.name == argument)?;
            (argument.to_owned(), equals + 1..word.end)
        };
        Some(Self {
            ask: ArgumentAsk {
                server: prompt.server.clone(),
                prompt: prompt.name.clone(),
                argument,
                value: text[value.clone()].to_owned(),
            },
            value,
        })
    }

    /// What to ask the server.
    pub fn ask(&self) -> &ArgumentAsk {
        &self.ask
    }

    /// The edit that makes the value `value`, with the cursor after it.
    #[must_use]
    pub fn edit(&self, value: &str) -> Edit {
        Edit {
            range: self.value.clone(),
            text: value.to_owned(),
            cursor: self.value.start + value.len(),
        }
    }
}

/// The MCP prompt command `text` starts with, such as
/// `/mcp:docs:search`, once something follows it: past the command, its
/// arguments are being typed.
#[must_use]
pub fn mcp_command(text: &str) -> Option<&str> {
    let token = &text[..text.find(char::is_whitespace)?];
    prompt_command(token).map(|_| token)
}

/// The server and prompt names of `/mcp:<server>:<name>`, its prefix in
/// any case. A server's identity holds no `:`, so the first one after the
/// prefix splits them.
fn prompt_command(token: &str) -> Option<(&str, &str)> {
    const PREFIX: &str = "/mcp:";
    let rest = token
        .get(..PREFIX.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
        .and_then(|_| token.get(PREFIX.len()..))?;
    let (server, name) = rest.split_once(':')?;
    (!server.is_empty() && !name.is_empty()).then_some((server, name))
}

/// What a skill's name may hold after its `$`.
fn is_skill_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':')
}

fn starts_with_ignoring_case(name: &str, prefix: &str) -> bool {
    name.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// `[0-9][0-9_]*`, then optionally one `k`, `m`, `b` or `t`, or `e` and
/// digits: how rho tells an amount from a skill.
fn reads_as_amount(name: &str) -> bool {
    let digits_end = name
        .bytes()
        .position(|byte| !(byte.is_ascii_digit() || byte == b'_'))
        .unwrap_or(name.len());
    if !name.starts_with(|ch: char| ch.is_ascii_digit()) {
        return false;
    }
    match name.as_bytes()[digits_end..] {
        [] => true,
        [unit] => matches!(unit.to_ascii_lowercase(), b'k' | b'm' | b'b' | b't'),
        [exponent, ref power @ ..] => {
            exponent.eq_ignore_ascii_case(&b'e') && power.iter().all(u8::is_ascii_digit)
        }
    }
}

#[cfg(test)]
mod tests {
    use rho_protocol::SessionCommand;

    use super::*;
    use crate::options::commands;

    /// The session's list: two commands and two skills, in menu order.
    fn offered() -> Vec<CommandInfo> {
        let command = |name: &str, kind| SessionCommand {
            name: name.to_owned(),
            kind,
            description: String::new(),
            input_hint: None,
        };
        commands(vec![
            command("compact", rho_protocol::CommandKind::Slash),
            command("Prompt:review", rho_protocol::CommandKind::Slash),
            command("pstack", rho_protocol::CommandKind::Skill),
            command("prove-it", rho_protocol::CommandKind::Skill),
        ])
    }

    /// The text with `|` marking the cursor, as the completion there sees it.
    fn at(marked: &str) -> Option<Completion> {
        let cursor = marked.find('|').expect("the text marks the cursor");
        Completion::at(&marked.replace('|', ""), cursor)
    }

    fn names(completion: &Completion) -> Vec<String> {
        let offered = offered();
        completion
            .matches(&offered)
            .map(|command| command.name().to_owned())
            .collect()
    }

    /// The text after completing `marked` to the first match, cursor marked.
    fn completed(marked: &str) -> String {
        let completion = at(marked).expect("a name is typed");
        let offered = offered();
        let command = completion.matches(&offered).next().expect("a match");
        let mut text = marked.replace('|', "");
        let edit = completion.edit(&text, command);
        text.replace_range(edit.range, &edit.text);
        text.insert(edit.cursor, '|');
        text
    }

    #[test]
    fn a_slash_command_is_the_first_word_of_the_prompt() {
        let completion = at("/p|").expect("a command is typed");
        assert_eq!(completion.kind(), CommandKind::Slash);
        assert_eq!(names(&completion), ["Prompt:review"], "case is ignored");
        assert_eq!(
            names(&at("/|").expect("a command")),
            ["compact", "Prompt:review"]
        );
        // The name is what precedes the cursor; the whole word is replaced.
        assert_eq!(at("/co|mpact").expect("a command").typed(), "co");

        for marked in [
            "|/co",
            "/compact now|",
            " /co|",
            "hi /co|",
            "//co|",
            "/co\nmore /p|",
        ] {
            assert_eq!(at(marked), None, "{marked:?}");
        }
    }

    #[test]
    fn a_skill_is_any_word_that_starts_with_a_dollar() {
        for marked in ["$p|", "use $p| here", "/compact keep $p|", "line\n$p|"] {
            let completion = at(marked).unwrap_or_else(|| panic!("a skill in {marked:?}"));
            assert_eq!(completion.kind(), CommandKind::Skill, "{marked:?}");
            assert_eq!(names(&completion), ["pstack", "prove-it"], "{marked:?}");
        }
        for marked in ["a$p|", "$p.|", "$|p(", "costs 5|"] {
            assert_eq!(at(marked), None, "{marked:?}");
        }
    }

    #[test]
    fn completing_replaces_the_word_and_leaves_one_space() {
        assert_eq!(completed("/co|"), "/compact |");
        assert_eq!(completed("/co| keep the plan"), "/compact |keep the plan");
        assert_eq!(completed("/co|mp"), "/compact |");
        assert_eq!(completed("use $ps| here"), "use $pstack |here");
        assert_eq!(completed("use $ps|\nhere"), "use $pstack |\nhere");
    }

    #[test]
    fn amounts_do_not_read_as_skill_names() {
        for marked in ["$|", "$20|", "$20k|", "$1_000|", "$1e6|"] {
            let completion = at(marked).unwrap_or_else(|| panic!("a word in {marked:?}"));
            assert!(!completion.reads_as_name(), "{marked:?}");
        }
        for marked in ["$p|", "$a1|", "$20x|", "/|"] {
            let completion = at(marked).unwrap_or_else(|| panic!("a word in {marked:?}"));
            assert!(completion.reads_as_name(), "{marked:?}");
        }
    }

    /// `marked` with `|` as the cursor, after `edit`, cursor marked.
    fn edited(marked: &str, edit: Edit) -> String {
        let mut text = marked.replace('|', "");
        text.replace_range(edit.range, &edit.text);
        text.insert(edit.cursor, '|');
        text
    }

    fn cursor(marked: &str) -> (String, usize) {
        let cursor = marked.find('|').expect("the text marks the cursor");
        (marked.replace('|', ""), cursor)
    }

    fn resource(uri: &str, name: &str) -> SessionMcpResource {
        SessionMcpResource {
            server: "docs".to_owned(),
            uri: uri.to_owned(),
            name: name.to_owned(),
            title: None,
            description: None,
            mime_type: None,
            templated: false,
        }
    }

    /// An `@` word names a resource by its URI or name; picking one takes
    /// the word out, or writes a template's URI in its place.
    #[test]
    fn a_mention_is_an_at_word() {
        let resources = [
            resource("docs://guide/intro", "Intro"),
            resource("docs://api", "Reference"),
        ];
        let cases = [
            ("@|", Some(vec!["docs://guide/intro", "docs://api"])),
            ("see @INT|", Some(vec!["docs://guide/intro"])),
            ("see @ref| now", Some(vec!["docs://api"])),
            ("line\n@ap|", Some(vec!["docs://api"])),
            ("@nothing|", Some(vec![])),
            ("me@ap|", None),
            ("|@ap", None),
            ("@ap @x |", None),
        ];
        for (marked, want) in cases {
            let (text, at) = cursor(marked);
            let got = Mention::at(&text, at).map(|mention| {
                mention
                    .matches(&resources)
                    .map(|resource| resource.uri.as_str())
                    .collect::<Vec<_>>()
            });
            assert_eq!(got, want, "{marked:?}");
        }

        let mention = |marked: &str| {
            let (text, at) = cursor(marked);
            Mention::at(&text, at).expect("a mention")
        };
        for (marked, removed) in [
            ("@in|", "|"),
            ("see @in| now", "see |now"),
            ("see @in|", "see |"),
        ] {
            let edit = mention(marked).remove(&marked.replace('|', ""));
            assert_eq!(edited(marked, edit), removed, "{marked:?}");
        }
        let edit = mention("see @us| now").edit("see @us now", "docs://users/{id}");
        assert_eq!(edited("see @us| now", edit), "see @docs://users/{id} |now");
    }

    /// The value being typed of a prompt's argument: all the text after
    /// the command for a prompt of one argument, else the `name=value`
    /// word, past its `=`; only for a prompt whose server suggests values.
    #[test]
    fn an_argument_value_is_found_by_the_prompts_arguments() {
        let argument = |name: &str| rho_protocol::SessionMcpPromptArgument {
            name: name.to_owned(),
            description: None,
            required: true,
        };
        let prompt = |name: &str, arguments: Vec<_>, completes_arguments| SessionMcpPrompt {
            server: "docs".to_owned(),
            name: name.to_owned(),
            title: None,
            description: None,
            arguments,
            completes_arguments,
        };
        let prompts = [
            prompt("search", vec![argument("query")], true),
            prompt("open", vec![argument("page"), argument("lang")], true),
            prompt("quiet", vec![argument("query")], false),
        ];
        let cases = [
            ("/mcp:docs:search rust own|", Some(("query", "rust own"))),
            ("/MCP:docs:search  rust |  ", Some(("query", "rust"))),
            ("/mcp:docs:search |", Some(("query", ""))),
            (
                "/mcp:docs:open page=in|tro lang=en",
                Some(("page", "intro")),
            ),
            ("/mcp:docs:open page=intro lang=|", Some(("lang", ""))),
            ("/mcp:docs:open pa|ge=intro", None),
            ("/mcp:docs:open size=|", None),
            ("/mcp:docs:quiet rust|", None),
            ("/mcp:docs:sea|rch rust", None),
            ("/mcp:other:search rust|", None),
            ("ask /mcp:docs:search rust|", None),
        ];
        for (marked, want) in cases {
            let (text, at) = cursor(marked);
            let got = ArgumentValue::at(&text, at, &prompts);
            let got = got
                .as_ref()
                .map(|value| (value.ask().argument.as_str(), value.ask().value.as_str()));
            assert_eq!(got, want, "{marked:?}");
        }

        let (text, at) = cursor("/mcp:docs:open page=in| lang=en");
        let value = ArgumentValue::at(&text, at, &prompts).expect("a value");
        assert_eq!(
            edited("/mcp:docs:open page=in| lang=en", value.edit("intro")),
            "/mcp:docs:open page=intro| lang=en"
        );
    }

    #[test]
    fn a_name_typed_in_full_is_typed_out() {
        let offered = offered();
        let compact = &offered[0];
        assert!(at("/COMPACT|").expect("a command").is_typed_out(compact));
        assert!(!at("/comp|").expect("a command").is_typed_out(compact));
    }
}
