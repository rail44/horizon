//! One block of a wiki page as elements, each run of its text knowing
//! where it is in the page's logical text: what a selection is read back
//! through, and what a thread's passage is marked by.
//!
//! The block is parsed as `horizon-wiki` parses the page for its logical
//! text (the `markdown` crate's syntax tree), so a run of text here is a
//! text node there.

use std::ops::Range;
use std::rc::Rc;

use horizon_ui::{El, el};

use super::styles;
use horizon_wiki::view_model::WikiPage;
use markdown::ParseOptions;
use markdown::mdast::Node;

/// One run of text as drawn.
pub(super) struct Piece {
    /// The element whose lines of text the run is in.
    pub(super) root: El,
    pub(super) value: String,
    /// The logical characters the run is, when it is all of them: a run
    /// whose text does not read as the logical text does (an escape, an
    /// entity) is drawn, and neither selected from nor marked.
    pub(super) logical: Option<Range<usize>>,
}

/// A passage to mark: its logical characters, and whether its thread is
/// the one in focus.
pub(super) type Mark = (Range<usize>, bool);

struct Drawing<'a> {
    page: &'a WikiPage,
    /// Where the block starts in the body.
    at: usize,
    marks: &'a [Mark],
    on_link: Rc<dyn Fn(&str)>,
    pieces: Vec<Piece>,
}

impl Drawing<'_> {
    /// Appends `value`, the text of the node at `source` (bytes of the
    /// block), to `into`, within the lines of `root`.
    fn text(&mut self, value: &str, source: Option<Range<usize>>, into: El, root: El) {
        // (A line break inside a paragraph reads as a space, and is one
        // character either way.)
        let value = value.replace('\n', " ");
        let count = value.chars().count();
        let logical = source
            .and_then(|source| {
                (self.page).logical_range(self.at + source.start..self.at + source.end)
            })
            .filter(|logical| logical.len() == count);
        match &logical {
            Some(logical) => {
                // The run in parts: unmarked, or marked for a passage.
                let mut cuts: Vec<usize> = vec![0, count];
                for (mark, _) in self.marks {
                    for edge in [mark.start, mark.end] {
                        if edge > logical.start && edge < logical.end {
                            cuts.push(edge - logical.start);
                        }
                    }
                }
                cuts.sort_unstable();
                cuts.dedup();
                let byte = |chars: usize| {
                    (value.char_indices().nth(chars)).map_or(value.len(), |(byte, _)| byte)
                };
                for part in cuts.windows(2) {
                    let text = &value[byte(part[0])..byte(part[1])];
                    let within = logical.start + part[0];
                    let marked = (self.marks.iter())
                        .filter(|(mark, _)| mark.start <= within && within < mark.end)
                        .map(|(_, focused)| *focused)
                        .max();
                    match marked {
                        Some(true) => {
                            into.child(el("span").class((styles::MARK, styles::FOCUSED)).text(text))
                        }
                        Some(false) => into.child(el("span").class(styles::MARK).text(text)),
                        None => into.text(text),
                    };
                }
            }
            None => {
                into.text(&value);
            }
        }
        self.pieces.push(Piece {
            root,
            value,
            logical,
        });
    }

    /// The inline content of `node`'s children, into `into`.
    fn inlines(&mut self, node: &Node, into: El, root: El) {
        for child in node.children().into_iter().flatten() {
            let source =
                (child.position()).map(|position| position.start.offset..position.end.offset);
            match child {
                Node::Text(text) => self.text(&text.value, source, into, root),
                Node::InlineCode(code) => {
                    let element = el("code");
                    into.child(element);
                    self.text(&code.value, source, element, root);
                }
                Node::Emphasis(_) | Node::Strong(_) | Node::Delete(_) => {
                    let element = el(match child {
                        Node::Emphasis(_) => "em",
                        Node::Strong(_) => "strong",
                        _ => "s",
                    });
                    into.child(element);
                    self.inlines(child, element, root);
                }
                Node::Link(link) => {
                    let (target, on_link) = (link.url.clone(), self.on_link.clone());
                    let element = el("span")
                        .class(styles::LINK)
                        .on("click", move |_| on_link(&target));
                    into.child(element);
                    self.inlines(child, element, root);
                }
                Node::Image(image) => self.text(&image.alt, None, into, root),
                Node::Break(_) => {
                    into.child(el("br"));
                }
                Node::Html(html) => self.text(&html.value, None, into, root),
                other => self.inlines(other, into, root),
            }
        }
    }

    /// The block content of `node`'s children, into `into`.
    fn blocks(&mut self, node: &Node, into: El) {
        for child in node.children().into_iter().flatten() {
            let source =
                (child.position()).map(|position| position.start.offset..position.end.offset);
            match child {
                Node::Paragraph(_) => {
                    let element = el("p");
                    into.child(element);
                    self.inlines(child, element, element);
                }
                Node::Heading(heading) => {
                    let element = el(match heading.depth {
                        1 => "h1",
                        2 => "h2",
                        _ => "h3",
                    });
                    into.child(element);
                    self.inlines(child, element, element);
                }
                Node::Code(code) => {
                    let element = el("pre");
                    into.child(element);
                    self.text(&code.value, source, element, element);
                }
                Node::Blockquote(_) => {
                    let element = el("blockquote");
                    into.child(element);
                    self.blocks(child, element);
                }
                Node::List(list) => {
                    let element = el(if list.ordered { "ol" } else { "ul" });
                    if let Some(start) = list.start {
                        element.attr("start", &start.to_string());
                    }
                    into.child(element);
                    self.blocks(child, element);
                }
                Node::ListItem(_) => {
                    let element = el("li");
                    into.child(element);
                    self.blocks(child, element);
                }
                Node::Table(_) => {
                    let element = el("table");
                    into.child(element);
                    self.blocks(child, element);
                }
                Node::TableRow(_) => {
                    let element = el("tr");
                    into.child(element);
                    self.blocks(child, element);
                }
                Node::TableCell(_) => {
                    let element = el("td");
                    into.child(element);
                    self.inlines(child, element, element);
                }
                Node::ThematicBreak(_) => {
                    into.child(el("hr"));
                }
                Node::Html(html) => {
                    let element = el("p");
                    into.child(element);
                    self.text(&html.value, None, element, element);
                }
                other => self.blocks(other, into),
            }
        }
    }
}

/// Draws the block at `range` of `page`'s body, the passages in `marks`
/// marked, a link calling `on_link` with where it points.
pub(super) fn block(
    page: &WikiPage,
    range: Range<usize>,
    marks: &[Mark],
    on_link: Rc<dyn Fn(&str)>,
) -> (El, Vec<Piece>) {
    let element = el("div").class(styles::BLOCK);
    let source = &page.document.body[range.clone()];
    let mut drawing = Drawing {
        page,
        at: range.start,
        marks,
        on_link,
        pieces: Vec::new(),
    };
    match markdown::to_mdast(source, &ParseOptions::gfm()) {
        Ok(tree) => drawing.blocks(&tree, element),
        // (GFM without MDX has no syntax errors; the text as it is, then.)
        Err(_) => {
            element.text(source);
        }
    }
    (element, drawing.pieces)
}
