//! Horizon's side of `extensions/wit/host.wit`: what an extension's view
//! calls beyond drawing.
//!
//! A call runs inside the plugin's own call, where only the store's data
//! is in reach. What it answers from (the theme, the type scale, the
//! document reader) is global; what needs the application (recording the
//! pane's document, watching a file, opening a URL) is left as a
//! [`Request`] for the view to carry out once the plugin has returned.

use std::path::{Path, PathBuf};

use gpui::{Hsla, Rgba};
use gpui_wit_plugin::wasmtime::component::{HasSelf, Linker};
use gpui_wit_plugin::State;

use crate::markdown::document::{self, LinkTarget};
use crate::theme;
use crate::ui::spec;

mod bindings {
    gpui_wit_plugin::wasmtime::component::bindgen!({
        path: "extensions/wit",
        world: "extension",
        wasmtime_crate: gpui_wit_plugin::wasmtime,
    });
}

use bindings::horizon::extension::host::{self, Link, Look, Parsed, Step};

/// Something an extension asked for that the application carries out.
pub(super) enum Request {
    SetDocument(PathBuf),
    Watch(PathBuf),
    OpenUrl(String),
}

/// What one extension view's store holds for these calls.
pub(super) struct HostData {
    /// The file the pane was opened on.
    pub(super) document: Option<PathBuf>,
    pub(super) requests: Vec<Request>,
}

fn hex(color: Hsla) -> String {
    let color = Rgba::from(color);
    let channel = |value: f32| (value * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a)
    )
}

fn step(step: spec::Step) -> Step {
    Step {
        size: step.size.into(),
        line_height: step.line_height.into(),
    }
}

impl host::Host for HostData {
    fn current_look(&mut self) -> Look {
        Look {
            background: format!("#{:06x}", theme::background()),
            text_primary: hex(theme::text_primary()),
            text_muted: hex(theme::text_muted()),
            danger: hex(theme::danger()),
            title: step(spec::t1()),
            body: step(spec::body()),
            meta: step(spec::meta()),
            anchor_weight: spec::ANCHOR.0,
            pad_x: spec::pad_x().into(),
            gap_label: spec::gap_label().into(),
            gap_tight: spec::gap_tight().into(),
            gap_section: spec::gap_section().into(),
            gap_composer: spec::gap_composer().into(),
            min_width: spec::VIEW_MIN_WIDTH.into(),
        }
    }

    fn document(&mut self) -> Option<String> {
        let document = self.document.as_ref()?;
        Some(document.to_string_lossy().into_owned())
    }

    fn set_document(&mut self, path: String) {
        self.requests.push(Request::SetDocument(path.into()));
    }

    fn watch(&mut self, path: String) {
        self.requests.push(Request::Watch(path.into()));
    }

    fn open_url(&mut self, url: String) {
        self.requests.push(Request::OpenUrl(url));
    }

    fn parse(&mut self, source: String) -> Parsed {
        let document = document::parse(&source);
        Parsed {
            title: document.title,
            fields: document.fields,
            body: document.body,
        }
    }

    fn resolve_link(&mut self, current: String, href: String) -> Link {
        match document::resolve_link(Path::new(&current), &href) {
            LinkTarget::Document(path) => Link::Document(path.to_string_lossy().into_owned()),
            LinkTarget::External(url) => Link::External(url),
            LinkTarget::Unsupported => Link::Unsupported,
        }
    }
}

/// Offers the interface to every plugin of `linker`'s runtime.
pub(super) fn add_to_linker(linker: &mut Linker<State>) -> gpui_wit_plugin::wasmtime::Result<()> {
    host::add_to_linker::<State, HasSelf<HostData>>(linker, |state| {
        (state.data.downcast_mut::<HostData>())
            .expect("an extension view's store holds its host data")
    })
}
