//! Ordinary task storage and daemon client. Legacy logs require explicit import.
//!
//! The queries, the event vocabulary, and the in-memory store source build
//! anywhere. Reaching the outside world — the event file, the git root, the
//! `horizon-logd` socket — is native-only, gated at the module declarations
//! below.
pub mod agents;
mod event;
mod model;
mod path;
mod rank;
mod store;
pub mod wire;

pub use model::{Comment, Item, read_position_advances};
pub use store::{ListResult, Position, Store, StoreError, sample_envelopes};

pub use store::SubscribeStream;

// Re-exported for `horizon-logd`'s write path (the append logic that moved
// there from this crate's `store`). These were crate-internal when the write
// path lived here; now the daemon needs them.
pub use event::{BoardEvent, Envelope, ReadReport, SCHEMA, VERSION, read_text};
pub use model::{fold, sorted_by_rank, tree_order};
pub use rank::between as rank_between;
pub use store::read_events;
