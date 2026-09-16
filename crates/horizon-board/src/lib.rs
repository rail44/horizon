//! Ordinary task storage and daemon client. Legacy logs require explicit import.
pub mod agents;
mod event;
mod model;
mod path;
mod rank;
mod store;
pub mod wire;

pub use model::{read_position_advances, Comment, Item};
pub use store::{ListResult, Position, Store, StoreError, SubscribeStream};

// Re-exported for `horizon-logd`'s write path (the append logic that moved
// there from this crate's `store`). These were crate-internal when the write
// path lived here; now the daemon needs them.
pub use event::{read as read_events, BoardEvent, Envelope, ReadReport, SCHEMA, VERSION};
pub use model::{fold, sorted_by_rank, tree_order};
pub use rank::between as rank_between;
