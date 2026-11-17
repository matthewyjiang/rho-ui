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
//! session with 198 KiB of such replies spent 26 ms parsing their parts.
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
/// follow, and a reply with a link's or a footnote's definition, which any
/// part could refer to, stays whole.
///
/// As a reply streams in, its parts only grow and gain parts after them,
/// but for a line that turns out not to start a part once more of it is
/// written: a rule, the line under a heading, a list's next item, or a
/// definition. Its part then joins the one before. So a line of `-` or `=`
/// still being written counts as text; a reply that ends in one, with no
/// line end, has it a few pixels closer to the text above than it would
/// be whole.
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
    for (at, line) in lines(text) {
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
        if unindented(line).is_some_and(|rest| rest.starts_with('<')) {
            break;
        }
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

/// `text`'s lines without their line ends, each with where it starts.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    text.split_inclusive('\n').map(move |line| {
        let start = at;
        at += line.len();
        (start, line.trim_end_matches(['\n', '\r']))
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

    /// Adds a row of `count` items at the end.
    pub(crate) fn push(&mut self, count: usize) {
        self.ends.push(self.len() + count);
    }

    /// Forgets the rows from `rows` on.
    pub(crate) fn truncate(&mut self, rows: usize) {
        self.ends.truncate(rows);
    }

    /// Row `row` takes `count` items from now on. The rows after it move.
    pub(crate) fn resize(&mut self, row: usize, count: usize) {
        let had = self.of(row..row + 1).len();
        for end in self.ends.iter_mut().skip(row) {
            *end = *end + count - had;
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

    /// The row item `item` is of, and which of the row's items it is.
    pub(crate) fn locate(&self, item: usize) -> Option<(usize, usize)> {
        let row = self.ends.partition_point(|&end| end <= item);
        (row < self.ends.len()).then(|| (row, item - self.of(row..row + 1).start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let long = paragraph(PART_BYTES);
        for block in [
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
        ] {
            let text = format!("{block}\n\nAfter.\n");
            assert_eq!(first_lines(&text).len(), 2, "{block:?}");
            assert_eq!(split(&text)[1], "After.\n", "{block:?}");
        }
        // A fence on an item's own line closes on a line of its own, so
        // the fence after it is seen to open.
        let fences =
            format!("- ```sh\n  {long}\n  ```\n\n```\n{long}\n\nstill code\n```\n\nAfter.\n");
        assert_eq!(first_lines(&fences), ["- ```sh", "```", "After."]);
    }

    #[test]
    fn a_heading_or_a_rule_keeps_its_neighbors() {
        let long = paragraph(PART_BYTES);
        for joined in [
            format!("{long}\n\n## Heading\n\nUnder it."),
            format!("{long}\n\n---\n\nUnder it."),
            format!("{long}\n\nHeading\n=======\n\nUnder it."),
            format!("{long}\n\nA heading\nof two lines\n---\n\nUnder it."),
        ] {
            let text = format!("{joined}\n\nAfter.\n");
            assert_eq!(
                split(&text),
                [format!("{joined}\n\n"), "After.\n".to_owned()]
            );
        }
    }

    #[test]
    fn a_reply_this_cannot_follow_stops_splitting() {
        let long = paragraph(PART_BYTES);
        // A definition: any part may refer to it.
        for defined in [
            format!("{long}\n\nSee [the docs][docs].\n\n{long}\n\n[docs]: https://example.com"),
            format!("{long}\n\nA note.[^1]\n\n{long}\n\n[^1]: The note."),
            format!("{long}\n\nSee [1].\n\n{long}\n\n- [1]: https://example.com"),
        ] {
            assert_eq!(split(&defined), [defined.as_str()]);
        }
        // HTML: nothing after it starts a part.
        let html = format!("{long}\n\n{long}\n\n<details>\n\n{long}\n\n</details>\n\n{long}");
        assert_eq!(first_lines(&html).len(), 2);
    }

    /// As a reply streams in, a part only grows, and parts come after it.
    #[test]
    fn a_streamed_reply_only_grows_its_parts() {
        let text = reply(&[
            "One ",
            "**Bold** ",
            "Intro:\n- item\n- item\n\nAfter the list ",
            "```\ncode\n\n```\n\nAfter the code ",
            "1. first\n\n2. second\n\nLast ",
        ]);
        let mut before: Vec<&str> = Vec::new();
        for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
            let parts = split(&text[..end]);
            assert!(parts.len() >= before.len(), "the first {end} bytes");
            for (part, was) in parts.iter().zip(&before) {
                assert!(
                    part.starts_with(was),
                    "the first {end} bytes: {was:?} became {part:?}"
                );
            }
            before = parts;
        }
        assert_eq!(before.len(), 5, "it split at each long block");
    }

    #[test]
    fn items_follow_rows_as_their_parts_change() {
        let mut items = Items::default();
        for count in [1, 3, 1] {
            items.push(count);
        }
        assert_eq!(items.len(), 5);
        assert_eq!(items.of(1..2), 1..4);
        assert_eq!(items.of(1..3), 1..5);
        assert_eq!(items.locate(0), Some((0, 0)));
        assert_eq!(items.locate(3), Some((1, 2)));
        assert_eq!(items.locate(4), Some((2, 0)));
        assert_eq!(items.locate(5), None);

        items.resize(1, 1);
        assert_eq!(items.of(1..2), 1..2);
        assert_eq!(items.locate(2), Some((2, 0)));
        items.resize(2, 4);
        assert_eq!(items.len(), 6);

        items.truncate(1);
        assert_eq!(items.len(), 1);
        assert_eq!(items.of(1..3), 1..1, "rows that went take nothing");
    }
}
