//! Markdown as elements of the document, drawn the same wherever it is
//! placed: an agent's message, a post, a Markdown file.

use std::rc::Rc;

use horizon_ui::{El, el};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

mod styles {
    horizon_ui::css_module! {"
        .body {
            > :first-child { margin-top: 0; }
            > :last-child { margin-bottom: 0; }
            h1 { font-size: var(--t2-size); line-height: var(--t2-line); font-weight: 600;
                 margin: var(--u8) 0; }
            h2, h3 { font-size: var(--body-size); line-height: var(--body-line);
                     font-weight: 600; margin: var(--u8) 0; }
            p, ul, ol, pre, blockquote, table { margin: 0 0 var(--u8) 0; }
            ul, ol { padding-left: calc(var(--u4) * 5); }
            li { margin-bottom: var(--u4); }
            pre { padding: var(--u16); border: var(--line) solid var(--border);
                  border-radius: var(--radius-large); line-height: 1.5;
                  background: var(--surface-panel); overflow-x: scroll; }
            code { font-family: var(--font-family); }
            :not(pre) > code { padding: 0 var(--u4); border-radius: var(--radius-small);
                               font-size: var(--meta-size); background: var(--surface-panel); }
            blockquote { padding-left: var(--u12);
                         border-left: var(--line-strong) solid var(--border);
                         color: var(--text-muted); }
            td, th { padding: var(--u2) var(--u8); border: var(--line) solid var(--border); }
        }
        .body.plain-headings h1 { font-size: var(--body-size); line-height: var(--body-line); }
        .language { margin: -11px 0 2px 0; text-align: right; font-size: var(--meta-size);
                    color: var(--text-muted); }
        .link { color: var(--accent); cursor: pointer; }
        .link:hover { text-decoration: underline; }
    "}
}

/// How big a heading is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Headings {
    /// The first level above the text's size: a page, a message.
    Scaled,
    /// Every level at the text's size, in bold: a post among posts.
    AsText,
}

/// Markdown as elements, a link calling `on_link` with where it points.
pub fn render(source: &str, headings: Headings, on_link: Rc<dyn Fn(&str)>) -> El {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // The elements open at this point of the text, innermost last.
    let body = el("div").class(styles::BODY);
    if headings == Headings::AsText {
        body.class(styles::PLAIN_HEADINGS);
    }
    let mut open = vec![body];
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
                            pre.child(el("div").class(styles::LANGUAGE).text(&language));
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
                            .class(styles::LINK)
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
