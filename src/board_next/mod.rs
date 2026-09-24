//! A from-scratch board, reachable only as named previews.
//!
//! It is two views, not one: [`BoardListView`] is the board's task list and
//! [`BoardThreadView`] is one task's thread. Neither contains the other:
//! each takes a pane's whole width and carries its own key map, and a pane
//! split is what puts the two side by side.
//!
//! What they share is [`model`] (the steering order, folding, the post
//! cursor, both key maps), [`spec`] (the measurements), [`parts`] (the
//! chips, tones, and running text they draw with), and the store hand-off:
//! both read a [`Store`](horizon_board::Store) through the same
//! [`BoardStoreSource`](crate::board_pane::execute::BoardStoreSource) /
//! [`run_store_job`](crate::board_pane::execute::run_store_job) pair the
//! shipped board pane uses, so a preview's in-memory store and a log-backed
//! one look the same from here; on the former every write answers
//! `StoreError::ReadOnly`, which the thread view reports on its notice line.
//!
//! Nothing here reaches the shell: neither view emits a command or holds a
//! session handle, so both compile for the preview plugin target as they
//! stand.

mod list;
mod model;
mod parts;
pub(crate) mod previews;
mod spec;
mod thread;

pub(crate) use list::BoardListView;
pub(crate) use thread::BoardThreadView;
