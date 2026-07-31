//! Version negotiation, the one handshake every runtime's hub shares
//! (`docs/remoc-adoption-design.md` §3): a peer advertises an inclusive
//! `[min_supported, current]` range in its `hello`, and the two ranges
//! intersect to the highest version both can honor.
//!
//! Which numbers a build puts in that range is *not* this crate's business:
//! the version constants belong to the hub being spoken, so the "range this
//! build advertises" constructor lives beside them --
//! `horizon_agent::wire::agent_version_range` and
//! `horizon_terminal_core::wire::terminal_version_range`, one pair per hub
//! since `docs/runtime-crate-alignment-design.md` phase 2.
//!
//! The `[`SessionHub::hello`]` links in the type docs below point at that
//! hub trait, which lives in the crate that owns it; the wording is pinned
//! byte-for-byte by the committed wire-schema artifact (it is these types'
//! `description`), so it is left exactly as it was written.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// An inclusive protocol-version range one peer supports, as exchanged in
/// [`SessionHub::hello`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VersionRange {
    pub min_supported: u32,
    pub current: u32,
}

impl VersionRange {
    /// The range a build with these two version constants advertises.
    pub const fn new(min_supported: u32, current: u32) -> Self {
        Self {
            min_supported,
            current,
        }
    }

    /// The highest version both ranges support, if the ranges overlap.
    pub fn negotiate(self, other: Self) -> Option<u32> {
        let low = self.min_supported.max(other.min_supported);
        let high = self.current.min(other.current);
        (low <= high).then_some(high)
    }
}

impl std::fmt::Display for VersionRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[v{}, v{}]", self.min_supported, self.current)
    }
}

/// The client half of the version negotiation, carried by the first rtc
/// call on every connection ([`SessionHub::hello`]).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientHello {
    pub supported: VersionRange,
    pub binary_id: String,
}

impl ClientHello {
    pub fn new(supported: VersionRange, binary_id: impl Into<String>) -> Self {
        Self {
            supported,
            binary_id: binary_id.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ranges_negotiate_to_the_highest_shared_version() {
        let ours = VersionRange {
            min_supported: 10,
            current: 12,
        };
        let theirs = VersionRange {
            min_supported: 11,
            current: 14,
        };
        assert_eq!(ours.negotiate(theirs), Some(12));
        assert_eq!(theirs.negotiate(ours), Some(12));
    }

    #[test]
    fn disjoint_version_ranges_do_not_negotiate() {
        let ours = VersionRange {
            min_supported: 10,
            current: 10,
        };
        let theirs = VersionRange {
            min_supported: 11,
            current: 14,
        };
        assert_eq!(ours.negotiate(theirs), None);
        assert_eq!(theirs.negotiate(ours), None);
    }
}
