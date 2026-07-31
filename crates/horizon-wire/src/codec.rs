//! The codec pin and the receive loops' skip diagnostic — the two pieces of
//! `docs/remoc-adoption-design.md`'s adoption conditions 1 and 2 that carry
//! no domain vocabulary at all.

/// The one wire codec, named everywhere a codec parameter appears
/// (adoption condition 1): Postbag in its Full configuration — the exact
/// configuration the spike's perf/skew experiments validated
/// (`docs/research/remoc-spike-2026-07-20.md` §§1–2). Never
/// `remoc::codec::Default`: the workspace builds remoc with default
/// features off, so the default-codec alias deliberately does not resolve
/// to a usable codec and a remoc upgrade that changes its default cannot
/// silently fork the wire.
///
/// Postbag is **not self-describing** (`deserialize_any` is rejected), so
/// the vocabularies' free-form JSON payloads — tool call inputs/outputs —
/// cross this wire as `horizon_agent::contract::JsonValue` (their JSON
/// text in one string) instead of `serde_json::Value`; see that type's
/// doc for the format-aware encoding that keeps the event log's on-disk
/// JSONL format byte-identical.
pub type WireCodec = remoc::codec::Postbag;

/// A rate-limited log for the receive/send loops' skip paths (adoption
/// condition 2: a poisoned item is skipped, never fatal): logs the first
/// occurrence, then only at powers of two and every 1000th, with the
/// running count, so a peer stuck emitting undecodable items cannot
/// flood stderr at channel throughput.
pub struct DecodeSkipLog {
    label: &'static str,
    skipped: u64,
}

impl DecodeSkipLog {
    pub const fn new(label: &'static str) -> Self {
        Self { label, skipped: 0 }
    }

    /// Records one skipped item, logging at 1, 10, 100, 1000, ...
    pub fn note(&mut self, error: &dyn std::fmt::Display) {
        self.skipped += 1;
        if self.skipped.is_power_of_two() || self.skipped.is_multiple_of(1000) {
            eprintln!(
                "{}: skipping an undecodable item (#{} so far): {error}",
                self.label, self.skipped
            );
        }
    }

    pub fn skipped(&self) -> u64 {
        self.skipped
    }
}
