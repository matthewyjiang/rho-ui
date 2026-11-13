//! A transcript's rows and turns. A user message, a reply or a notice is a
//! row of its own; each run of tool calls and thoughts between them is one
//! work row, which a UI draws as a single line that expands. Each prompt
//! starts a turn, as does each turn the daemon began itself
//! ([`Entry::Wake`]); a UI can fold a turn's working once it finishes.

use std::ops::Range;

use crate::{Entry, Message, ToolCall, ToolKind, ToolStatus};

/// An entry a work row holds.
#[derive(Clone, Copy, Debug)]
pub enum Step<'a> {
    /// The agent's visible reasoning.
    Thought(&'a Message),
    /// A tool call.
    Tool(&'a ToolCall),
}

impl Entry {
    /// The entry as a work step, if it is one.
    pub fn as_step(&self) -> Option<Step<'_>> {
        match self {
            Entry::Thought(thought) => Some(Step::Thought(thought)),
            Entry::Tool(tool) => Some(Step::Tool(tool)),
            Entry::User(_) | Entry::Agent(_) | Entry::Notice(_) | Entry::Wake => None,
        }
    }
}

/// One transcript row: entry indices into the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A user message, a reply, a notice, or a turn's wake.
    Message(usize),
    /// Consecutive tool calls and thoughts.
    Work(Range<usize>),
}

impl Row {
    fn start(&self) -> usize {
        match self {
            Row::Message(ix) => *ix,
            Row::Work(entries) => entries.start,
        }
    }
}

/// The rows for a session's entries, kept up to date as entries arrive.
#[derive(Debug, Default)]
pub struct Timeline {
    rows: Vec<Row>,
    /// Each turn's prompt, as a row index, in order.
    turns: Vec<usize>,
    /// Entries already placed in a row.
    seen: usize,
}

/// What [`Timeline::sync`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// The last row took new entries, so its height changed.
    pub grew: Option<usize>,
    /// Rows added at the end.
    pub added: usize,
}

impl Timeline {
    /// Places entries added since the last call. Entries are only appended
    /// or changed in place, so only the last row can grow.
    pub fn sync(&mut self, entries: &[Entry]) -> Synced {
        let before = self.rows.len();
        let mut grew = None;
        for (ix, entry) in entries.iter().enumerate().skip(self.seen) {
            match self.rows.last_mut() {
                Some(Row::Work(run)) if is_work(entry) && run.end == ix => {
                    run.end += 1;
                    // A row added in this call is counted as added instead.
                    if self.rows.len() == before {
                        grew = Some(before - 1);
                    }
                }
                _ if is_work(entry) => self.rows.push(Row::Work(ix..ix + 1)),
                _ => {
                    if matches!(entry, Entry::User(_) | Entry::Wake) {
                        self.turns.push(self.rows.len());
                    }
                    self.rows.push(Row::Message(ix));
                }
            }
        }
        self.seen = entries.len();
        Synced {
            grew,
            added: self.rows.len() - before,
        }
    }

    /// Every row, in order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Each turn's prompt row, oldest first.
    pub fn turns(&self) -> &[usize] {
        &self.turns
    }

    /// The turn row `row` belongs to; rows before the first prompt have none.
    pub fn turn_of(&self, row: usize) -> Option<usize> {
        self.turns
            .partition_point(|&prompt| prompt <= row)
            .checked_sub(1)
    }

    /// The rows of `turn`, its prompt included.
    pub fn turn_rows(&self, turn: usize) -> Range<usize> {
        let end = self.turns.get(turn + 1).copied().unwrap_or(self.rows.len());
        self.turns.get(turn).map_or(end..end, |&prompt| prompt..end)
    }

    /// A turn's working, which a UI folds once the turn finishes: the rows
    /// after its prompt through its last tool call or thought. What follows,
    /// such as the answer and any notices, stays out. A turn with no work
    /// has nothing to fold.
    pub fn working(&self, turn: usize) -> Option<Range<usize>> {
        let rows = self.turn_rows(turn);
        let first = rows.start + 1;
        let last = self
            .rows
            .get(first..rows.end)?
            .iter()
            .rposition(|row| matches!(row, Row::Work(_)))?;
        Some(first..first + last + 1)
    }

    /// The row holding entry `ix`.
    pub fn row_of(&self, ix: usize) -> Option<usize> {
        let after = self.rows.partition_point(|row| row.start() <= ix);
        (ix < self.seen).then(|| after.checked_sub(1)).flatten()
    }
}

fn is_work(entry: &Entry) -> bool {
    matches!(entry, Entry::Thought(_) | Entry::Tool(_))
}

/// Whether a tool call reads as thinking rather than an action.
fn is_thinking(tool: &ToolCall) -> bool {
    tool.kind() == ToolKind::Think
}

/// A finished work row in one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// As T3 Code words it: "Ran 3 commands and read 2 files".
    pub text: String,
    /// The first kind of action named, for an icon; `None` for thoughts.
    pub lead: Option<ToolKind>,
}

/// Sums up a work row. Names the first two kinds of action in a fixed order
/// (edits, commands, reads, searches, fetches, others) and counts the rest;
/// a row of thoughts alone is "Thought (×2)".
pub fn summary(entries: &[Entry]) -> Summary {
    let mut counts = [0usize; ORDER.len()];
    let mut thoughts = 0;
    for step in entries.iter().filter_map(Entry::as_step) {
        match step {
            Step::Tool(tool) if !is_thinking(tool) => counts[slot(tool.kind())] += 1,
            Step::Tool(_) | Step::Thought(_) => thoughts += 1,
        }
    }
    let Some(lead) = counts.iter().position(|&count| count > 0) else {
        let text = match thoughts {
            0 | 1 => "Thought".to_owned(),
            n => format!("Thought (×{n})"),
        };
        return Summary { text, lead: None };
    };
    let mut parts: Vec<String> = counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .take(2)
        .map(|(ix, &count)| phrase(ix, count))
        .collect();
    let named: usize = parts.len();
    let rest: usize = counts.iter().filter(|count| **count > 0).skip(named).sum();
    if rest > 0 {
        parts.push(format!("performed {rest} other {}", plural(rest, "action")));
    }
    let mut line = match parts.as_slice() {
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b}, and {c}"),
        _ => unreachable!("at most two named kinds and the rest"),
    };
    if let Some(first) = line.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    Summary {
        text: line,
        lead: Some(ORDER[lead].0),
    }
}

/// The kinds of action in the order [`summary`] names them, each with its
/// verb and what it counts.
const ORDER: [(ToolKind, &str, &str); 6] = [
    (ToolKind::Edit, "changed", "file"),
    (ToolKind::Execute, "ran", "command"),
    (ToolKind::Read, "read", "file"),
    (ToolKind::Search, "searched code", "time"),
    (ToolKind::Fetch, "fetched", "page"),
    (ToolKind::Other, "used", "tool"),
];

/// A kind's place in [`ORDER`].
fn slot(kind: ToolKind) -> usize {
    ORDER
        .iter()
        .position(|(known, ..)| *known == kind)
        .unwrap_or(ORDER.len() - 1)
}

fn phrase(slot: usize, count: usize) -> String {
    let (_, verb, noun) = ORDER[slot];
    format!("{verb} {count} {}", plural(count, noun))
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_owned()
    } else {
        format!("{noun}s")
    }
}

/// Whether any tool call in `entries` failed.
pub fn any_failed(entries: &[Entry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, Entry::Tool(tool) if tool.status() == ToolStatus::Failed))
}

/// A thought in one line: its bold heading if it opens with one, else its
/// first sentence, without Markdown marks.
pub fn thought_line(text: &str) -> String {
    let text = text.trim_start();
    if let Some(heading) = text
        .strip_prefix("**")
        .and_then(|rest| rest.split_once("**"))
        .map(|(heading, _)| heading.trim())
        .filter(|heading| !heading.is_empty())
    {
        return heading.to_owned();
    }
    let line = text.lines().next().unwrap_or_default();
    let line = line.trim_start_matches(['#', '-', '*', '>', ' ']);
    let sentence = line
        .match_indices(['.', '?', '!'])
        .find(|(at, _)| line[at + 1..].starts_with(' '))
        .map_or(line, |(at, _)| &line[..=at]);
    sentence.replace(['*', '`'], "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Session, SessionId, SessionUpdate};

    /// A session built from daemon events: `("tool", kind)`,
    /// `("thought", text)`, `("agent", text)`, `("notice", text)` or
    /// `("user", text)`.
    fn entries(script: &[(&str, &str)]) -> Vec<Entry> {
        let mut session = Session::following(SessionId::from("s"));
        for (n, (what, text)) in script.iter().enumerate() {
            let event = match *what {
                "tool" => serde_json::json!({"type": "tool_call",
                    "id": format!("t{n}"), "title": "x", "kind": text}),
                "thought" => serde_json::json!({"type": "thought_chunk",
                    "message": format!("m{n}"), "text": text}),
                "notice" => serde_json::json!({"type": "notice", "text": text}),
                "agent" => serde_json::json!({"type": "agent_message_chunk",
                    "message": format!("m{n}"), "text": text}),
                _ => serde_json::json!({"type": "user_message",
                    "message": format!("m{n}"), "content": [{"type": "text", "text": text}]}),
            };
            session.apply(SessionUpdate(
                serde_json::from_value(event).expect("the script is a valid event"),
            ));
        }
        session.entries().to_vec()
    }

    #[test]
    fn runs_of_tools_and_thoughts_become_one_row_that_grows() {
        let mut timeline = Timeline::default();
        let mut script = vec![("user", "go"), ("thought", "hmm"), ("tool", "read")];
        let synced = timeline.sync(&entries(&script));
        assert_eq!(
            synced,
            Synced {
                grew: None,
                added: 2
            }
        );
        assert_eq!(timeline.rows(), [Row::Message(0), Row::Work(1..3)]);

        script.push(("tool", "execute"));
        let synced = timeline.sync(&entries(&script));
        assert_eq!(
            synced,
            Synced {
                grew: Some(1),
                added: 0
            },
            "the run grows"
        );

        script.extend([("agent", "done"), ("tool", "edit")]);
        timeline.sync(&entries(&script));
        assert_eq!(
            timeline.rows(),
            [
                Row::Message(0),
                Row::Work(1..4),
                Row::Message(4),
                Row::Work(5..6)
            ]
        );
        assert_eq!(timeline.row_of(3), Some(1));
        assert_eq!(timeline.row_of(5), Some(3));
        assert_eq!(timeline.row_of(6), None);
    }

    fn summary_text(entries: &[Entry]) -> String {
        summary(entries).text
    }

    #[test]
    fn turns_split_at_prompts_and_their_working_stops_at_the_reply() {
        let script = [
            ("user", "one"),
            ("thought", "a"),
            ("agent", "looking"),
            ("tool", "read"),
            ("agent", "answer"),
            ("user", "two"),
            ("agent", "just a reply"),
            ("user", "three"),
            ("tool", "execute"),
            ("user", "four"),
            ("tool", "read"),
            ("agent", "answer"),
            ("notice", "Context compacted"),
        ];
        let entries = entries(&script);
        let mut timeline = Timeline::default();
        timeline.sync(&entries);
        // Rows: prompt, work, commentary, work, answer, prompt, reply,
        // prompt, work, prompt, work, answer, notice.
        assert_eq!(timeline.turns(), [0, 5, 7, 9]);
        assert_eq!(timeline.turn_of(4), Some(0));
        assert_eq!(timeline.turn_of(7), Some(2));
        assert_eq!(timeline.turn_rows(1), 5..7);
        assert_eq!(timeline.working(0), Some(1..4), "the answer stays out");
        assert_eq!(timeline.working(1), None, "a reply alone");
        assert_eq!(
            timeline.working(2),
            Some(8..9),
            "no final reply: every row after the prompt"
        );
        assert_eq!(
            timeline.working(3),
            Some(10..11),
            "a notice after the answer leaves the answer out"
        );
        assert_eq!(timeline.working(4), None, "no such turn");
    }

    #[test]
    fn summaries_name_two_kinds_and_count_the_rest() {
        let tools = |kinds: &[&'static str]| {
            entries(&kinds.iter().map(|kind| ("tool", *kind)).collect::<Vec<_>>())
        };
        assert_eq!(summary_text(&tools(&["read"])), "Read 1 file");
        assert_eq!(
            summary_text(&tools(&["read", "execute", "execute", "read"])),
            "Ran 2 commands and read 2 files"
        );
        assert_eq!(
            summary_text(&tools(&["fetch", "search", "edit", "read", "other"])),
            "Changed 1 file, read 1 file, and performed 3 other actions"
        );
        assert_eq!(
            summary_text(&entries(&[
                ("thought", "a"),
                ("tool", "read"),
                ("thought", "b")
            ])),
            "Read 1 file",
            "thoughts aren't actions"
        );
        assert_eq!(summary_text(&entries(&[("thought", "a")])), "Thought");
        assert_eq!(
            summary(&tools(&["read", "edit"])).lead,
            Some(ToolKind::Edit)
        );
        assert_eq!(summary(&entries(&[("thought", "a")])).lead, None);
        assert_eq!(
            summary_text(&entries(&[("thought", "a"), ("tool", "think")])),
            "Thought (×2)"
        );
    }

    #[test]
    fn a_thought_line_is_its_heading_or_first_sentence() {
        assert_eq!(
            thought_line("**Finding the walker**\n\nIt is in src."),
            "Finding the walker"
        );
        assert_eq!(
            thought_line("The `walk` fn is slow. It copies."),
            "The walk fn is slow."
        );
        assert_eq!(thought_line("- v1.2 looks fine\nmore"), "v1.2 looks fine");
    }
}
