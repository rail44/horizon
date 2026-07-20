//! Quarantined legacy JSONL *encoder* — the one surviving piece of the
//! pre-v10 envelope wire (`docs/remoc-adoption-design.md` §6: "a small
//! legacy JSONL encoder … survives, quarantined in one module whose only
//! caller is the prober").
//!
//! Its single purpose: when a v10 (remoc) UI finds a still-running
//! JSONL-generation daemon on the socket — detected as a bounded-timeout
//! failure of the remoc connect, never the raw 60 s chmux timeout — PR
//! #18's contract-mismatch auto-recovery must be able to ask that daemon to
//! `Drain` *in the daemon's own dialect*: a versioned `session_control`
//! envelope on one newline-terminated JSON line. Pre-v9 daemons reject any
//! envelope whose `v` differs from their own before even looking at its
//! kind, so the prober walks candidate versions downward
//! ([`NEWEST_JSONL_VERSION`] → [`OLDEST_DRAINABLE_VERSION`]); a probe at
//! the wrong version is harmless (the stale daemon logs a malformed
//! message and closes that one connection), and a probe at the right one
//! drains it.
//!
//! Encoder only, by design: the prober never reads a reply (it observes
//! success as the socket refusing connections), so none of the decode
//! machinery survives.

/// The newest protocol version that spoke JSONL — the prober's first
/// candidate when the stale daemon never revealed its version (a pre-v9
/// daemon closes a foreign hello without replying; a v9 daemon is
/// equally unable to answer a chmux handshake, so from v10's side both
/// look the same: silence).
pub const NEWEST_JSONL_VERSION: u32 = 9;

/// The earliest protocol version whose `horizon-sessiond` honors a
/// pre-hello `SessionControl::Drain` (that handling landed together with
/// terminal hosting, in the v3 vocabulary). Daemons older than that
/// predate the `Drain` control entirely, so probing below it is
/// pointless.
pub const OLDEST_DRAINABLE_VERSION: u32 = 3;

/// One `SessionControl::Drain` envelope at `version`, as the JSONL wire
/// framed it: `{"v":N,"session_id":null,"kind":"session_control",
/// "payload":"drain"}` plus the terminating newline. Field order and the
/// unit-variant string encoding (`"drain"`) match what
/// `serde_json::to_string` produced for the retired `Envelope` /
/// `SessionControl` types byte-for-byte, so every JSONL-generation daemon
/// decodes it exactly as it always did.
pub fn drain_line(version: u32) -> String {
    format!("{{\"v\":{version},\"session_id\":null,\"kind\":\"session_control\",\"payload\":\"drain\"}}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder's output must parse as the JSON the JSONL daemons
    /// expect — guarded structurally so a stray format-string edit cannot
    /// silently break the one cross-generation recovery path.
    #[test]
    fn drain_line_is_one_terminated_json_line_in_the_envelope_shape() {
        let line = drain_line(7);
        assert!(line.ends_with('\n'));
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["v"], 7);
        assert_eq!(value["session_id"], serde_json::Value::Null);
        assert_eq!(value["kind"], "session_control");
        assert_eq!(value["payload"], "drain");
    }
}
