//! Ordinary task storage and daemon client. Legacy logs require explicit import.
//!
//! The queries, the event vocabulary, and the in-memory store source build
//! anywhere. Reaching the outside world — the event file, the git root, the
//! `horizon-logd` socket — is native-only, gated at the module declarations
//! below.
pub mod agents;
mod event;
mod model;
#[cfg(not(target_family = "wasm"))]
mod path;
mod rank;
mod store;
pub mod wire;

pub use model::{read_position_advances, Comment, Item};
pub use store::{sample_envelopes, ListResult, Position, Store, StoreError};

#[cfg(not(target_family = "wasm"))]
pub use store::SubscribeStream;

// Re-exported for `horizon-logd`'s write path (the append logic that moved
// there from this crate's `store`). These were crate-internal when the write
// path lived here; now the daemon needs them.
pub use event::{read_text, BoardEvent, Envelope, ReadReport, SCHEMA, VERSION};
pub use model::{fold, sorted_by_rank, tree_order};
pub use rank::between as rank_between;
#[cfg(not(target_family = "wasm"))]
pub use store::read_events;
