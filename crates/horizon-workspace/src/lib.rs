//! The workspace domain model, shared by both shells: tabs, panes, the
//! N-ary layout tree, session attachments, operations/queries,
//! workspace-mode state, and spatial navigation. Deliberately
//! framework-free (no floem, no gpui) — view projections live in each
//! shell. `SessionId` lives here too: it is the identity the model
//! attaches to panes, and the rest of each shell re-exports it.

pub mod commands;
mod layout;
mod mode;
mod nav;
mod operations;
pub mod persistence;
mod query;
mod session;
mod session_id;
pub mod snapshot;
pub mod types;

pub use mode::Direction;
pub use persistence::{
    InventoryError, InventoryReconcile, SessionInventory, WorkspaceStateError,
    WORKSPACE_STATE_VERSION,
};
pub use session_id::SessionId;
pub use types::{Pane, PaneId, PaneKind, SessionKind, SplitAxis, ViewKind, ViewState, Workspace};

#[cfg(test)]
mod tests;
