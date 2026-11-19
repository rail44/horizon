//! Views the shell's tests drive the host of extensions with
//! (`tests/preview.rs`): each small, and each there for one thing the
//! host does for a guest. A pane of it is made on the view's name.
//!
//! The shell does not come with it.
#![cfg(target_family = "wasm")]

mod views;

#[horizon_ui::plugin(name = "fixture", title = "Fixture")]
mod fixture {
    pub mod pane {
        use std::cell::RefCell;

        /// What a pane keeps: which of the views it shows.
        pub struct State {
            pub view: String,
        }

        thread_local! {
            static SHOWN: RefCell<String> = const { RefCell::new(String::new()) };
        }

        pub fn mount(state: Option<State>) -> u64 {
            let view = state.map_or_default(|state| state.view);
            SHOWN.with(|shown| *shown.borrow_mut() = view.clone());
            let mounted = horizon_ui::guest::mount(|| crate::views::view(&view));
            mounted.unwrap_or_default()
        }

        pub fn state() -> State {
            State {
                view: SHOWN.with(|shown| shown.borrow().clone()),
            }
        }

        #[create]
        pub fn create(view: String) -> Result<State, String> {
            match crate::views::NAMES.contains(&view.as_str()) {
                true => Ok(State { view }),
                false => Err(format!("the fixture has no view named `{view}`")),
            }
        }

        #[command(title = "Show Over", description = "Put a view over the workspace.")]
        pub fn show_over() {
            crate::views::show_over();
        }
    }
}
