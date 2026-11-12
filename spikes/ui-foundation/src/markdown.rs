//! Horizon's Markdown view on this stack: one file read from disk, drawn as
//! running text, with links to other Markdown files followed in place.
//!
//! What the view holds is a handful of signals (the file, the way back, the
//! parts of the document, a notice). The elements are built once; each
//! reads the signals it shows. Reading the front matter and resolving
//! links is Horizon's own code, used as it is
//! (`src/markdown/document.rs`).

use std::path::PathBuf;
use std::rc::Rc;

use blitz_traits::events::DomEventData;
use keyboard_types::{Key, NamedKey};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, GetUntracked, Set, Update};

use crate::{El, dispatched, el};

#[allow(dead_code)]
#[path = "../../../src/markdown/document.rs"]
mod document;

use document::LinkTarget;

const BACK: &str = "Back";
const NO_DOCUMENT: &str = "No document";
const UNSUPPORTED_LINK: &str = "Not a Markdown file";

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it.
pub const CSS: &str = "
    .markdown { display: flex; flex-direction: column; height: 100%; min-width: 240px;
                outline: none; font-family: sans-serif; font-size: 14px;
                line-height: 26px; color: #202020; background: #fafafa;
        .header { flex: none; display: flex; flex-direction: column; gap: 8px;
                  padding: 24px 16px 0 16px; }
        .header:empty { display: none; }
        .body > :first-child { margin-top: 0; }
        .back { align-self: flex-start; padding: 0 8px; border-radius: 4px; font-size: 12px;
                line-height: 20px; color: #606060; cursor: pointer; }
        .back:hover { background: #ececec; }
        .title { font-size: 16px; line-height: 24px; font-weight: 600; }
        .chips { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; }
        .chip { padding: 0 4px; border-radius: 3px; background: #ececec; color: #606060;
                font-size: 11px; line-height: 16px; }
        .notice, .empty, .error { flex: none; padding: 8px 16px 0 16px; font-size: 12px;
                                  line-height: 18px; color: #808080; }
        .error { color: #c0392b; }
        .scroll { flex: 1; min-height: 0; overflow-y: scroll; padding: 24px 16px 32px 16px; }
        .scroll h1, .scroll h2, .scroll h3 { font-size: 14px; line-height: 22px;
                                             font-weight: 600; margin: 24px 0 8px 0; }
        .scroll h1 { font-size: 16px; line-height: 24px; }
        .scroll p, .scroll ul, .scroll ol, .scroll pre, .scroll blockquote, .scroll table {
            margin: 0 0 8px 0; }
        .scroll pre { padding: 8px 12px; border-radius: 4px; background: #f0f0f0;
                      font-size: 13px; line-height: 20px; overflow-x: scroll; }
        .scroll code { font-family: monospace; font-size: 13px; }
        .scroll blockquote { padding-left: 12px; border-left: 2px solid #d0d0d0; color: #606060; }
        .scroll td, .scroll th { padding: 2px 8px; border: 1px solid #d0d0d0; }
        .link { color: #356df3; cursor: pointer; }
        .link:hover { text-decoration: underline; }
    }
";

/// What is outside the view: a URL handed to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;

/// The view's state, for whoever placed it to read or drive.
#[derive(Clone, Copy)]
pub struct Markdown {
    pub path: RwSignal<Option<PathBuf>>,
    /// The files left by following links, most recent last.
    pub history: RwSignal<Vec<PathBuf>>,
    title: RwSignal<Option<String>>,
    fields: RwSignal<Vec<(String, String)>>,
    body: RwSignal<String>,
    error: RwSignal<Option<String>>,
    /// A line under the header: a link the view did not follow.
    pub notice: RwSignal<Option<String>>,
}

impl Markdown {
    /// Points the view at `path` from outside: the history is left as it
    /// is, since the jump was not a link followed here.
    pub fn open(&self, path: PathBuf) {
        if self.path.get_untracked().as_ref() == Some(&path) {
            self.reload();
        } else {
            self.show(path);
        }
    }

    fn show(&self, path: PathBuf) {
        self.notice.set(None);
        self.path.set(Some(path));
        self.reload();
    }

    /// Reads the file again.
    pub fn reload(&self) {
        let Some(path) = self.path.get_untracked() else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                let document = document::parse(&source);
                self.error.set(None);
                self.title.set(document.title);
                self.fields.set(document.fields);
                self.body.set(document.body);
            }
            Err(error) => {
                self.error.set(Some(format!("{}: {error}", path.display())));
                self.title.set(None);
                self.fields.set(Vec::new());
                self.body.set(String::new());
            }
        }
    }

    fn follow_link(&self, target: &str, open_url: &OpenUrl) {
        let Some(current) = self.path.get_untracked() else {
            return;
        };
        match document::resolve_link(&current, target) {
            LinkTarget::Document(path) => {
                if path != current {
                    self.history.update(|history| history.push(current));
                    self.show(path);
                }
            }
            LinkTarget::External(url) => open_url(&url),
            LinkTarget::Unsupported => {
                self.notice
                    .set(Some(format!("{UNSUPPORTED_LINK}: {target}")));
            }
        }
    }

    fn back(&self) {
        let mut previous = None;
        self.history.update(|history| previous = history.pop());
        if let Some(previous) = previous {
            self.show(previous);
        }
    }
}

/// Builds the view, opened on `path`.
pub fn view(path: Option<PathBuf>, open_url: OpenUrl) -> (El, Markdown) {
    let state = Markdown {
        path: RwSignal::new(None),
        history: RwSignal::new(Vec::new()),
        title: RwSignal::new(None),
        fields: RwSignal::new(Vec::new()),
        body: RwSignal::new(String::new()),
        error: RwSignal::new(None),
        notice: RwSignal::new(None),
    };
    if let Some(path) = path {
        state.show(path);
    }

    let header = el("div")
        .class("header")
        .when(
            move || !state.history.get().is_empty(),
            move || {
                el("div")
                    .class("back")
                    .text(BACK)
                    .on("click", move |_| state.back())
            },
        )
        .when(
            move || state.title.get().is_some(),
            move || {
                el("div")
                    .class("title")
                    .text_dyn(move || state.title.get().unwrap_or_default())
            },
        )
        .when(
            move || !state.fields.get().is_empty(),
            move || {
                el("div").class("chips").each(
                    move || state.fields.get(),
                    |field| field.clone(),
                    |(key, value)| el("span").class("chip").text(&format!("{key} {value}")),
                )
            },
        );

    let link_state = state;
    let scroll = el("div").class("scroll").each(
        // The whole body again when the text changes: Markdown has no
        // parts that stay the same from one text to the next.
        move || vec![state.body.get()],
        |body| body.clone(),
        move |body| {
            let open_url = open_url.clone();
            render(
                body,
                Rc::new(move |target: &str| link_state.follow_link(target, &open_url)),
            )
        },
    );
    // A new file starts at its top.
    reactive_graph::effect::Effect::new(move |_| {
        state.path.get();
        scroll.scroll_to(0., 0.);
    });

    let root = el("div").class("markdown").attr("tabindex", "0");
    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        if key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
            return;
        }
        // A line is three of the body's lines; a page most of the box.
        let scroll_by = move |lines: f64, pages: f64| {
            dispatched(move |page| {
                let height = page.size(scroll).map_or(0., |(_, height)| height);
                scroll.scroll_by(0., lines * 26. * 3. + pages * height * 0.8);
            });
        };
        match &key.key {
            Key::Character(character) if character == "j" => scroll_by(1., 0.),
            Key::Character(character) if character == "k" => scroll_by(-1., 0.),
            Key::Character(character) if character == " " => scroll_by(0., 1.),
            Key::Character(character) if character == "h" => state.back(),
            Key::Named(NamedKey::ArrowDown) => scroll_by(1., 0.),
            Key::Named(NamedKey::ArrowUp) => scroll_by(-1., 0.),
            Key::Named(NamedKey::PageDown) => scroll_by(0., 1.),
            Key::Named(NamedKey::PageUp) => scroll_by(0., -1.),
            Key::Named(NamedKey::Backspace | NamedKey::ArrowLeft) => state.back(),
            _ => {}
        }
    })
    .child(header)
    .when(
        move || state.notice.get().is_some(),
        move || {
            el("div")
                .class("notice")
                .text_dyn(move || state.notice.get().unwrap_or_default())
        },
    )
    .when(
        move || state.path.get().is_none(),
        || el("div").class("empty").text(NO_DOCUMENT),
    )
    .when(
        move || state.error.get().is_some(),
        move || {
            el("div")
                .class("error")
                .text_dyn(move || state.error.get().unwrap_or_default())
        },
    )
    .child(scroll);
    (root, state)
}

/// Markdown as elements, a link calling `on_link` with where it points.
pub(crate) fn render(source: &str, on_link: Rc<dyn Fn(&str)>) -> El {
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
                        let code = el("code");
                        if let CodeBlockKind::Fenced(language) = kind {
                            code.attr("data-language", &language);
                        }
                        let pre = el("pre").child(code);
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
