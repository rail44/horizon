//! The Markdown view: one file read from disk, drawn as running text,
//! with links to other Markdown files followed in place, and read again
//! when it changes on disk.
//!
//! What the view holds is a handful of signals (the file, the way back,
//! the parts of the document, a notice). The elements are built once; each
//! reads the signals it shows. Reading the front matter and resolving
//! links is `horizon-workspace`'s.

use std::path::PathBuf;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update};
use horizon_ui::{Callable, Callback, Component, El, dispatched, el};
#[cfg(not(target_family = "wasm"))]
use horizon_ui::{each_layout, relayout};

use crate::parts::{Chip, Tone};
use horizon_workspace::markdown::{LinkTarget, parse, resolve_link};
use keyboard_types::{Key, NamedKey};

use crate::wake::Wake;

mod render;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod watch;

pub use render::{Headings, render};

const BACK: &str = "Back";
const NO_DOCUMENT: &str = "No document";
const UNSUPPORTED_LINK: &str = "Not a Markdown file";

mod styles {
    horizon_ui::css_module! {"
    .markdown { display: flex; flex-direction: column; height: 100%; min-width: 240px;
                outline: none; background: var(--background); color: var(--text);
                font-family: var(--font-family); font-size: var(--body-size);
                line-height: var(--body-line);
        .header { flex: none; display: flex; flex-direction: column; gap: var(--u8);
                  padding: 0 var(--u16); }
        .header > :first-child { margin-top: var(--u24); }
        .back { align-self: flex-start; padding: 0 var(--u8); border-radius: var(--radius);
                font-size: var(--meta-size); line-height: calc(var(--u4) * 5); color: var(--text-muted);
                cursor: pointer; }
        .back:hover { background: var(--surface-raised); }
        .title { font-size: var(--t1-size); line-height: var(--t1-line); font-weight: 600; }
        .chips { display: flex; flex-wrap: wrap; align-items: center; gap: var(--u4); }
        .notice, .empty, .error { flex: none; padding: var(--u8) var(--u16) 0 var(--u16);
                                  font-size: var(--meta-size); line-height: var(--meta-line);
                                  color: var(--text-muted); }
        .error { color: var(--danger); }
        .scroll { flex: 1; min-height: 0; overflow-y: scroll;
                  padding: var(--u24) var(--u16) var(--u32) var(--u16); }
    }
"}
}

/// What is outside the view: a URL handed to the desktop.
pub type OpenUrl = Rc<dyn Fn(&str)>;

/// A Markdown file, read from disk and read again when it changes. Links
/// to other Markdown files are followed in place, with a way back.
///
/// The file it opens on is a value: to show another from outside, another
/// is placed.
pub struct Markdown {
    /// The file it opens on.
    pub path: Option<PathBuf>,
    /// Hands a URL to the desktop.
    pub open_url: OpenUrl,
    /// Told of the file a link, or the way back, took the view to.
    pub on_navigate: Option<Callback<PathBuf>>,
    /// Makes the page take in a change of the file.
    pub wake: Wake,
}

/// What the view keeps.
#[derive(Clone, Copy)]
struct State {
    path: RwSignal<Option<PathBuf>>,
    /// The files left by following links, most recent last.
    history: RwSignal<Vec<PathBuf>>,
    title: RwSignal<Option<String>>,
    fields: RwSignal<Vec<(String, String)>>,
    body: RwSignal<String>,
    error: RwSignal<Option<String>>,
    /// A line under the header: a link the view did not follow.
    notice: RwSignal<Option<String>>,
    on_navigate: Option<Callback<PathBuf>>,
}

impl State {
    /// Goes to `path` from here: whoever placed the view is told.
    fn go(&self, path: PathBuf) {
        self.show(path.clone());
        if let Some(on_navigate) = self.on_navigate {
            on_navigate.run(path);
        }
    }

    fn show(&self, path: PathBuf) {
        self.notice.set(None);
        self.path.set(Some(path));
        self.reload();
    }

    /// Reads the file again.
    fn reload(&self) {
        let Some(path) = self.path.get_untracked() else {
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(source) => {
                let document = parse(&source);
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
        match resolve_link(&current, target) {
            LinkTarget::Document(path) => {
                if path != current {
                    self.history.update(|history| history.push(current));
                    self.go(path);
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
            self.go(previous);
        }
    }
}

impl Component for Markdown {
    fn view(self) -> El {
        build(self).0
    }
}

/// What a plugin showing the view carries its commands out on.
#[derive(Clone, Copy)]
pub struct Controls {
    state: State,
}

impl Controls {
    /// Goes back to the file a link was followed from, if there is one.
    pub fn back(&self) {
        self.state.back();
    }
}

impl Markdown {
    /// The view, and what its commands are carried out on.
    pub fn view_with_controls(self) -> (El, Controls) {
        build(self)
    }
}

/// A change of the file the view shows is read in once the file has sat
/// still, `wake` making the page take it in.
fn build(given: Markdown) -> (El, Controls) {
    let Markdown {
        path,
        open_url,
        on_navigate,
        wake,
    } = given;
    let state = State {
        on_navigate,
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
        .class(styles::HEADER)
        .when(
            move || !state.history.get().is_empty(),
            move || {
                el("div")
                    .class(styles::BACK)
                    .text(BACK)
                    .on("click", move |_| state.back())
            },
        )
        .when(
            move || state.title.get().is_some(),
            move || {
                el("div")
                    .class(styles::TITLE)
                    .text_dyn(move || state.title.get().unwrap_or_default())
            },
        )
        .when(
            move || !state.fields.get().is_empty(),
            move || {
                el("div").class(styles::CHIPS).each(
                    move || state.fields.get(),
                    |field| field.clone(),
                    |(key, value)| {
                        Chip {
                            tone: Tone::Muted,
                            text: &format!("{key} {value}"),
                        }
                        .view()
                    },
                )
            },
        );

    let link_state = state;
    let scroll = el("div").class(styles::SCROLL).each(
        // The whole body again when the text changes: Markdown has no
        // parts that stay the same from one text to the next.
        move || vec![state.body.get()],
        |body| body.clone(),
        move |body| {
            let open_url = open_url.clone();
            render(
                body,
                Headings::Scaled,
                Rc::new(move |target: &str| link_state.follow_link(target, &open_url)),
            )
        },
    );
    // A new file starts at its top, and is the one watched from then on.
    #[cfg(not(target_family = "wasm"))]
    {
        let changed = watch::Changed::default();
        let noted = changed.clone();
        // (The watch is what the effect keeps: the one before it ends when
        // the next takes its place.)
        Effect::new(move |_| {
            let path = state.path.get();
            scroll.scroll_to(0., 0.);
            path.and_then(|path| watch::on(path, noted.clone(), wake.clone()))
        });
        each_layout(move |_| {
            if changed.settled() {
                state.reload();
                relayout();
            }
        });
    }
    // (A plugin has no watch of its own yet: the file is read as it is.)
    #[cfg(target_family = "wasm")]
    {
        let _ = wake;
        Effect::new(move |_| {
            let _ = state.path.get();
            scroll.scroll_to(0., 0.);
        });
    }

    let root = el("div").class(styles::MARKDOWN).attr("tabindex", "0");
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
                .class(styles::NOTICE)
                .text_dyn(move || state.notice.get().unwrap_or_default())
        },
    )
    .when(
        move || state.path.get().is_none(),
        || el("div").class(styles::EMPTY).text(NO_DOCUMENT),
    )
    .when(
        move || state.error.get().is_some(),
        move || {
            el("div")
                .class(styles::ERROR)
                .text_dyn(move || state.error.get().unwrap_or_default())
        },
    )
    .child(scroll);
    (root, Controls { state })
}

/// What a pane of a document view keeps: the file it shows.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct DocumentState {
    pub path: Option<PathBuf>,
}

#[cfg(not(target_family = "wasm"))]
impl DocumentState {
    /// The file's name, as a pane's title.
    pub fn file_name(&self) -> Option<String> {
        let name = self.path.as_deref()?.file_name()?;
        Some(name.to_string_lossy().into_owned())
    }
}

/// The view as a plugin: a pane keeps the file it shows, which a link
/// followed moves.
#[cfg(not(target_family = "wasm"))]
pub struct MarkdownPlugin {
    pub open_url: OpenUrl,
    pub wake: Wake,
}

#[cfg(not(target_family = "wasm"))]
impl crate::plugin::Plugin for MarkdownPlugin {
    const NAME: &'static str = "markdown";
    const TITLE: &'static str = "Markdown";
    type State = DocumentState;
    type Command = std::convert::Infallible;
    type Asked = std::convert::Infallible;

    fn commands() -> Vec<crate::plugin::Command<std::convert::Infallible>> {
        Vec::new()
    }

    fn title(state: &DocumentState) -> String {
        state.file_name().unwrap_or_else(|| Self::TITLE.to_string())
    }

    fn mount(
        self,
        state: DocumentState,
        _: Callback<std::convert::Infallible>,
    ) -> crate::plugin::Mounted<DocumentState, std::convert::Infallible> {
        let shown = RwSignal::new(state.clone());
        let view = Markdown {
            path: state.path,
            open_url: self.open_url,
            wake: self.wake,
            on_navigate: Some(Callback::new(move |path| {
                shown.set(DocumentState { path: Some(path) })
            })),
        }
        .view();
        crate::plugin::Mounted {
            view,
            run: Callback::new(|never| match never {}),
            enabled: None,
            state: horizon_ui::reactive_graph::wrappers::read::Signal::derive_local(move || {
                horizon_ui::reactive_graph::traits::Get::get(&shown)
            }),
        }
    }
}
