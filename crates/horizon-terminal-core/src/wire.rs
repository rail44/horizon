//! The terminal runtime's hub — `horizon-terminald`'s whole rtc surface —
//! and the version pair it negotiates.
//!
//! Until `docs/runtime-crate-alignment-design.md` phase 2 this trait lived
//! in `horizon-session-protocol` next to the agent hub, sharing one
//! `SESSION_PROTOCOL_VERSION`. Under the lockstep policy that shared
//! constant is what made an agent-only wire addition (v18 was exactly one)
//! reject the running `horizon-terminald` too and auto-drain it — every PTY
//! dying for a change that never touched the terminal wire. Judgment 3
//! dissolved the union: this hub now lives in the runtime crate that owns
//! it, with its own version pair
//! ([`TERMINAL_PROTOCOL_VERSION`]/[`MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION`])
//! and its own schema artifact (`schema/terminal-wire.json`), while the
//! domain-free foundation stays in `horizon-wire` ([`ClientHello`],
//! [`VersionRange`], the codec pin, the size caps, and [`HubError`]).
//!
//! remoc appears in this crate for this module only: the emulation engine,
//! the session loop, and the vocabulary types stay serde-plain and
//! transport-free, exactly as `docs/remoc-adoption-design.md` §1's
//! exit-cost note requires.

use remoc::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use horizon_wire::{
    CappedReceiver, ClientHello, HubError, TERMINAL_EVENT_MAX_ITEM_BYTES, VersionRange, WireCodec,
    channel_schema,
};

use crate::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};

/// The terminal-daemon protocol version this build speaks.
///
/// The v4–v18 history of the single pre-split constant — including the
/// terminal-side bumps (v5's owned colors, v7's frame styles/selection/
/// cursor shape, v8's `SetColorScheme`, v9's dropped `TerminalFrame.text`,
/// v11's snapshot-valued frame path, v12's scrollback windowing, v13's
/// structured input, v17's carve-out of this very trait) — was recorded on
/// the agent hub's `AGENT_PROTOCOL_VERSION`, which the ACP cutover retired
/// (`docs/acp-agentd-design.md`; the text survives in the history of
/// `crates/horizon-agent/src/wire/hub.rs`). From 18 on this constant moves
/// on its own, and only terminal-side history is recorded here.
///
/// **This slice is append-only** (`docs/terminald-split-design.md` decision
/// 5): `horizon-terminald` is rarely restarted
/// — a running one keeps its PTYs across every `Reload Agent Runtime` — so a
/// reshape of [`TerminalHub`], [`TerminalAttachment`], or this crate's
/// vocabularies is a *heavy* change that kills every live shell on the next
/// `Reload Terminal Runtime`. Evolve by appending (new methods, new
/// `#[serde(default)]` fields, new variants before the `#[serde(other)]
/// Unknown` catch-all) and retire slots as tombstones rather than removing
/// them. Splitting the version pair is what finally makes that discipline
/// pay: an agent-side reshape no longer forces this number up.
///
/// Version 19: wire-only `Unknown` catch-alls removed; no decode compat
/// (owner decision 2026-08-03 — this is a personal project, so backward
/// compatibility is not carried by default).
///
/// Version 20: `TerminalUpdate::Notification` added — OSC 9/777
/// desktop-notification requests extracted from the PTY stream
/// (`contract::TerminalNotification`).
///
/// Version 21: `TerminalUpdate::ProgramStatus` added — what a program in
/// the terminal says of its own state over OSC 7501
/// (`contract::TerminalProgramStatus`).
pub const TERMINAL_PROTOCOL_VERSION: u32 = 22;

/// The oldest terminal-wire version this build is still willing to
/// negotiate down to in [`TerminalHub::hello`] — the low end of the
/// advertised `[min_supported, current]` range.
///
/// Equal to [`TERMINAL_PROTOCOL_VERSION`] under the standing **lockstep, no
/// per-feature gates** policy (owner, 2026-07-30): same-machine
/// self-spawned daemons do not need cross-version interop, they need honest
/// restart, so a mismatched `hello` is rejected and recovered by the
/// client's auto-drain-and-respawn rather than bridged by gate constants.
/// The two such constants this wire once had (`SCROLLBACK_WINDOW_MIN_VERSION`
/// = 12, `TERMINAL_STRUCTURED_INPUT_VERSION` = 13) were deleted with the
/// phase-2 split: under a floor of 17+ their `>=` comparisons can never be
/// false (`docs/runtime-granularity-design.md` Q4). `TerminalCommand::
/// KeyInput` is sent unconditionally now — no per-keystroke gate survives
/// at all, versioned or otherwise (`src/terminal/session.rs`'s key-dispatch
/// path).
pub const MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION: u32 = 22;

/// The version range this build advertises in every `hello` to
/// `horizon-terminald`.
///
/// The range *type* is domain-free ([`VersionRange`]); which numbers go in
/// it is not, which is why this constructor sits beside this hub's own two
/// constants rather than on the type.
pub fn terminal_version_range() -> VersionRange {
    VersionRange::new(
        MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION,
        TERMINAL_PROTOCOL_VERSION,
    )
}

/// A [`ClientHello`] advertising [`terminal_version_range`] under
/// `binary_id` — what every client sends as the first call on a connection
/// to `horizon-terminald`.
pub fn terminal_client_hello(binary_id: impl Into<String>) -> ClientHello {
    ClientHello::new(terminal_version_range(), binary_id)
}

/// `horizon-terminald`'s `hello` reply. Channel-free: the terminal domain's
/// only streams are per-attachment ([`TerminalAttachment`]).
///
/// `binary_id` is load-bearing beyond diagnostics here — it is the terminald
/// connection's skew insurance (`docs/terminald-split-design.md` decision
/// 6). A terminald that outlives many UI rebuilds may be running an older
/// binary than the client that connects to it, and the layer *below* the
/// negotiated version (remoc/chmux framing, the Postbag codec) is not
/// covered by version negotiation at all — the tmux 3.6 lesson. The client
/// records this id at `hello` and names it if a post-`hello` decode failure
/// suggests exactly that kind of skew, so the failure surfaces as a clean
/// refusal pointing at `Reload Terminal Runtime` rather than as silent
/// misbehavior.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalHubHello {
    /// The highest mutually supported version.
    pub negotiated: u32,
    pub binary_id: String,
}

/// What [`TerminalHub::create_terminal`]/[`TerminalHub::attach_terminal`]
/// hand back: the session's live channels. `events` carries the
/// terminal's state and the bytes after it to a client that asks
/// (`TerminalCommand::WantState`, `docs/terminal-stream-design.md`), and
/// what the session says of itself (title, bell, clipboard, exit, error);
/// `commands` carries what the client sends. There are no frames: a client
/// draws from a terminal of its own.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct TerminalAttachment {
    #[schemars(schema_with = "channel_schema::<TerminalUpdate>")]
    pub events: CappedReceiver<TerminalUpdate, TERMINAL_EVENT_MAX_ITEM_BYTES>,
    #[schemars(schema_with = "channel_schema::<TerminalCommand>")]
    pub commands: rch::mpsc::Sender<TerminalCommand, WireCodec>,
}

/// The terminal hub — `horizon-terminald`'s whole rtc surface
/// (`docs/terminald-split-design.md` decision 1). Carved off the agent
/// daemon's hub in v17 so terminal hosting lives in its own rarely-restarted process:
/// `Reload Agent Runtime` drains the *agent* daemon and never touches a
/// PTY, while `Reload Terminal Runtime` is the explicit, destructive
/// counterpart for this one.
///
/// **Append-only from here on** (design decision 5): every
/// restart of the daemon serving this trait kills real interactive shells,
/// so a reshape is a heavy, user-visible change. New methods and new
/// `#[serde(default)]`/`Unknown`-guarded vocabulary go on the end; a retired
/// method becomes a tombstone that errors, never a hole. The version range
/// [`hello`](Self::hello) negotiates gates *behavior*.
#[rtc::remote]
pub trait TerminalHub {
    /// Version negotiation — the first call on every connection, and (with
    /// [`drain`](Self::drain)) the version-stable surface a
    /// range-rejected client may still use.
    async fn hello(&self, client: ClientHello) -> Result<TerminalHubHello, HubError>;

    /// Every live terminal session, sorted by id. Replaces the
    /// request-id-correlated `TerminalControl::List`/`ListResult` pair.
    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError>;

    /// Spawns a PTY for `session_id` and attaches to it. The id is
    /// caller-chosen (the workspace model owns pane identity), exactly as
    /// the JSONL `Create` control's envelope `session_id` was.
    async fn create_terminal(
        &self,
        session_id: Uuid,
        spec: TerminalSpawnSpec,
    ) -> Result<TerminalAttachment, HubError>;

    /// Attaches to an already-running terminal session. A client that
    /// shows it asks for its state (`TerminalCommand::WantState`).
    async fn attach_terminal(&self, session_id: Uuid) -> Result<TerminalAttachment, HubError>;

    /// Flush-and-exit: shuts every hosted terminal down and exits. The
    /// destructive half of `Reload Terminal Runtime`. The call itself
    /// typically errors because the process is gone before a reply can
    /// travel.
    async fn drain(&self) -> Result<(), HubError>;

    /// Gives every terminal to the daemon's next build: the daemon
    /// executes its executable again, keeping its PTYs, the programs in
    /// them and its listening socket, and hands each terminal's state to
    /// the new image (`docs/terminald-handover-design.md`). Like
    /// [`drain`](Self::drain) it is answered whatever version the client
    /// negotiated. Done, it is never answered: the connection closes with
    /// the old image. A reply means the terminals go on in this daemon.
    async fn hand_over(&self) -> Result<HandOverRefused, HubError>;
}

/// Why [`TerminalHub::hand_over`] left the terminals where they were.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HandOverRefused {
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_range_negotiates_with_itself_at_the_current_version() {
        assert_eq!(
            terminal_version_range().negotiate(terminal_version_range()),
            Some(TERMINAL_PROTOCOL_VERSION)
        );
    }

    /// The mirror of `horizon_agent::wire`'s own assertion: the two hubs'
    /// version pairs are independent constants but stay equal in practice
    /// (the phase-2 split started them equal, and the v19 `Unknown`-removal
    /// bump moved both in lockstep too). Neither crate may name the other,
    /// so each pins its half against the literal 20.
    #[test]
    fn the_split_started_at_the_pre_split_version() {
        assert_eq!(TERMINAL_PROTOCOL_VERSION, 22);
        assert_eq!(MIN_SUPPORTED_TERMINAL_PROTOCOL_VERSION, 22);
    }

    /// The fields the request enum reads for `method`: what serde is asked
    /// for when it reads that variant. (Read so, not by reading a request
    /// written out: the reply channel's own shape is remoc's.)
    fn fields_of(method: &'static str) -> Vec<&'static str> {
        use serde::de::{self, IntoDeserializer};

        #[derive(Debug)]
        struct Stop(Option<&'static [&'static str]>, String);
        impl std::fmt::Display for Stop {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(&self.1)
            }
        }
        impl std::error::Error for Stop {}
        impl de::Error for Stop {
            fn custom<T: std::fmt::Display>(message: T) -> Self {
                Stop(None, message.to_string())
            }
        }

        struct Probe(&'static str);
        impl<'de> de::Deserializer<'de> for Probe {
            type Error = Stop;
            fn deserialize_any<V: de::Visitor<'de>>(self, _: V) -> Result<V::Value, Stop> {
                Err(de::Error::custom("not an enum"))
            }
            fn deserialize_enum<V: de::Visitor<'de>>(
                self,
                _: &'static str,
                _: &'static [&'static str],
                visitor: V,
            ) -> Result<V::Value, Stop> {
                visitor.visit_enum(self)
            }
            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
                bytes byte_buf option unit unit_struct newtype_struct seq tuple
                tuple_struct map struct identifier ignored_any
            }
        }
        impl<'de> de::EnumAccess<'de> for Probe {
            type Error = Stop;
            type Variant = Self;
            fn variant_seed<V: de::DeserializeSeed<'de>>(
                self,
                seed: V,
            ) -> Result<(V::Value, Self), Stop> {
                let name: de::value::StrDeserializer<Stop> = self.0.into_deserializer();
                Ok((seed.deserialize(name)?, self))
            }
        }
        impl<'de> de::VariantAccess<'de> for Probe {
            type Error = Stop;
            fn unit_variant(self) -> Result<(), Stop> {
                Err(de::Error::custom("a unit variant"))
            }
            fn newtype_variant_seed<T: de::DeserializeSeed<'de>>(
                self,
                _: T,
            ) -> Result<T::Value, Stop> {
                Err(de::Error::custom("a newtype variant"))
            }
            fn tuple_variant<V: de::Visitor<'de>>(self, _: usize, _: V) -> Result<V::Value, Stop> {
                Err(de::Error::custom("a tuple variant"))
            }
            fn struct_variant<V: de::Visitor<'de>>(
                self,
                fields: &'static [&'static str],
                _: V,
            ) -> Result<V::Value, Stop> {
                Err(Stop(Some(fields), String::new()))
            }
        }

        use serde::Deserialize;
        match TerminalHubReqRef::<WireCodec>::deserialize(Probe(method)) {
            Err(Stop(Some(fields), _)) => fields.to_vec(),
            Err(Stop(None, message)) => panic!("{method}: {message}"),
            Ok(_) => panic!("{method}: read from nothing"),
        }
    }

    /// [`TerminalHub`]'s mechanical method-surface snapshot — the guard
    /// behind the v17 split's central claim that the terminal methods moved
    /// *verbatim*: the same names, the same argument names, on the daemon
    /// that now owns them. Read off the serde shape of the rtc macro's
    /// generated request enum, so renaming a method or an argument goes red
    /// and the schema artifact cannot silently drift from the real trait.
    #[test]
    fn terminal_hub_request_enum_matches_the_documented_method_surface() {
        let variants =
            match serde_json::from_str::<TerminalHubReqRef<WireCodec>>("{\"__bogus\":null}") {
                Ok(_) => panic!("a bogus variant must fail"),
                Err(error) => error.to_string(),
            };
        assert_eq!(
            variants,
            "unknown variant `__bogus`, expected one of `Hello`, `ListTerminals`, \
             `CreateTerminal`, `AttachTerminal`, `Drain`, `HandOver` at line 1 column 10",
        );

        // Each method's arguments, by name and in order, after the reply
        // channel the macro puts first.
        assert_eq!(fields_of("Hello"), ["__reply_tx", "client"]);
        assert_eq!(fields_of("ListTerminals"), ["__reply_tx"]);
        assert_eq!(
            fields_of("CreateTerminal"),
            ["__reply_tx", "session_id", "spec"]
        );
        assert_eq!(fields_of("AttachTerminal"), ["__reply_tx", "session_id"]);
        assert_eq!(fields_of("Drain"), ["__reply_tx"]);
        assert_eq!(fields_of("HandOver"), ["__reply_tx"]);
    }
}
