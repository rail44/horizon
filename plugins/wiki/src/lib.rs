//! The wiki view, an extension: a page of the project wiki with the
//! owner's comment threads anchored in it. A pane keeps the page's file,
//! which a link followed moves.
//!
//! The pages and the threads are files in Horizon's own data, which the
//! extension is lent to write in: a comment is a line appended to the
//! page's log, where the daemon's wiki flow finds it.
//!
//! The view itself is `view`.
#![cfg(target_family = "wasm")]

mod view;

#[horizon_ui::plugin(
    name = "wiki",
    title = "Wiki",
    access(read = ["/"], write = ["{data}/wiki"])
)]
mod wiki {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use horizon_ui::{Callback, Component};

    use crate::view::Wiki;

    /// What a pane keeps: the file of the page it shows.
    pub struct State {
        pub path: Option<String>,
    }

    thread_local! {
        /// The page's file the view shows.
        static SHOWN: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// The page the view shows is at `path`: the pane is titled for it.
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
            let view = Wiki {
                path,
                open_url: Rc::new(horizon_ui::guest::open_url),
                on_navigate: Some(Callback::new(|path: PathBuf| shows(Some(path)))),
            }
            .view();
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

    /// A page of the wiki of the project the pane was asked for in: the
    /// overview, unless a page is named.
    #[create]
    pub fn create(page: Option<String>, directory: WorkingDirectory) -> Result<State, String> {
        let page = page.unwrap_or_else(|| "index".to_string());
        let directory = PathBuf::from(directory.path());
        let store = horizon_wiki::Store::for_dir(&directory)
            .ok_or_else(|| format!("{} is not in a repository with a wiki", directory.display()))?;
        let path = store.page_path(&page).map_err(|error| error.to_string())?;
        if !path.is_file() {
            return Err(format!("the wiki has no page `{page}`"));
        }
        Ok(State {
            path: Some(path.to_string_lossy().into_owned()),
        })
    }
}
