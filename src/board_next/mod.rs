//! A from-scratch board: the task list and one task's thread, as two views.
//!
//! [`BoardListView`] is the board's task list and [`BoardThreadView`] is
//! one task's thread. Neither contains the other: each takes a pane's
//! whole width and carries its own key map, and a pane split is what puts
//! the two side by side.
//!
//! What they share is [`model`] (the tree order, folding, the post cursor,
//! drag placement, both key maps), [`spec`] (the measurements), [`parts`]
//! (the chips, tones, and running text they draw with), [`activity`] (what
//! a bound session reports), and the store hand-off: both read a
//! [`Store`](horizon_board::Store) through the same
//! [`BoardStoreSource`](crate::board_pane::execute::BoardStoreSource) /
//! [`run_store_job`](crate::board_pane::execute::run_store_job) pair the
//! shipped board pane uses, so a preview's in-memory store and a
//! log-backed one look the same from here; on the former every write
//! answers `StoreError::ReadOnly`, which both views report on their notice
//! line.
//!
//! # Native-only halves
//!
//! [`live`] (the logd subscribe pump that re-reads on an external write)
//! and [`sessions`] (observing the shell's `AgentSession` entities) reach
//! the host, so they are gated at their declaration below, as is each
//! view's shell-facing block: `root`, `board_command`, `observe_sessions`,
//! `finish_inventory_refresh`, and the thread's `task_session`. A store
//! handed in whole — a preview's — has no project directory, so it gets no
//! pump and no session observation.
//!
//! # What the views report upwards
//!
//! Neither view reaches the shell. Every request that needs a pane, a
//! session, or the workspace's inventory leaves as an event:
//!
//! | Event | Emitted by | What the shell does with it |
//! | --- | --- | --- |
//! | [`events::OpenTaskThread`] | the list, on `Enter` | opens that task's thread in a pane |
//! | [`events::OpenTaskSession`] | the thread, on 「セッション」 | resolves and attaches the bound agent session |
//! | [`events::BoardSessionsRefreshed`] | both, after a load found new bindings | resolves the ids in its inventory, then calls `finish_inventory_refresh` and `observe_sessions` |
//!
//! # The list's keys
//!
//! `j`/`down` and `k`/`up` move the selection, `l`/`right` opens the
//! selected parent and `h`/`left` closes it (on a leaf, `h` selects its
//! parent), `o` folds the finished band open and shut, and `Enter` asks
//! for the selected task's thread. The thread's own keys are `j`/`k` for
//! the post cursor, `e` to fold the post under it, `Enter` for the
//! composer, and `Esc` back out of it.

pub(crate) mod activity;
pub(crate) mod events;
mod list;
pub(crate) mod model;
mod parts;
pub(crate) mod previews;
mod spec;
mod thread;

#[cfg(not(target_family = "wasm"))]
pub(crate) mod live;
#[cfg(not(target_family = "wasm"))]
pub(crate) mod sessions;

pub(crate) use list::BoardListView;
pub(crate) use thread::BoardThreadView;
