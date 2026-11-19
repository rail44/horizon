//! The Markdown view, an extension: a pane keeps the file it shows,
//! which a link followed moves, and `back` goes back along the links.
//! The view itself is `view`.
//!
//! The module below is all there is of the extension's shape: its WIT
//! world, its exports and its manifest are made from it
//! (`horizon_ui::plugin`).
#![cfg(target_family = "wasm")]

mod view;

#[horizon_ui::plugin(name = "markdown", title = "Markdown", access(read = ["/"]))]
mod markdown {
    pub mod pane {
        use std::cell::{Cell, RefCell};
        use std::path::PathBuf;
        use std::rc::Rc;

        use horizon_ui::Callback;

        use crate::view::{Controls, Markdown};

        /// What a pane keeps: the file it shows.
        pub struct State {
            pub path: Option<String>,
        }

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

        pub fn mount(state: Option<State>) -> u64 {
            let path = state.and_then(|state| state.path).map(PathBuf::from);
            shows(path.clone());
            let mounted = horizon_ui::guest::mount(|| {
                let (view, controls) = Markdown {
                    path,
                    open_url: Rc::new(horizon_ui::guest::open_url),
                    on_navigate: Some(Callback::new(|path: PathBuf| shows(Some(path)))),
                }
                .view_with_controls();
                CONTROLS.with(|kept| kept.set(Some(controls)));
                Some(view)
            });
            mounted.unwrap_or_default()
        }

        pub fn state() -> State {
            let path = SHOWN.with(|shown| shown.borrow().clone());
            State {
                path: path.map(|path| path.to_string_lossy().into_owned()),
            }
        }

        #[create(file(extensions = ["md", "markdown"]))]
        pub fn create(file: File) -> State {
            State {
                path: Some(file.path()),
            }
        }

        #[command(
            title = "Back",
            description = "Go back to the file a link was followed from."
        )]
        pub fn back() {
            if let Some(controls) = CONTROLS.with(Cell::get) {
                controls.back();
            }
        }
    }
}
