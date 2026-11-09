//! A page's logical text: the text a reader sees, without Markdown syntax,
//! which comment anchors quote and count positions in (Unicode code points,
//! as Web Annotation's text selectors require). Defined here once, so the
//! shell's view and agents agree on it.
//!
//! It is built from the `markdown` crate's syntax tree of the page body (the
//! parser gpui-component's `TextView` uses): the values of its text nodes in
//! document order, with one `\n` between blocks and for a hard break. Every
//! logical character keeps the source byte range it came from, so a view
//! converts through source positions — a selection's source range to a
//! logical range, a stored logical range back to a source range — and its
//! own rendered string need not match this one.
//!
//! Parsed as GFM without the math constructs. The view parses math too but
//! turns an inline `$…$` no plugin claims back into prose, so only a `$$`
//! block can read differently here.

use std::ops::Range;

use markdown::mdast::Node;
use markdown::ParseOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalText {
    text: String,
    /// Per logical character, the source byte range it came from; empty for
    /// a separator the parser does not spell out.
    sources: Vec<Range<usize>>,
}

/// One unit a view draws on its own and places comment threads against:
/// a top-level block (a paragraph, a heading, a table, a code block…), or
/// one item of a top-level list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// The block's bytes in the body.
    pub range: Range<usize>,
    /// For a list item, which top-level list it belongs to (counted in
    /// order), so a view can keep a list's items together.
    pub list: Option<usize>,
}

/// `body` split into [`Block`]s, in order, parsed as
/// [`LogicalText::of_markdown`] parses. A top-level list is split into its
/// items, each with its nested content; an item's own text keeps its
/// marker, and an ordered item its number.
pub fn page_blocks(body: &str) -> Vec<Block> {
    let Ok(root) = markdown::to_mdast(body, &ParseOptions::gfm()) else {
        return Vec::new();
    };
    let span = |node: &Node| {
        node.position()
            .map(|position| position.start.offset..position.end.offset)
    };
    let mut blocks = Vec::new();
    let mut lists = 0;
    for node in root.children().into_iter().flatten() {
        match node {
            Node::List(list) => {
                for item in &list.children {
                    if let Some(range) = span(item) {
                        blocks.push(Block {
                            range,
                            list: Some(lists),
                        });
                    }
                }
                lists += 1;
            }
            other => {
                if let Some(range) = span(other) {
                    blocks.push(Block { range, list: None });
                }
            }
        }
    }
    blocks
}

impl LogicalText {
    /// The logical text of a page body (the Markdown after its frontmatter).
    pub fn of_markdown(body: &str) -> Self {
        let mut builder = Builder {
            source: body,
            text: String::new(),
            sources: Vec::new(),
        };
        // GFM never fails to parse; only MDX can.
        if let Ok(root) = markdown::to_mdast(body, &ParseOptions::gfm()) {
            builder.walk(&root);
        }
        while builder.text.ends_with('\n') {
            builder.text.pop();
            builder.sources.pop();
        }
        Self {
            text: builder.text,
            sources: builder.sources,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Length in characters (code points).
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// The characters `chars` (code point indices) as a string.
    pub fn slice(&self, chars: Range<usize>) -> String {
        self.text
            .chars()
            .skip(chars.start)
            .take(chars.end.saturating_sub(chars.start))
            .collect()
    }

    /// The source bytes the logical characters `chars` came from: from the
    /// first one's start to the last one's end. `None` when the range holds
    /// only separators or is out of bounds.
    pub fn source_range(&self, chars: Range<usize>) -> Option<Range<usize>> {
        let spans: Vec<&Range<usize>> = self
            .sources
            .get(chars)?
            .iter()
            .filter(|span| !span.is_empty())
            .collect();
        Some(spans.first()?.start..spans.last()?.end)
    }

    /// The logical characters whose source lies wholly inside `source`
    /// (bytes), from the first to the last; `None` when there are none.
    pub fn logical_range(&self, source: Range<usize>) -> Option<Range<usize>> {
        let inside = |span: &Range<usize>| {
            !span.is_empty() && span.start >= source.start && span.end <= source.end
        };
        let first = self.sources.iter().position(inside)?;
        let last = self.sources.iter().rposition(inside)?;
        Some(first..last + 1)
    }
}

struct Builder<'a> {
    source: &'a str,
    text: String,
    sources: Vec<Range<usize>>,
}

impl Builder<'_> {
    fn walk(&mut self, node: &Node) {
        let span = node
            .position()
            .map(|position| position.start.offset..position.end.offset);
        match node {
            Node::Text(text) => self.push_value(&text.value, span),
            Node::InlineCode(code) => self.push_value(&code.value, span),
            Node::Code(code) => {
                self.block_break();
                self.push_value(&code.value, span);
                self.block_break();
            }
            Node::Break(_) => self.push_separator(),
            // Not text a reader sees.
            Node::Html(_) | Node::Image(_) | Node::ImageReference(_) | Node::Definition(_) => {}
            _ => {
                let block = is_block(node);
                if block {
                    self.block_break();
                }
                for child in node.children().into_iter().flatten() {
                    self.walk(child);
                }
                if block {
                    self.block_break();
                }
            }
        }
    }

    /// Appends `value`, aligning each character with the next equal
    /// character of the node's source span, so escapes, entities and code
    /// fences in the source are skipped over. A character with no match
    /// (a decoded entity) takes an empty range at the cursor.
    fn push_value(&mut self, value: &str, span: Option<Range<usize>>) {
        let span = span.unwrap_or(0..0);
        let source = &self.source[span.clone()];
        let mut cursor = 0;
        for ch in value.chars() {
            match source[cursor..].find(ch) {
                Some(found) => {
                    let start = span.start + cursor + found;
                    let end = start + ch.len_utf8();
                    self.sources.push(start..end);
                    cursor += found + ch.len_utf8();
                }
                None => {
                    let at = span.start + cursor;
                    self.sources.push(at..at);
                }
            }
            self.text.push(ch);
        }
    }

    fn push_separator(&mut self) {
        let at = self.sources.last().map_or(0, |span| span.end);
        self.text.push('\n');
        self.sources.push(at..at);
    }

    /// One `\n` between blocks, never two, and none at the start.
    fn block_break(&mut self) {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.push_separator();
        }
    }
}

fn is_block(node: &Node) -> bool {
    matches!(
        node,
        Node::Paragraph(_)
            | Node::Heading(_)
            | Node::Blockquote(_)
            | Node::List(_)
            | Node::ListItem(_)
            | Node::Table(_)
            | Node::TableRow(_)
            | Node::TableCell(_)
            | Node::ThematicBreak(_)
            | Node::FootnoteDefinition(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_left_out_and_blocks_are_separated_once() {
        let logical =
            LogicalText::of_markdown("# Sandbox\n\nThe **helper** runs `nono`.\n\n- one\n- two\n");
        assert_eq!(logical.as_str(), "Sandbox\nThe helper runs nono.\none\ntwo");
    }

    #[test]
    fn every_character_maps_back_to_its_source() {
        let body = "Use **bold** and `code`.";
        let logical = LogicalText::of_markdown(body);
        assert_eq!(logical.as_str(), "Use bold and code.");
        // "bold": logical 4..8 sits inside the `**` in the source.
        let source = logical.source_range(4..8).unwrap();
        assert_eq!(&body[source.clone()], "bold");
        assert_eq!(logical.logical_range(source), Some(4..8));
        // A selection spanning the delimiters maps to the same characters.
        assert_eq!(logical.logical_range(4..12), Some(4..8));
    }

    #[test]
    fn positions_are_code_points_and_escapes_are_skipped() {
        let body = "日本語の\\*説明\\*です";
        let logical = LogicalText::of_markdown(body);
        assert_eq!(logical.as_str(), "日本語の*説明*です");
        assert_eq!(logical.len(), 10);
        assert_eq!(logical.slice(4..8), "*説明*");
        let source = logical.source_range(5..7).unwrap();
        assert_eq!(&body[source], "説明");
    }

    #[test]
    fn code_blocks_keep_their_content_without_the_fence() {
        let body = "Run:\n\n```sh\ncargo test\n```\n\nDone.";
        let logical = LogicalText::of_markdown(body);
        assert_eq!(logical.as_str(), "Run:\ncargo test\nDone.");
        let start = logical.as_str().find("cargo").unwrap();
        let source = logical.source_range(start..start + 10).unwrap();
        assert_eq!(&body[source], "cargo test");
    }

    #[test]
    fn a_page_splits_into_blocks_and_lists_into_their_items() {
        let body =
            "# Title\n\nOne paragraph.\n\n- a\n- b\n  - nested\n\n3. c\n4. d\n\n```sh\nx\n```\n";
        let blocks: Vec<(&str, Option<usize>)> = page_blocks(body)
            .into_iter()
            .map(|block| (body[block.range].trim_end(), block.list))
            .collect();
        assert_eq!(
            blocks,
            [
                ("# Title", None),
                ("One paragraph.", None),
                ("- a", Some(0)),
                ("- b\n  - nested", Some(0)),
                ("3. c", Some(1)),
                ("4. d", Some(1)),
                ("```sh\nx\n```", None),
            ]
        );
    }

    #[test]
    fn separators_alone_have_no_source() {
        let logical = LogicalText::of_markdown("a\n\nb");
        assert_eq!(logical.as_str(), "a\nb");
        assert_eq!(logical.source_range(1..2), None);
        assert_eq!(logical.source_range(0..3), Some(0..4));
    }
}
