//! The control-plane envelope and its request/response vocabulary --
//! see `docs/cli-control-plane-design.md`'s "Contract" decision.
//!
//! This module only defines shapes; nothing here reads or writes bytes
//! (that's [`crate::wire`]) or knows about a Unix socket (that's the
//! future listener/CLI binary, out of scope for this crate).

use serde::{Deserialize, Serialize};

/// The control-plane vocabulary version this build speaks, carried in every
/// [`Envelope::v`] and checked structurally by [`crate::wire::read_envelope`]
/// -- an envelope from an incompatible wire format can't be assumed to parse
/// into today's [`EnvelopeBody`] shapes. This is a *separate* version from
/// `horizon-agent::wire::CONTRACT_VERSION`: workspace control and agent
/// session hosting are sibling contracts that evolve independently (see the
/// design doc's "Contract" decision).
///
/// [`Hello::control_version`] carries the same number again, but as a
/// semantic handshake value the receiving side is free to compare with its
/// own policy (e.g. reject) -- the two checks are kept separate, mirroring
/// `horizon-agent::wire`'s `CONTRACT_VERSION`/`Hello::contract_version`
/// split.
pub const CONTROL_VERSION: u32 = 1;

/// One newline-delimited control-plane message: `{"v":1,"id":..,"kind":..,
/// "payload":..}`. `id` is chosen by the client and echoed back by the
/// server on the matching response, for request/response correlation --
/// the same mechanism a future pipelined or subscription-upgraded
/// connection (deferred to v2, see the design doc) would reuse, so it is
/// part of the envelope from v1 rather than added later as a breaking
/// change.
///
/// A single envelope type carries both directions (requests and responses
/// share one `body` vocabulary, see [`EnvelopeBody`]) because a connection
/// only ever expects one direction's variants in each read -- same as
/// `horizon-agent::wire::Envelope` mixing client- and server-authored
/// `Control` messages on one wire.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Envelope {
    pub v: u32,
    pub id: u64,
    #[serde(flatten)]
    pub body: EnvelopeBody,
}

impl Envelope {
    /// Builds an envelope stamped with this build's [`CONTROL_VERSION`].
    pub fn new(id: u64, body: EnvelopeBody) -> Self {
        Self {
            v: CONTROL_VERSION,
            id,
            body,
        }
    }
}

/// The envelope's `kind`/`payload` pair, adjacently tagged
/// (`{"kind":"invoke","payload":{..}}`). Deserializing needs the version
/// check *before* picking which type to decode `payload` as, so reading is
/// hand-rolled in [`crate::wire::read_envelope`] rather than derived --
/// mirroring `horizon-agent::wire::EnvelopeBody`, which only derives
/// [`Serialize`] for the same reason.
///
/// Request kinds (client -> server) are [`EnvelopeBody::Hello`],
/// [`EnvelopeBody::Invoke`], [`EnvelopeBody::Query`]. Response kinds
/// (server -> client) are [`EnvelopeBody::HelloAck`],
/// [`EnvelopeBody::Rejected`], [`EnvelopeBody::Ok`],
/// [`EnvelopeBody::Error`], [`EnvelopeBody::Sessions`],
/// [`EnvelopeBody::State`].
///
/// [`EnvelopeBody::Unknown`] is this contract's forward-compatibility
/// escape hatch: an envelope whose `kind` this build doesn't recognize is
/// *not* a hard read error (unlike a structural [`crate::wire::WireError`])
/// -- it decodes to `Unknown` and is handed back to the caller, which can
/// ignore it or log it. This lets a newer peer add body kinds (e.g. a
/// future v2 subscription event) without breaking an older peer's ability
/// to keep reading the rest of the stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EnvelopeBody {
    Hello(Hello),
    Invoke(Invoke),
    Query(Query),
    HelloAck(HelloAck),
    Rejected(Rejected),
    /// A successful [`Invoke`] with nothing to return.
    Ok,
    Error(ErrorMessage),
    Sessions(Sessions),
    State(State),
    /// See [`EnvelopeBody`]'s doc comment. Never constructed by
    /// [`Envelope::new`] callers on purpose -- only
    /// [`crate::wire::read_envelope`] produces this, as the fallback for an
    /// unrecognized `kind`.
    Unknown {
        kind: String,
        payload: serde_json::Value,
    },
}

/// Must be the first message sent on a new connection (design doc's
/// "Contract" decision). A `control_version` the server can't honor gets a
/// [`Rejected`] response and the server closes the connection -- that
/// semantic check is separate from the structural `v` field check
/// `crate::wire::read_envelope` performs on every envelope (see
/// [`CONTROL_VERSION`]'s doc comment).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub control_version: u32,
    pub binary_id: String,
}

/// Runs an external command by its stable, kebab-case name. `args`
/// interpretation is entirely the app side's responsibility (the mapping
/// table from external names to internal `CommandId`s, per the design
/// doc's "Command exposure" decision) -- this contract only carries the
/// value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Invoke {
    pub command: String,
    pub args: serde_json::Value,
}

/// Asks for a read-only snapshot. `what` is `"sessions"` or `"state"` (see
/// [`EnvelopeBody::Sessions`]/[`EnvelopeBody::State`]); unrecognized
/// values are an app-side concern (e.g. an [`EnvelopeBody::Error`] reply),
/// not something this contract enumerates as a closed type -- new query
/// names should be addable without a contract version bump.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub what: String,
}

/// The server's reply to a successful [`Hello`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelloAck {
    pub control_version: u32,
    pub binary_id: String,
    pub capabilities: Vec<String>,
}

/// The server's reply to a [`Hello`] it can't honor (currently: a
/// `control_version` mismatch). The connection is closed after this is
/// sent, per the design doc's version-negotiation handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rejected {
    pub reason: String,
}

/// A failed [`Invoke`] or [`Query`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub message: String,
}

/// Reply to `Query { what: "sessions" }`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Sessions {
    pub sessions: Vec<SessionEntry>,
}

/// One session, as reshaped from the app side's own summaries -- a plain
/// transport DTO, deliberately not the app's internal session type (see
/// this crate's `lib.rs` doc comment).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_id: String,
    pub kind: String,
    pub attached: bool,
    pub title: String,
}

/// Reply to `Query { what: "state" }` -- a snapshot equivalent to the app's
/// internal command-enablement state (`tab_count`, `visible_pane_count`,
/// `has_active_session`, `detached_session_count`, `has_pending_approval`,
/// `has_turn_in_flight`), plus the stable external names of every command
/// currently marked destructive (`destructive_commands`) so a CLI front-end
/// can prompt for confirmation before invoking one, per the design doc's
/// "Authorization" decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub tab_count: usize,
    pub visible_pane_count: usize,
    pub has_active_session: bool,
    pub detached_session_count: usize,
    pub has_pending_approval: bool,
    pub has_turn_in_flight: bool,
    pub destructive_commands: Vec<String>,
}
