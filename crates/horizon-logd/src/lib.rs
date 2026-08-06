//! `horizon-logd`: the log infrastructure daemon (`docs/logd-design.md`).
//!
//! Owns board WRITES (the exclusive-flock append + id/rank computation that
//! used to live in `horizon-board`'s `Store`) and serves them over its own
//! unix socket as the [`LogHub`] rtc trait — a third daemon beside
//! `horizon-agentd` and `horizon-terminald`, spawn-on-demand by any client.
//!
//! **Stage A (this crate):** `ingest` only. Board reads stay file folds in
//! the library; **stage B adds the subscribe stream** (raw NDJSON pokes,
//! first-byte-sniffed away from the remoc chmux path on the same socket);
//! agent-events migration and the DuckDB projection are later phases.
//!
//! **No persistence, no readiness gate.** Like terminald, this daemon owns no
//! event log of its own and has nothing to resume at startup or flush on
//! drain: bind, accept, serve. The board events.jsonl is written through
//! (appended + flushed) before each `ingest` reply returns, so durability is
//! per-call, not per-session.
//!
//! **Crate shape:** `lib + bin` per the task brief. The lib holds the daemon
//! implementation ([`Hub`], [`writer`]); the bin is a thin entry point. The
//! wire types ([`LogHub`] trait, version pair, `IngestRequest`/`IngestReply`)
//! live in `horizon-board` (not here) to break what would otherwise be a
//! circular package dependency: this crate depends on `horizon-board` for the
//! append logic, and `horizon-board`'s write path is this daemon's client.

pub mod hub;
pub mod subscribe;
pub mod subscribers;
pub mod writer;

pub use hub::Hub;
pub use subscribe::handle_subscribe;
pub use subscribers::SubscriberRegistry;
