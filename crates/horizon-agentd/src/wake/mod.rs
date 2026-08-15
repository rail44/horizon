//! Board wake subscriber: the daemon-lifetime task that subscribes to
//! `horizon-logd`'s board-event poke stream, applies the wake policy, and
//! wakes the keeper when the policy decides to.
//!
//! This is the v2 landing point described in `docs/board-keeper-design.md`:
//! the keeper wakes automatically when the board changes, rather than
//! requiring a manual launch. The mechanism is a scheduling concern layered
//! on top of the existing role/skill/tool design — no role definition or
//! tool changes are involved.
//!
//! ## Architecture
//!
//! - [`policy`] — pure wake-policy logic (author filter, coalesce, cursor,
//!   multi-wake prevention). Unit-tested without sockets.
//! - [`cursor`] — persists the last-processed seq to disk for restart
//!   recovery.
//! - [`action`] — the swappable `WakeAction` seam. v1 spawns a fresh keeper
//!   session; #36 will plug a resume-with-aggregated-context impl here.
//! - [`task`] — the async subscriber loop that ties the above together.
//!
//! The wake policy is a subscriber-side concern, not logd's job
//! (`docs/logd-design.md` decision 8): logd notifies; the subscriber decides
//! who to wake and when.

pub(crate) mod action;
pub(crate) mod cursor;
pub(crate) mod policy;
pub(crate) mod task;

pub(crate) use action::SpawnKeeper;
pub(crate) use task::spawn as spawn_subscriber;
