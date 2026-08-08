//! `horizon-board`: an event-sourced work-item store for Horizon's T-board
//! (task board).
//!
//! Items are persisted as an append-only JSONL event log at
//! `<data-home>/horizon/board/<sanitized-root>/events.jsonl`, serialised
//! across processes with an advisory `flock` (the CLI invokes separate
//! short-lived processes, so the agent event log's single-writer-thread
//! model does not apply). On read, events are folded in memory; malformed
//! lines are skipped with a count reported — the same house style as the
//! agent event log.
//!
//! Zero-dependency on `horizon-agent` or any other Horizon crate. The crate
//! boundary is a seam for future extension/plugin abstraction (owner
//! requirement).
//!
//! ## Item fields
//!
//! - `id`: sequential within the project (assigned on `item-created`)
//! - `title` / `body`: one-line summary / markdown body
//! - `status`: free-form string (recommended: proposed / ready /
//!   in-progress / review / done / blocked / archived). Not an enum so
//!   future vocabulary changes never break past events. `done` and
//!   `archived` are treated as closed — hidden from the default `list`
//!   view — by `is_closed_status`.
//! - `rank`: lexicographic rank string (lexorank over `a`-`z`)
//! - `assignee`: free-form string (empty = unassigned)
//! - `parent`: optional parent item id
//! - `depends_on`: list of item ids
//! - `links`: free-form strings (session ids, branch names, doc paths)
//! - `comments`: `{ author, text, at }` entries

mod event;
pub mod keeper;
mod model;
mod path;
mod rank;
mod store;
pub mod wire;

pub use model::{Comment, Item};
pub use store::{ListResult, Position, Store, StoreError, SubscribeStream};

// Re-exported for `horizon-logd`'s write path (the append logic that moved
// there from this crate's `store`). These were crate-internal when the write
// path lived here; now the daemon needs them.
pub use event::{read as read_events, BoardEvent, Envelope, ReadReport, SCHEMA, VERSION};
pub use model::{fold, is_closed_status, sorted_by_rank};
pub use rank::between as rank_between;
