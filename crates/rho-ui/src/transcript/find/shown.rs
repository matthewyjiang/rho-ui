//! The text Kit's text view shows for a Markdown source, without making a
//! view: what find searches in a thought or a tool's Markdown body, whose
//! details may never open.
//!
//! Kit exposes the parser its view uses ([`parse_markdown`]), but not how
//! the view turns the blocks into the text its range highlights index
//! (`BlockNode::text` in gpui-base 0.9.1's `text/node.rs`, private). So
//! [`shown_text`] follows it, block by block. The tests check that it
//! gives a view's `rendered_text()`, and find asserts the same, in debug
//! builds, of each view it marks.

use std::sync::Arc;

use gpui_kit::base::text::editor_host::{parse_markdown, BlockNode, InlineNode, Paragraph};

/// The text a [`TextViewState`](gpui_kit::component::text::TextViewState)
/// renders for `markdown` with no extensions, as its `rendered_text()`
/// gives it: emphasis and link addresses go, escapes resolve, and each
/// block ends with a newline. Nothing for Markdown Kit fails to parse,
/// whose view shows an error instead.
pub(super) fn shown_text(markdown: &str) -> String {
    let mut text = String::new();
    for block in parse_markdown(markdown, Arc::default()).unwrap_or_default() {
        push_block(&mut text, &block);
    }
    text
}

/// Appends `block`'s text, as Kit's `BlockNode::text` does.
fn push_block(text: &mut String, block: &BlockNode) {
    // A block that shows text ends its own line.
    let line = |text: &mut String, block_text: &str| {
        if !block_text.is_empty() {
            text.push_str(block_text);
            text.push('\n');
        }
    };
    let children_text = |children: &[BlockNode]| {
        let mut inner = String::new();
        for child in children {
            push_block(&mut inner, child);
        }
        inner
    };
    match block {
        BlockNode::Root { children, .. } | BlockNode::Blockquote { children, .. } => {
            line(text, &children_text(children));
        }
        BlockNode::List { children, .. }
        | BlockNode::ListItem { children, .. }
        | BlockNode::Surface { children, .. } => text.push_str(&children_text(children)),
        BlockNode::Paragraph(paragraph)
        | BlockNode::Heading {
            children: paragraph,
            ..
        } => line(text, &paragraph_text(paragraph)),
        BlockNode::Table(table) => {
            let mut rows = String::new();
            for row in table.rows() {
                let cells: Vec<String> = row.map(paragraph_text).collect();
                if !cells.is_empty() {
                    rows.push_str(&cells.join(" "));
                    rows.push('\n');
                }
            }
            line(text, &rows);
        }
        BlockNode::CodeBlock(code) => line(text, &code.code()),
        BlockNode::Custom { node, .. } => line(text, node.as_text()),
        // Definitions, breaks and rules show no text. Nor, until handled
        // here, does a block a later Kit adds: find's debug assertion and
        // the tests below catch it once it shows any.
        _ => {}
    }
}

fn paragraph_text(paragraph: &Paragraph) -> String {
    paragraph.children().iter().map(InlineNode::text).collect()
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::text::TextViewState;
    use gpui_kit::{AppContext as _, TestAppContext};

    use super::shown_text;

    /// Each kind of block and inline Kit parses, as agents write them.
    const SOURCES: &[&str] = &[
        "The **tree** walk is in [src/0.rs](https://example.com/walk/0).",
        "# Title\n\nhello **world** \\* &amp; `code` ~~gone~~ *it* ==mark==",
        "- one\n- two\n  - nested\n\n1. first\n2. second\n\n- [ ] todo\n- [x] done",
        "> quoted **text**\n>\n> - in a list",
        "| a | b |\n|---|---|\n| c | d |\n| | e |",
        "```rust\nlet x = 1;\n```\n\n    indented code",
        "line one  \nline two\n\n---\n\n[ref]: https://example.com\n\n[a ref][ref]",
        "inline $x^2$ math and\n\n$$\ny = mx\n$$",
        "<b>html</b> and <span>inline</span>\n\n<div>block</div>",
        "![an image](https://example.com/a.png) after it, a footnote[^1].\n\n[^1]: The note.",
        "",
    ];

    #[test]
    fn shown_text_is_what_a_view_renders() {
        let cx = TestAppContext::single();
        cx.update(gpui_kit::init);
        for source in SOURCES {
            let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(source, cx)));
            cx.run_until_parked();
            let rendered = state.read_with(&cx, |state, _| state.rendered_text());
            assert_eq!(shown_text(source), rendered.as_str(), "for {source:?}");
        }
    }
}
