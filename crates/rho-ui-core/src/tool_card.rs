//! What a tool call shows a person: the card rho's terminal UI draws for
//! it, which the daemon sends with the call's updates. A call's own content
//! is what the model read, such as a whole file, a command's output wrapped
//! in `stdout:` and `exit code:` lines, or an edit's receipt; the card holds
//! only what a person needs: one line, short facts, and what opens under it.

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
    /// Output, source code or search results, verbatim.
    Text(String),
    /// Markdown, such as a message from an agent.
    Markdown(String),
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
        let facts = card.facts.into_iter().filter_map(shown_fact).collect();
        let body = match card.body {
            body if body.is_empty() => ToolBody::None,
            WireBody::Lines(lines)
            | WireBody::Code { lines, .. }
            | WireBody::Search { lines, .. } => ToolBody::Text(lines.join("\n")),
            WireBody::None | WireBody::Diff(_) => ToolBody::None,
        };
        Self {
            header,
            facts,
            body,
        }
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
