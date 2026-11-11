//! The Markdown view as an extension: one file read from disk, drawn as
//! running text, with links to other Markdown files followed inside the
//! same pane.
//!
//! The view holds what it shows (the file, the way back, a notice) and
//! decides what a link or a key does. Horizon draws the Markdown itself
//! and the controls (the `markdown`, `button` and `chip` components),
//! reads the front matter and resolves links (`horizon:extension/host`),
//! and watches the file. The file is read here, through WASI.

use gpui_wit_plugin_sdk::*;

wit_bindgen::generate!({ path: "../wit", world: "extension" });

use horizon::extension::host::{self, Link, Look, Parsed, Step};

const BACK: &str = "Back";
const NO_DOCUMENT: &str = "No document";
const UNSUPPORTED_LINK: &str = "Not a Markdown file";

struct Markdown {
    path: Option<String>,
    /// The files left by following links, most recent last.
    history: Vec<String>,
    content: Result<Parsed, String>,
    /// A line under the header: a link the view did not follow.
    notice: Option<String>,
    look: Look,
    scroll: ScrollHandle,
}

impl Markdown {
    fn new(_cx: &mut Cx<Self>) -> Self {
        let mut view = Markdown {
            path: None,
            history: Vec::new(),
            content: Err(String::new()),
            notice: None,
            look: host::current_look(),
            scroll: ScrollHandle::new(),
        };
        if let Some(path) = host::document() {
            view.show(path);
        }
        view
    }

    /// Points the view at `path` from outside: the history is left as it
    /// is, since the jump was not a link followed here.
    fn open(&mut self, path: String) {
        if self.path.as_ref() == Some(&path) {
            self.reload();
        } else {
            self.show(path);
        }
    }

    fn show(&mut self, path: String) {
        self.notice = None;
        self.scroll.scroll_to(0.);
        host::watch(&path);
        host::set_document(&path);
        self.path = Some(path);
        self.reload();
    }

    fn reload(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        self.content = match std::fs::read_to_string(path) {
            Ok(source) => Ok(host::parse(&source)),
            Err(error) => Err(format!("{path}: {error}")),
        };
    }

    fn follow_link(&mut self, target: &str) {
        let Some(current) = self.path.clone() else {
            return;
        };
        match host::resolve_link(&current, target) {
            Link::Document(path) => {
                if path != current {
                    self.history.push(current);
                    self.show(path);
                }
            }
            Link::External(url) => host::open_url(&url),
            Link::Unsupported => self.notice = Some(format!("{UNSUPPORTED_LINK}: {target}")),
        }
    }

    fn back(&mut self) {
        if let Some(previous) = self.history.pop() {
            self.show(previous);
        }
    }

    fn on_key(&mut self, event: &Value, cx: &mut Cx<Self>) {
        if ["control", "alt", "platform"]
            .iter()
            .any(|modifier| event[modifier] == true)
        {
            return;
        }
        let line = self.look.body.line_height * 3.;
        match event["key"].as_str().unwrap_or_default() {
            "j" | "down" => self.scroll.scroll_by(line, 0.),
            "k" | "up" => self.scroll.scroll_by(-line, 0.),
            "space" | "pagedown" => self.scroll.scroll_by(0., 0.8),
            "pageup" => self.scroll.scroll_by(0., -0.8),
            "backspace" | "h" | "left" => self.back(),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn header(&self, cx: &mut Cx<Self>) -> Option<Element> {
        let look = &self.look;
        let document = self.content.as_ref().ok();
        let title = document.and_then(|document| document.title.clone());
        let fields = document.map(|document| document.fields.as_slice());
        let fields = fields.unwrap_or_default();
        let can_go_back = !self.history.is_empty();
        if title.is_none() && fields.is_empty() && !can_go_back {
            return None;
        }
        let mut header = div()
            .flex_col()
            .flex_none()
            .w_full()
            .px(look.pad_x)
            .pt(look.gap_section)
            .gap(look.gap_tight);
        if can_go_back {
            let back = component("button", json!({ "label": BACK })).on(
                "click",
                cx.listener(|view, _, cx| {
                    view.back();
                    cx.notify();
                }),
            );
            header = header.child(div().flex_row().items_center().child(back));
        }
        if let Some(title) = title {
            header = header.child(
                sized(div(), &look.title)
                    .font_weight(look.anchor_weight)
                    .text_color(&look.text_primary)
                    .child(title),
            );
        }
        if !fields.is_empty() {
            let chips = (fields.iter()).map(|(key, value)| {
                component("chip", json!({ "label": format!("{key} {value}") }))
            });
            header = header.child(
                div()
                    .flex_row()
                    .items_center()
                    .flex_wrap()
                    .gap(look.gap_label)
                    .children(chips),
            );
        }
        Some(header)
    }

    fn body(&self, cx: &mut Cx<Self>) -> Element {
        let look = &self.look;
        let line = |text: &str, color: &str| {
            sized(div(), &look.meta)
                .px(look.pad_x)
                .pt(look.gap_section)
                .text_color(color)
                .child(text)
        };
        let Some(path) = &self.path else {
            return line(NO_DOCUMENT, &look.text_muted);
        };
        match &self.content {
            Err(error) => line(error, &look.danger),
            Ok(document) => div()
                .flex_col()
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .px(look.pad_x)
                .pt(look.gap_section)
                .pb(look.gap_composer)
                .child(
                    component(
                        "markdown",
                        json!({ "id": format!("markdown:{path}"), "source": document.body }),
                    )
                    .on(
                        "link",
                        cx.listener(|view, target, cx| {
                            view.follow_link(target.as_str().unwrap_or_default());
                            cx.notify();
                        }),
                    ),
                ),
        }
    }
}

/// `element` with the type scale's `step`.
fn sized(element: Element, step: &Step) -> Element {
    element.text_size(step.size).line_height(step.line_height)
}

impl Render for Markdown {
    fn host_event(&mut self, name: &str, payload: &Value, cx: &mut Cx<Self>) {
        match name {
            "open" => self.open(payload.as_str().unwrap_or_default().to_owned()),
            "file-changed" => self.reload(),
            "look-changed" => self.look = host::current_look(),
            _ => return,
        }
        cx.notify();
    }

    fn render(&mut self, cx: &mut Cx<Self>) -> Element {
        let look = &self.look;
        let mut view = div()
            .flex_col()
            .size_full()
            .min_w(look.min_width)
            .bg(&look.background)
            .text_color(&look.text_primary)
            .on_key_down(cx.listener(|view, event, cx| view.on_key(event, cx)));
        if let Some(header) = self.header(cx) {
            view = view.child(header);
        }
        if let Some(notice) = &self.notice {
            view = view.child(
                sized(div(), &look.meta)
                    .flex_none()
                    .px(look.pad_x)
                    .pt(look.gap_tight)
                    .text_color(&look.text_muted)
                    .child(notice.as_str()),
            );
        }
        view.child(
            sized(div(), &look.body)
                .flex_1()
                .min_h(0.)
                .w_full()
                .child(self.body(cx)),
        )
    }
}

export_view!(Markdown::new);
