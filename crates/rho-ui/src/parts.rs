//! A long reply in parts, each an item of its own in the transcript's list.
//!
//! Kit's `TextView` builds, lays out and paints a whole Markdown text every
//! frame any of it shows. The transcript's list draws only the items near
//! the window, so a reply that is one item costs its whole length a frame,
//! and a reply in parts costs what is on screen. Scrolling through replies
//! of 33 KiB, each about 14 windows tall, a frame took 8.5 ms with each as
//! one item (17 ms for one in twenty, 28 to 38 ms at worst) and 2.7 ms with
//! each in parts (5.2 ms, and under 7 ms): 150 wheel steps of 120 px in the
//! UI tests' window, with rho-ui unoptimized and GPUI and Kit optimized.
//!
//! The parts cost time as a session opens: Kit parses a text under 4 KiB
//! at once, where it parsed a long reply in the background. Opening a
//! session with 198 KiB of such replies spent 26 ms parsing their parts. A
//! Mermaid diagram is drawn as its text is parsed, so a long reply's
//! diagrams are now drawn as the session opens too.
//!
//! Each part is a text of its own to Kit, so Select All in a reply selects
//! the part the keyboard is in, and a selection goes when the part it
//! started in scrolls out of the list's drawn range, as one always did
//! when its row did.
//!
//! [`split`] says where a reply's Markdown parts, and [`Items`] which of
//! the scroller's items each row takes.

use std::ops::Range;

/// How much Markdown a part holds, in bytes, before the next place a reply
/// can split ends it: about 20 lines of prose, half the UI tests' window.
/// Smaller parts lay out less off screen: measured as the module docs say,
/// a frame took 3.3 ms with parts of 8,192 bytes and 2.7 ms with parts of
/// 4,096, of 2,048 and of 1,024, whose replies had a 2.4 KiB code block in
/// every 3.3 KiB.
const PART_BYTES: usize = 2048;

/// `text`, a reply's Markdown, in parts that draw one under another as the
/// whole would, a paragraph gap apart. Joined, the parts are `text`.
///
/// A reply splits only between two top-level blocks that Kit sets a
/// paragraph gap apart whatever is around them, which leaves out a heading
/// or a rule on either side: Kit spaces those by their neighbors. Nothing
/// splits from an HTML block on, which may hold blank lines this doesn't
/// follow. A reply stays whole if it has a link's or a footnote's
/// definition, which any part could refer to, or a carriage return that
/// ends a line by itself, which this doesn't read as a line's end.
///
/// As a reply streams in, the parts before its last only grow. The line
/// being written can move between the last two: a `-` or `*` reads as a
/// list's next item until it turns out to be text, and starts a part then;
/// a line that turns out a rule, or the line under a heading, joins its
/// part to the one before. So a line of `-` or `=` still being written
/// counts as text; a reply that ends in one, with no line end, has it a
/// few pixels closer to the text above than it would be whole. Only a
/// definition joins every part again.
pub(crate) fn split(text: &str) -> Vec<&str> {
    // Where each part after the first starts.
    let mut starts: Vec<usize> = Vec::new();
    let mut fence: Option<Fence> = None;
    // How far the text of the last top-level block's item is indented, if
    // the block is a list. A blank line and another item continue a list,
    // and an item holds what is indented as far as its text.
    let mut list: Option<usize> = None;
    let mut after_blank = false;
    // Whether a part starts at the first line of the block being read.
    let mut starts_part = false;
    // The line of text before the one being read.
    let mut above = "";
    // Whether an HTML block has started, after which no part does.
    let mut in_html = false;
    for (at, line) in lines(text) {
        if line.contains('\r') {
            return vec![text];
        }
        let blank = line.trim_matches([' ', '\t']).is_empty();
        if let Some(open) = fence {
            // A fence in a list item or a quote ends with it, closed or not.
            let left = open.contained && !blank && !line.starts_with([' ', '\t', '>']);
            if !left {
                if open.closed_by(line) {
                    fence = None;
                }
                after_blank = blank;
                continue;
            }
        }
        if blank {
            after_blank = true;
            starts_part = false;
            continue;
        }
        if is_definition(inner(line)) {
            return vec![text];
        }
        in_html |= unindented(line).is_some_and(|rest| rest.starts_with('<'));
        let written = at + line.len() < text.len();
        let item = item_indent(line);
        if line.starts_with([' ', '\t']) {
            // An item indented less than the list's text is the list's own.
            let indent = line.len() - line.trim_start_matches(' ').len();
            if item.is_some() && list.is_none_or(|within| indent < within) {
                list = item;
            }
        } else {
            let continues_list = item.is_some() && list.is_some();
            list = item.or(list.filter(|_| !after_blank));
            let start = starts.last().copied().unwrap_or(0);
            if after_blank
                && !in_html
                && !continues_list
                && at - start >= PART_BYTES
                && is_plain(above)
                && !is_heading(line)
                && !(written && is_rule(line))
            {
                starts.push(at);
                starts_part = true;
            }
        }
        // The line under a heading: the block it ends is no paragraph.
        if written && !after_blank && starts_part && is_underline(line) {
            starts.pop();
            starts_part = false;
        }
        fence = Fence::opened_by(line, list);
        after_blank = false;
        above = line;
    }
    let ends = starts.iter().copied().chain([text.len()]);
    let parts = [0].into_iter().chain(starts.iter().copied()).zip(ends);
    parts.map(|(start, end)| &text[start..end]).collect()
}

/// `text`'s lines without their line ends, each with where it starts. A
/// carriage return left in a line ends a line by itself.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = at;
        at += line.len();
        let line = line.strip_suffix('\n').unwrap_or(line);
        (start, line.strip_suffix('\r').unwrap_or(line))
    })
}

/// `line` without the up to three spaces a top-level block may start
/// with, or `None` for a line indented further.
fn unindented(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    (line.len() - rest.len() <= 3 && !rest.starts_with('\t')).then_some(rest)
}

/// `line` without its indent and the marks of the list items and quotes it
/// is in: `- > 1. text` is `text`.
fn inner(line: &str) -> &str {
    let mut rest = line.trim_start_matches([' ', '\t']);
    while let Some(within) = rest.strip_prefix('>').or_else(|| after_marker(rest)) {
        rest = within.trim_start_matches([' ', '\t']);
    }
    rest
}

/// What follows the list marker `line` starts with: a bullet, or a number
/// and `.` or `)`, then a space or the line's end.
fn after_marker(line: &str) -> Option<&str> {
    let numbered = || {
        let rest = line.trim_start_matches(|c: char| c.is_ascii_digit());
        (rest.len() < line.len())
            .then(|| rest.strip_prefix(['.', ')']))
            .flatten()
    };
    line.strip_prefix(['-', '*', '+'])
        .or_else(numbered)
        .filter(|rest| rest.is_empty() || rest.starts_with([' ', '\t']))
}

/// How far the text of the list item `line` starts is indented: past its
/// marker and up to four spaces after it. `None` if `line` starts none.
fn item_indent(line: &str) -> Option<usize> {
    let after = after_marker(unindented(line)?)?;
    let spaces = after.len() - after.trim_start_matches(' ').len();
    Some(line.len() - after.len() + spaces.clamp(1, 4))
}

/// An open fence, which holds blank lines: a code block's backticks or
/// tildes, or a formula's dollar signs.
#[derive(Clone, Copy)]
struct Fence {
    mark: char,
    len: usize,
    /// Whether it is in a list item or a quote.
    contained: bool,
}

impl Fence {
    /// The fence `line` opens. After a list whose item's text is indented
    /// by `list`, a line indented as far is in the item.
    fn opened_by(line: &str, list: Option<usize>) -> Option<Self> {
        let rest = inner(line);
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let contained =
            indent + rest.len() < line.len() || list.is_some_and(|within| indent >= within);
        // Indented this far at the top level, it is code, not a fence.
        if !contained && unindented(line).is_none() {
            return None;
        }
        let mark = rest.chars().next().filter(|mark| "`~$".contains(*mark))?;
        let len = rest.len() - rest.trim_start_matches(mark).len();
        let least = if mark == '$' { 2 } else { 3 };
        // What follows a backtick or dollar fence can't hold its character:
        // that is code or a formula within a line.
        let within_line = mark != '~' && rest[len..].contains(mark);
        (len >= least && !within_line).then_some(Self {
            mark,
            len,
            contained,
        })
    }

    /// Whether `line` closes the fence: only its character, as many times
    /// or more.
    fn closed_by(self, line: &str) -> bool {
        let rest = if self.contained {
            Some(line.trim_start_matches([' ', '\t', '>']))
        } else {
            unindented(line)
        };
        rest.map(str::trim_end)
            .is_some_and(|rest| rest.len() >= self.len && rest.chars().all(|c| c == self.mark))
    }
}

/// Whether `line`, without what [`inner`] takes off, defines a link or a
/// footnote, which text anywhere in the reply may refer to.
fn is_definition(line: &str) -> bool {
    line.strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .is_some_and(|(label, rest)| !label.is_empty() && rest.starts_with(':'))
}

/// Whether `line` is a heading that starts with `#`.
fn is_heading(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

/// Whether `line` is a rule, or the line under a heading: only `-`, `=`,
/// `*` or `_`, and spaces.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && line.chars().all(|c| "-=*_ \t".contains(c))
}

/// Whether `line` makes the text right above it a heading: only `=`, or
/// only `-`.
fn is_underline(line: &str) -> bool {
    let Some(mark) = unindented(line).and_then(|rest| rest.chars().next()) else {
        return false;
    };
    "=-".contains(mark) && line.trim().chars().all(|c| c == mark)
}

/// Whether `line` is of a block Kit sets a paragraph gap from its
/// neighbors: not a heading, and not a rule.
fn is_plain(line: &str) -> bool {
    !is_heading(line) && !is_rule(line)
}

/// Where one of the scroller's items is in the transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) row: usize,
    /// Which of the row's items it is.
    pub(crate) part: usize,
    /// Whether it is the row's last item.
    pub(crate) ends_row: bool,
}

/// What a change to [`Items`] did, to do to the scroller's items too:
/// `count` new ones took the place of `old`.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "the scroller's items must change as these did"]
pub(crate) struct Edit {
    pub(crate) old: Range<usize>,
    pub(crate) count: usize,
}

/// Which of the scroller's items each transcript row takes: one, but a
/// long reply's row one for each of its parts.
#[derive(Debug, Default)]
pub(crate) struct Items {
    /// Per row, the item after its last.
    ends: Vec<usize>,
}

impl Items {
    /// How many items the rows take.
    pub(crate) fn len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }

    /// Adds rows at the end, each of as many items as `counts` says.
    pub(crate) fn extend(&mut self, counts: impl IntoIterator<Item = usize>) -> Edit {
        let before = self.len();
        for count in counts {
            self.ends.push(self.len() + count);
        }
        Edit {
            old: before..before,
            count: self.len() - before,
        }
    }

    /// Forgets the rows from `rows` on.
    pub(crate) fn truncate(&mut self, rows: usize) -> Edit {
        let before = self.len();
        self.ends.truncate(rows);
        Edit {
            old: self.len()..before,
            count: 0,
        }
    }

    /// Row `row` takes `count` items from now on: it gains or loses its
    /// last ones, and the rows after it move.
    pub(crate) fn resize(&mut self, row: usize, count: usize) -> Edit {
        let had = self.of(row..row + 1);
        for end in self.ends.iter_mut().skip(row) {
            *end = *end + count - had.len();
        }
        let kept = count.min(had.len());
        Edit {
            old: had.start + kept..had.end,
            count: count - kept,
        }
    }

    /// The items `rows` take. Rows there are none of take none.
    pub(crate) fn of(&self, rows: Range<usize>) -> Range<usize> {
        let before = |row: usize| match row.checked_sub(1) {
            Some(last) => self.ends.get(last).copied().unwrap_or(self.len()),
            None => 0,
        };
        before(rows.start)..before(rows.end)
    }

    /// Where item `item` is, if there is one.
    pub(crate) fn locate(&self, item: usize) -> Option<Place> {
        let row = self.ends.partition_point(|&end| end <= item);
        let end = *self.ends.get(row)?;
        Some(Place {
            row,
            part: item - self.of(row..row + 1).start,
            ends_row: item + 1 == end,
        })
    }
}

#[cfg(test)]
mod tests {
    use markdown::mdast::Node;
    use rho_ui_core::fake_daemon::report;

    use super::*;
    use crate::figures::parse_options;

    /// A paragraph of at least `bytes` bytes, so a split may follow it.
    fn paragraph(bytes: usize) -> String {
        "word ".repeat(bytes / 5 + 1)
    }

    /// `blocks`, each long enough that a split may follow it, a blank line
    /// apart.
    fn reply(blocks: &[&str]) -> String {
        let blocks = blocks
            .iter()
            .map(|block| format!("{block}{}", paragraph(PART_BYTES)));
        blocks.collect::<Vec<_>>().join("\n\n")
    }

    /// Where `text` splits: each part's first line.
    fn first_lines(text: &str) -> Vec<&str> {
        let parts = split(text);
        assert_eq!(parts.concat(), text, "the parts join to the text");
        let first_lines = parts.into_iter().map(|part| {
            let line = part.lines().next().unwrap_or_default();
            &line[..line.len().min(12)]
        });
        first_lines.collect()
    }

    /// Long blocks that hold blank lines, each one top-level block.
    fn blocks_with_blank_lines() -> Vec<String> {
        let long = paragraph(PART_BYTES);
        vec![
            // Fences, closed only by one as long of their own character.
            format!("```rust\n{long}\n\nfn main() {{}}\n~~~\n\nstill code\n```"),
            format!("~~~~\n{long}\n\n~~~\n\nstill code\n~~~~"),
            format!("$$\n{long}\n\nx = 1\n$$"),
            // A list's items, and what an item holds.
            format!("- {long}\n\n- next\n\n  more of it\n\n1. numbered"),
            format!(" 1. {long}\n\n2. an item of the same list"),
            format!("1. {long}\n\n   ```\n   code\n\n   more\n   ```"),
            // A fence a quote holds ends with the quote.
            format!("> ```\n> {long}"),
        ]
    }

    /// A long paragraph, then a heading or a rule and a line under it.
    fn headings_and_rules() -> Vec<String> {
        let long = paragraph(PART_BYTES);
        vec![
            format!("{long}\n\n## Heading\n\nUnder it."),
            format!("{long}\n\n---\n\nUnder it."),
            format!("{long}\n\nHeading\n=======\n\nUnder it."),
            format!("{long}\n\nA heading\nof two lines\n---\n\nUnder it."),
        ]
    }

    /// Replies of long paragraphs that stay whole.
    fn whole_replies() -> Vec<String> {
        let long = paragraph(PART_BYTES);
        vec![
            // A definition: any part may refer to it.
            format!("{long}\n\nSee [the docs][docs].\n\n{long}\n\n[docs]: https://example.com"),
            format!("{long}\n\nA note.[^1]\n\n{long}\n\n[^1]: The note."),
            format!("{long}\n\nSee [1].\n\n{long}\n\n- [1]: https://example.com"),
            // One after HTML too, where nothing starts a part.
            format!(
                "{long}\n\nSee [the docs][docs]. {long}\n\n<br>\n\n[docs]: https://example.com"
            ),
            // A carriage return alone ends a line: here it opens a fence.
            format!("Intro\r```\r{long}\n\nstill code\n```\n\nAfter.\n"),
        ]
    }

    /// A reply with HTML in it: nothing after the HTML starts a part.
    fn html_reply() -> String {
        let long = paragraph(PART_BYTES);
        format!("{long}\n\n{long}\n\n<details>\n\n{long}\n\n</details>\n\n{long}")
    }

    /// A reply of the blocks a model writes, each long.
    fn mixed_reply() -> String {
        reply(&[
            "One ",
            "**Bold** ",
            "Intro:\n- item\n- item\n\nAfter the list ",
            "```\ncode\n\n```\n\nAfter the code ",
            "1. first\n\n2. second\n\nLast ",
            // A `*` or a `1.` reads as a list's next item until more of
            // its line is written.
            "- item ",
            "**Bold** after a list ",
            "1. item ",
            "1.5x faster ",
        ])
    }

    #[test]
    fn a_reply_splits_between_long_blocks() {
        let text = reply(&["One ", "Two ", "- a\n- b\n\nThree "]);
        assert_eq!(first_lines(&text), ["One word wor", "Two word wor", "- a"]);
        // A short reply stays whole, and so do short blocks.
        let short = "One.\n\nTwo.\n\n```rust\nfn main() {}\n```\n";
        assert_eq!(split(short), [short]);
    }

    #[test]
    fn a_block_that_holds_blank_lines_stays_whole() {
        for block in blocks_with_blank_lines() {
            let text = format!("{block}\n\nAfter.\n");
            assert_eq!(first_lines(&text).len(), 2, "{block:?}");
            assert_eq!(split(&text)[1], "After.\n", "{block:?}");
        }
        // A fence on an item's own line closes on a line of its own, so
        // the fence after it is seen to open.
        let long = paragraph(PART_BYTES);
        let fences =
            format!("- ```sh\n  {long}\n  ```\n\n```\n{long}\n\nstill code\n```\n\nAfter.\n");
        assert_eq!(first_lines(&fences), ["- ```sh", "```", "After."]);
    }

    #[test]
    fn a_heading_or_a_rule_keeps_its_neighbors() {
        for joined in headings_and_rules() {
            let text = format!("{joined}\n\nAfter.\n");
            assert_eq!(
                split(&text),
                [format!("{joined}\n\n"), "After.\n".to_owned()]
            );
        }
    }

    #[test]
    fn a_reply_this_cannot_follow_stops_splitting() {
        for whole in whole_replies() {
            assert_eq!(split(&whole), [whole.as_str()]);
        }
        assert_eq!(first_lines(&html_reply()).len(), 2);
    }

    /// The parts start where Kit's parser starts a top-level block, with
    /// no heading or rule beside the split and no HTML block before it, and
    /// a reply the parser finds a definition in is whole.
    #[test]
    fn a_reply_splits_where_the_parser_starts_a_block() {
        let after = |blocks: Vec<String>| {
            blocks
                .into_iter()
                .map(|block| format!("{block}\n\nAfter.\n"))
        };
        let samples = [mixed_reply(), html_reply(), report()]
            .into_iter()
            .chain(after(blocks_with_blank_lines()))
            .chain(after(headings_and_rules()))
            .chain(whole_replies());
        let mut splits = 0;
        for text in samples {
            let root = markdown::to_mdast(&text, &parse_options()).expect("Markdown parses");
            let blocks = root.children().expect("a root has children");
            let start = |block: &Node| block.position().expect("a block has a place").start.offset;
            let mut nodes = vec![&root];
            let mut defines = false;
            while let Some(node) = nodes.pop() {
                defines |= matches!(node, Node::Definition(_) | Node::FootnoteDefinition(_));
                nodes.extend(node.children().into_iter().flatten());
            }
            let parts = split(&text);
            assert!(!defines || parts.len() == 1, "a definition: {text:?}");
            let mut at = 0;
            for part in &parts[..parts.len() - 1] {
                at += part.len();
                splits += 1;
                let next = blocks.partition_point(|block| start(block) < at);
                let indent = &text[at..blocks.get(next).map_or(at + 1, start)];
                assert!(
                    indent.bytes().all(|byte| byte == b' '),
                    "byte {at} starts no block: {text:?}"
                );
                assert!(
                    !matches!(
                        (&blocks[next - 1], &blocks[next]),
                        (Node::Heading(_) | Node::ThematicBreak(_), _)
                            | (_, Node::Heading(_) | Node::ThematicBreak(_))
                    ),
                    "byte {at} is beside a heading or a rule: {text:?}"
                );
                assert!(
                    !blocks[..=next]
                        .iter()
                        .any(|block| matches!(block, Node::Html(_))),
                    "byte {at} is after HTML: {text:?}"
                );
            }
        }
        assert!(splits > 20, "the samples split: {splits} times");
    }

    /// As a reply streams in, the parts before the last only grow: only
    /// the line being written moves, between the last two.
    #[test]
    fn a_streamed_reply_only_grows_the_parts_before_its_last() {
        let text = mixed_reply();
        let mut before: Vec<&str> = Vec::new();
        for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
            let parts = split(&text[..end]);
            let settled = before.len().saturating_sub(1);
            assert!(parts.len() >= settled, "the first {end} bytes");
            for (part, was) in parts.iter().zip(&before[..settled]) {
                assert!(
                    part.starts_with(was),
                    "the first {end} bytes: {was:?} became {part:?}"
                );
            }
            before = parts;
        }
        assert_eq!(before.len(), 9, "it split at each long block");
    }

    #[test]
    fn items_follow_rows_as_their_parts_change() {
        let mut items = Items::default();
        assert_eq!(
            items.extend([1, 3, 1]),
            Edit {
                old: 0..0,
                count: 5
            }
        );
        assert_eq!(items.len(), 5);
        assert_eq!(items.of(1..2), 1..4);
        assert_eq!(items.of(1..3), 1..5);
        let place = |row, part, ends_row| {
            Some(Place {
                row,
                part,
                ends_row,
            })
        };
        assert_eq!(items.locate(0), place(0, 0, true));
        assert_eq!(items.locate(2), place(1, 1, false));
        assert_eq!(items.locate(3), place(1, 2, true));
        assert_eq!(items.locate(4), place(2, 0, true));
        assert_eq!(items.locate(5), None);

        // A row loses its last items, or gains items after them.
        assert_eq!(
            items.resize(1, 1),
            Edit {
                old: 2..4,
                count: 0
            }
        );
        assert_eq!(items.of(1..2), 1..2);
        assert_eq!(items.locate(2), place(2, 0, true));
        assert_eq!(
            items.resize(2, 4),
            Edit {
                old: 3..3,
                count: 3
            }
        );
        assert_eq!(items.len(), 6);

        assert_eq!(
            items.truncate(1),
            Edit {
                old: 1..6,
                count: 0
            }
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items.of(1..3), 1..1, "rows that went take nothing");
    }
}
