//! Markdown as elements of the document: what an agent's message, and a
//! Markdown file, are drawn from. The view that places it gives it its
//! rules.

use std::rc::Rc;

use horizon_ui::{El, el};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Markdown as elements, a link calling `on_link` with where it points.
pub fn render(source: &str, on_link: Rc<dyn Fn(&str)>) -> El {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // The elements open at this point of the text, innermost last.
    let mut open = vec![el("div").class("body")];
    for event in Parser::new_ext(source, options) {
        let top = *open.last().expect("the body");
        match event {
            Event::Start(tag) => {
                let element = match tag {
                    Tag::Paragraph => el("p"),
                    Tag::Heading { level, .. } => el(match level {
                        HeadingLevel::H1 => "h1",
                        HeadingLevel::H2 => "h2",
                        _ => "h3",
                    }),
                    Tag::BlockQuote(_) => el("blockquote"),
                    Tag::CodeBlock(kind) => {
                        let (code, pre) = (el("code"), el("pre"));
                        // A fenced block names its language above its code,
                        // as gpui-component's Markdown does.
                        if let CodeBlockKind::Fenced(language) = kind
                            && !language.is_empty()
                        {
                            pre.child(el("div").class("language").text(&language));
                        }
                        pre.child(code);
                        top.child(pre);
                        // Text goes into the `code`; the `pre` is its box.
                        open.push(code);
                        continue;
                    }
                    Tag::List(Some(_)) => el("ol"),
                    Tag::List(None) => el("ul"),
                    Tag::Item => el("li"),
                    Tag::Emphasis => el("em"),
                    Tag::Strong => el("strong"),
                    Tag::Strikethrough => el("s"),
                    Tag::Link { dest_url, .. } => {
                        let (target, on_link) = (dest_url.to_string(), on_link.clone());
                        el("span")
                            .class("link")
                            .on("click", move |_| on_link(&target))
                    }
                    Tag::Table(_) => el("table"),
                    Tag::TableHead | Tag::TableRow => el("tr"),
                    Tag::TableCell => el("td"),
                    _ => el("span"),
                };
                top.child(element);
                open.push(element);
            }
            Event::End(TagEnd::Paragraph | TagEnd::Item) if open.len() == 1 => {}
            Event::End(_) => {
                if open.len() > 1 {
                    open.pop();
                }
            }
            Event::Text(text) => {
                top.text(&text);
            }
            Event::Code(code) => {
                top.child(el("code").text(&code));
            }
            Event::SoftBreak => {
                top.text(" ");
            }
            Event::HardBreak => {
                top.child(el("br"));
            }
            Event::Rule => {
                top.child(el("hr"));
            }
            Event::TaskListMarker(done) => {
                top.text(if done { "[x] " } else { "[ ] " });
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                top.text(&html);
            }
            _ => {}
        }
    }
    open[0]
}
