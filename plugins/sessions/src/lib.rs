//! The list of the workspace's sessions, an extension: the terminals and
//! the agents by the project each works in, what each is doing, and what
//! can be done to the chosen one. A pane of it keeps nothing: the
//! sessions are the shell's (`horizon:ui/sessions`), and so is what is
//! done to them (`horizon:ui/session-control`).
//!
//! What the list is made of -- the order, what a search finds -- is
//! `list`, and the view is `view`.
#![cfg(target_family = "wasm")]

mod list;
mod view;

#[horizon_ui::plugin(name = "sessions", title = "Sessions")]
mod sessions {
    pub mod pane {
        pub fn mount() -> u64 {
            let mounted = horizon_ui::guest::mount(|| Some(crate::view::view()));
            mounted.unwrap_or_default()
        }
    }
}
