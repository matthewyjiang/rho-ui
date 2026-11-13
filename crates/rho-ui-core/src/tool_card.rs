//! What a tool call shows a person: the card rho's terminal UI draws for
//! it, which the daemon sends with the call's updates. A call's own content
//! is what the model read, such as a whole file, a command's output wrapped
//! in `stdout:` and `exit code:` lines, or an edit's receipt; the card holds
//! only what a person needs: one line, short facts, and what opens under it.

use std::ops::Range;

use regex::Regex;
use rho_protocol::display::{
    Presentation, ToolBody as WireBody, ToolCard as WireCard, ToolFact as WireFact, ToolHeader,
};

/// How a tool call looks, from the daemon's latest card for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCard {
    header: String,
    facts: Vec<ToolFact>,
    body: ToolBody,
}

/// One fact about a tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolFact {
    /// Lines the call added and removed.
    Changed {
        /// Lines added.
        added: u64,
        /// Lines removed.
        removed: u64,
    },
    /// A short result for the call's line, such as `32 lines`,
    /// `13 matches in 2 files` or `exit 1`.
    Stat(String),
    /// A remark for under the line, such as the command a process runs or
    /// an item of a checklist.
    Note(String),
    /// What went wrong.
    Error(String),
}

/// What opens under a tool call's line, besides the files it changed: the
/// call's own diffs show those, from the whole texts, so a card's compact
/// diff is left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolBody {
    /// Nothing.
    #[default]
    None,
    /// Output, verbatim.
    Text(String),
    /// Markdown, such as a message from an agent.
    Markdown(String),
    /// Source code, such as a script or a file being written.
    Code {
        /// What to highlight it as: a syntax name or file extension, such as
        /// `python` or `rs`.
        language: String,
        /// The code.
        text: String,
    },
    /// What a search found, file by file.
    Search(Vec<SearchLine>),
}

/// One line of a search's results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchLine {
    /// A file the lines after it are in.
    File(String),
    /// A matching line.
    Hit {
        /// Its line number in the file.
        line: u32,
        /// The line, without its indentation.
        text: String,
        /// Where the search's pattern matches `text`, as byte ranges.
        matches: Vec<Range<usize>>,
    },
    /// Anything else rho says, such as `... +4 more in this file` or why a
    /// search stopped short.
    Note(String),
}

impl ToolCard {
    /// The call in one line, such as `read_file(src/main.rs:1-40)`, the
    /// command it runs, or `reviewer · completed`. Empty while the daemon
    /// is still streaming what the call is.
    pub fn header(&self) -> &str {
        &self.header
    }

    /// The call's facts, in the daemon's order.
    pub fn facts(&self) -> &[ToolFact] {
        &self.facts
    }

    /// What opens under the call's line.
    pub fn body(&self) -> &ToolBody {
        &self.body
    }

    pub(crate) fn from_wire(presentation: Presentation) -> Self {
        match presentation {
            Presentation::Card(card) | Presentation::SummaryCard(card) => Self::from_card(card),
            Presentation::Notification(note) => {
                let body = note.markdown_body();
                Self {
                    body: if body.trim().is_empty() {
                        ToolBody::None
                    } else {
                        ToolBody::Markdown(body)
                    },
                    header: note.title,
                    facts: note.details.into_iter().map(ToolFact::Note).collect(),
                }
            }
        }
    }

    fn from_card(card: WireCard) -> Self {
        let header = match card.header {
            ToolHeader::Call { verb, primary } => match primary {
                Some(primary) if !primary.is_empty() => format!("{verb}({primary})"),
                _ => verb,
            },
            // The command alone: the call's icon says it is one.
            ToolHeader::Shell { command, .. } => command.unwrap_or_default(),
            ToolHeader::StatusFirst { identity, detail } if detail.is_empty() => identity,
            ToolHeader::StatusFirst { identity, detail } => format!("{identity} · {detail}"),
            // A header this build does not know: the call's title stands in.
            _ => String::new(),
        };
        let facts: Vec<ToolFact> = card.facts.into_iter().filter_map(shown_fact).collect();
        let body = match card.body {
            body if body.is_empty() => ToolBody::None,
            WireBody::Lines(lines) => ToolBody::Text(lines.join("\n")),
            WireBody::Code {
                language, lines, ..
            } => ToolBody::Code {
                language,
                text: lines.join("\n"),
            },
            WireBody::Search {
                pattern,
                literal,
                case_sensitive,
                lines,
            } => {
                // Rust regex syntax, as rho's grep searched with. A pattern
                // this crate's regex refuses goes unmarked.
                let pattern = if literal {
                    regex::escape(&pattern)
                } else {
                    pattern
                };
                let flags = if case_sensitive { "" } else { "(?i)" };
                let pattern = Regex::new(&format!("{flags}{pattern}")).ok();
                ToolBody::Search(search_lines(&lines, pattern.as_ref(), &facts))
            }
            WireBody::None | WireBody::Diff(_) => ToolBody::None,
        };
        Self {
            header,
            facts,
            body,
        }
    }
}

/// rho's grep results, read back: a path, then `N | text` for each line that
/// matched in it, perhaps `... +N more in this file`, and after a blank line
/// how many it found and why it stopped short. A note that only repeats one
/// of `facts` is left out: the call's line shows it.
fn search_lines(lines: &[String], pattern: Option<&Regex>, facts: &[ToolFact]) -> Vec<SearchLine> {
    let mut results = Vec::new();
    let mut in_files = true;
    for line in lines {
        if line.trim().is_empty() {
            in_files = false;
            continue;
        }
        let hit = line
            .split_once(" | ")
            .and_then(|(number, text)| Some((number.parse().ok()?, text)));
        let parsed = match hit {
            Some((line, text)) if in_files => hit_line(line, text, pattern),
            None if in_files && !line.starts_with("... ") => SearchLine::File(line.clone()),
            _ if facts.contains(&ToolFact::Stat(line.clone())) => continue,
            _ => SearchLine::Note(line.clone()),
        };
        results.push(parsed);
    }
    results
}

/// A matching line, without its indentation, and where `pattern` matches it.
/// It matches the whole line, as the search did, so a pattern that counts
/// the indentation still finds its place.
fn hit_line(line: u32, text: &str, pattern: Option<&Regex>) -> SearchLine {
    let indent = text.len() - text.trim_start().len();
    let matches = pattern
        .into_iter()
        .flat_map(|pattern| pattern.find_iter(text))
        .map(|found| found.start().max(indent) - indent..found.end().max(indent) - indent)
        .filter(|range| !range.is_empty())
        .collect();
    SearchLine::Hit {
        line,
        text: text[indent..].to_owned(),
        matches,
    }
}

/// `fact` as a UI shows it; `None` for one that only adds noise there: a
/// shell's timeout budget, a zero exit (the call's status says it), and
/// kinds this build does not know.
fn shown_fact(fact: WireFact) -> Option<ToolFact> {
    let shown = match fact {
        WireFact::DiffStat { added, removed, .. } => ToolFact::Changed { added, removed },
        WireFact::Exit { code: 0, .. } | WireFact::Timeout { .. } => return None,
        WireFact::Exit { code, .. } => ToolFact::Stat(format!("exit {code}")),
        WireFact::Count { .. } | WireFact::Progress { .. } => ToolFact::Stat(fact.plain_text()),
        WireFact::Meta { text } | WireFact::Text { text } => ToolFact::Note(text),
        WireFact::Error { text } => ToolFact::Error(text),
        _ => return None,
    };
    Some(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grep card's body, as this crate reads it back.
    fn search(body: WireBody) -> ToolBody {
        let card = WireCard::new(
            rho_protocol::display::ToolCardStatus::Ok,
            rho_protocol::display::ToolFamily::FileCommand,
            ToolHeader::call("grep", None),
        )
        .with_facts(vec![WireFact::Count {
            label: "matches".to_owned(),
            value: 2,
            detail: Some("in 1 files".to_owned()),
        }])
        .with_body(body);
        ToolCard::from_card(card).body
    }

    fn owned(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    fn hit(line: u32, text: &str, matches: Vec<Range<usize>>) -> SearchLine {
        SearchLine::Hit {
            line,
            text: text.to_owned(),
            matches,
        }
    }

    /// Matches follow the search's own flags, counted from the line without
    /// its indentation; a count the call's line already shows is dropped,
    /// and rho's other remarks stay.
    #[test]
    fn a_search_reads_back_file_by_file_with_its_matches() {
        let lines = [
            "src/a.rs",
            "3 |     Foo.bar(foo)",
            "... +4 more in this file",
            "",
            "2 matches in 1 files",
        ];
        assert_eq!(
            search(WireBody::Search {
                pattern: "foo".to_owned(),
                literal: false,
                case_sensitive: false,
                lines: owned(&lines),
            }),
            ToolBody::Search(vec![
                SearchLine::File("src/a.rs".to_owned()),
                hit(3, "Foo.bar(foo)", vec![0..3, 8..11]),
                SearchLine::Note("... +4 more in this file".to_owned()),
            ])
        );
        assert_eq!(
            search(WireBody::Search {
                pattern: "o.b".to_owned(),
                literal: true,
                case_sensitive: true,
                lines: owned(&lines[..2]),
            }),
            ToolBody::Search(vec![
                SearchLine::File("src/a.rs".to_owned()),
                hit(3, "Foo.bar(foo)", vec![Range { start: 2, end: 5 }]),
            ])
        );
    }

    /// A pattern this crate's regex refuses still lists what rho found.
    #[test]
    fn a_pattern_regex_refuses_goes_unmarked() {
        assert_eq!(
            search(WireBody::Search {
                pattern: "(?<=a)b".to_owned(),
                literal: false,
                case_sensitive: true,
                lines: owned(&["x", "1 | ab"]),
            }),
            ToolBody::Search(vec![SearchLine::File("x".to_owned()), hit(1, "ab", vec![])])
        );
    }
}
