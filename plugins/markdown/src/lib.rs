//! The Markdown view as an extension: a pane keeps the file it shows,
//! which a link followed moves, and `back` goes back along the links.
#![cfg(target_family = "wasm")]

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use horizon::markdown::{Controls, Markdown};
use horizon::wake::Wake;
use horizon_ui::Callback;

wit_bindgen::generate!({
    path: ["../../crates/horizon-ui/wit", "wit"],
    world: "horizon-ext:markdown/markdown",
    with: {
        "horizon:ui/document": horizon_ui::guest::bindings::horizon::ui::document,
        "horizon:ui/desktop": horizon_ui::guest::bindings::horizon::ui::desktop,
        "horizon:ui/pane": horizon_ui::guest::bindings::horizon::ui::pane,
        "horizon:ui/files": horizon_ui::guest::bindings::horizon::ui::files,
    },
});

thread_local! {
    /// The file the view shows.
    static SHOWN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static CONTROLS: Cell<Option<Controls>> = const { Cell::new(None) };
}

/// The file the view shows is `path`: the pane is titled for it.
fn shows(path: Option<PathBuf>) {
    if let Some(name) = path.as_deref().and_then(|path| path.file_name()) {
        horizon_ui::guest::set_title(&name.to_string_lossy());
    }
    SHOWN.with(|shown| *shown.borrow_mut() = path);
}

struct Plugin;

impl Guest for Plugin {
    fn mount(state: Option<State>) -> u64 {
        let path = state.and_then(|state| state.path).map(PathBuf::from);
        shows(path.clone());
        let mounted = horizon_ui::guest::mount(|| {
            let (view, controls) = Markdown {
                path,
                open_url: Rc::new(horizon_ui::guest::open_url),
                on_navigate: Some(Callback::new(|path: PathBuf| shows(Some(path)))),
                wake: Wake::default(),
            }
            .view_with_controls();
            CONTROLS.with(|kept| kept.set(Some(controls)));
            Some(view)
        });
        mounted.unwrap_or_default()
    }

    fn state() -> State {
        let path = SHOWN.with(|shown| shown.borrow().clone());
        State {
            path: path.map(|path| path.to_string_lossy().into_owned()),
        }
    }
}

impl exports::commands::Guest for Plugin {
    fn back() {
        if let Some(controls) = CONTROLS.with(Cell::get) {
            controls.back();
        }
    }
}

export!(Plugin);
horizon_ui::export_view!();
