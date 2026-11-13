//! Completing a command or skill name as the user types it into a prompt.
//!
//! A prompt that starts with a single `/` runs the slash command its first
//! word names; a `$name` word anywhere in it mentions a skill, which the turn
//! loads first. The session lists both ([`crate::Session::commands`]) and the
//! daemon reads them in the prompt as typed, so completing a name only edits
//! the text. The rules are those of rho's terminal UI.

use std::ops::Range;

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

    #[test]
    fn a_name_typed_in_full_is_typed_out() {
        let offered = offered();
        let compact = &offered[0];
        assert!(at("/COMPACT|").expect("a command").is_typed_out(compact));
        assert!(!at("/comp|").expect("a command").is_typed_out(compact));
    }
}
