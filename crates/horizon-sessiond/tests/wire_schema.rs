//! The wire-schema artifact's generator and drift check —
//! `docs/remoc-adoption-design.md` §4 rule 3. Every type that crosses the
//! session-daemon socket derives `schemars::JsonSchema`; this test
//! regenerates one canonical schema document from those live types and
//! fails on any drift from the committed artifact at
//! `crates/horizon-session-protocol/schema/session-wire.json`. The result:
//! every wire change is visible, reviewable text in its PR diff, and
//! forgetting to regenerate is a red test. The merge-time additive-vs-
//! reshape classification of that diff is `scripts/check-wire-schema.sh`
//! (pre-commit), built on `horizon_session_protocol::schema_check`.
//!
//! To regenerate after an intentional wire change:
//!
//! ```sh
//! HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-sessiond wire_schema
//! ```
//!
//! This generator lives in `horizon-sessiond` (rather than the protocol
//! crate that hosts the artifact) because the daemon is the one binary that
//! already links every wire vocabulary through `horizon-session-protocol`'s
//! own re-exports and the sister crates.
//!
//! ## What the v10 artifact documents
//!
//! The wire is the `SessionHub` rtc trait over remoc, not JSONL envelopes
//! (`docs/remoc-adoption-design.md` §2). The document therefore has two
//! sections instead of the old `envelope`+`kinds`:
//!
//! - `hub`: every rtc method mapped to its request/reply payload types
//!   (`hello`'s `ClientHello`→`HubHello`, the terminal/agent attach calls,
//!   `drain`). The channel-bearing reply structs (`HubHello`,
//!   `TerminalAttachment`, `AgentAttachment`) carry remoc channel halves,
//!   which are chmux port references on the wire, not data — they appear
//!   here as opaque markers.
//! - `channels`: the vocabularies those channels carry
//!   (`TerminalUpdate`/`TerminalCommand`, `AgentWireEvent`/agent `Command`,
//!   `HostToolRequest`/`HostToolResponse`). This is where the frame
//!   vocabulary (`Snapshot`/`FrameDiff`, unchanged in v10) and every
//!   `#[serde(other)] Unknown`-guarded command/event live.
//!
//! ## Version history, inherited from the retired pin tests
//!
//! This check replaces the four `contract_version_*` pin tests of
//! `crates/horizon-agent/src/wire.rs`. The v4–v9 bump narrative lives on in
//! `SESSION_PROTOCOL_VERSION`'s own doc comment (terminal discovery/attach,
//! owned colors, dropped `Hello.capabilities`, frame styles/selection/
//! cursor shape, `SetColorScheme`, dropped `TerminalFrame.text`), extended
//! by v10 (the remoc cutover). From here on the version stays put for
//! additive changes — the checker enforces exactly that — and a reshape
//! demands a `SESSION_PROTOCOL_VERSION` bump in the same change, which the
//! artifact carries as `x-session-protocol-version`.

use std::path::Path;

use schemars::generate::SchemaSettings;
use serde_json::{json, Value};

use horizon_agent::contract::{Command, Event, SessionId};
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, SessionNew, SessionSummary,
};
use horizon_session_protocol::{
    schema_check::PROTOCOL_VERSION_KEY, AgentAttachment, ClientHello, HubError, HubHello,
    TerminalAttachment, SESSION_PROTOCOL_VERSION,
};
use horizon_terminal_core::{TerminalCommand, TerminalSpawnSpec, TerminalSummary, TerminalUpdate};

const ARTIFACT_RELATIVE_PATH: &str = "../horizon-session-protocol/schema/session-wire.json";

/// One canonical document: the hub's method signatures, the channel
/// payload vocabularies, and every named type collected once under
/// `$defs`.
fn generate_wire_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    // The unit `()` request/reply of the argument-less / result-less
    // methods is documented as JSON `null` rather than a schema.
    let unit = json!({"type": "null"});

    let hub = json!({
        "hello": {
            "request": generator.subschema_for::<ClientHello>().to_value(),
            "reply": generator.subschema_for::<HubHello>().to_value(),
            "error": generator.subschema_for::<HubError>().to_value(),
        },
        "list_terminals": {
            "request": unit,
            "reply": generator.subschema_for::<Vec<TerminalSummary>>().to_value(),
        },
        "create_terminal": {
            "request": {
                "session_id": generator.subschema_for::<uuid::Uuid>().to_value(),
                "spec": generator.subschema_for::<TerminalSpawnSpec>().to_value(),
            },
            "reply": generator.subschema_for::<TerminalAttachment>().to_value(),
        },
        "attach_terminal": {
            "request": generator.subschema_for::<uuid::Uuid>().to_value(),
            "reply": generator.subschema_for::<TerminalAttachment>().to_value(),
        },
        "list_agents": {
            "request": unit,
            "reply": generator.subschema_for::<Vec<SessionSummary>>().to_value(),
        },
        "new_agent": {
            "request": generator.subschema_for::<SessionNew>().to_value(),
            "reply": generator.subschema_for::<AgentAttachment>().to_value(),
        },
        "attach_agent": {
            "request": generator.subschema_for::<SessionId>().to_value(),
            "reply": generator.subschema_for::<AgentAttachment>().to_value(),
        },
        "drain": {
            "request": unit,
            "reply": unit,
        },
    });

    let channels = json!({
        "terminal_updates": generator.subschema_for::<TerminalUpdate>().to_value(),
        "terminal_commands": generator.subschema_for::<TerminalCommand>().to_value(),
        "agent_events": generator.subschema_for::<AgentWireEvent>().to_value(),
        "agent_commands": generator.subschema_for::<Command>().to_value(),
        "agent_event_payload": generator.subschema_for::<Event>().to_value(),
        "host_tool_requests": generator.subschema_for::<HostToolRequest>().to_value(),
        "host_tool_responses": generator.subschema_for::<HostToolResponse>().to_value(),
    });

    let mut defs = Value::Object(generator.take_definitions(true));
    strip_unknown_catch_alls(&mut defs);

    let mut schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "horizon-session-wire",
        "$comment": "Generated from the live wire types (the SessionHub rtc trait and the \
                     vocabularies its channels carry). Regenerate with \
                     `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-sessiond \
                     wire_schema`; additive-vs-reshape classification of changes is \
                     scripts/check-wire-schema.sh (docs/remoc-adoption-design.md §4).",
        PROTOCOL_VERSION_KEY: SESSION_PROTOCOL_VERSION,
        "hub": hub,
        "channels": channels,
        "$defs": defs,
    });
    sort_object_keys(&mut schema);
    schema
}

/// Removes the `#[serde(other)] Unknown` skew catch-all from the generated
/// schema: the artifact documents what a peer may *send*, and `Unknown` is
/// never legally put on the wire (nothing constructs it on a send path).
/// Removing it also keeps the checker's appended-variant rule simple —
/// schemars renders a `#[serde(other)]` unit variant as a trailing
/// `{"const": "Unknown"}` `oneOf` branch (or, when it groups with other
/// unit variants, as a `"Unknown"` entry in an `enum` array); a newly
/// appended variant would otherwise read as "the branch that used to be
/// `Unknown` changed", a false reshape. With it stripped, a new variant
/// declared above the catch-all lands as a genuine trailing element.
fn strip_unknown_catch_alls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // A grouped unit-variant enum: drop the "Unknown" member.
            if let Some(Value::Array(items)) = map.get_mut("enum") {
                items.retain(|item| item.as_str() != Some("Unknown"));
            }
            for key in ["oneOf", "anyOf"] {
                if let Some(Value::Array(branches)) = map.get_mut(key) {
                    branches.retain(|branch| !is_unknown_catch_all(branch));
                }
            }
            for child in map.values_mut() {
                strip_unknown_catch_alls(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_unknown_catch_alls(item);
            }
        }
        _ => {}
    }
}

/// Whether a `oneOf`/`anyOf` branch is the standalone `Unknown` catch-all:
/// a `{"const": "Unknown"}` branch, or a `{"enum": ["Unknown"]}` branch
/// that carries nothing else.
fn is_unknown_catch_all(branch: &Value) -> bool {
    if branch.get("const").and_then(Value::as_str) == Some("Unknown") {
        return true;
    }
    matches!(
        branch.get("enum"),
        Some(Value::Array(items))
            if items.len() == 1 && items[0].as_str() == Some("Unknown")
    )
}

/// Byte-stable artifact output independent of `serde_json`'s map ordering
/// (feature unification may switch it to insertion order): object keys are
/// sorted recursively; arrays keep their order (variant/`required` order is
/// meaningful).
fn sort_object_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (key, mut child) in entries {
                sort_object_keys(&mut child);
                map.insert(key, child);
            }
        }
        Value::Array(items) => {
            for item in items {
                sort_object_keys(item);
            }
        }
        _ => {}
    }
}

/// The committed artifact must match what the live wire types generate.
/// Red here means a wire type changed without regenerating the artifact —
/// run the bless command in the module doc, then review the artifact diff
/// as part of the change (the pre-commit checker classifies it).
#[test]
fn committed_wire_schema_artifact_is_current() {
    let mut generated = serde_json::to_string_pretty(&generate_wire_schema()).unwrap();
    generated.push('\n');
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT_RELATIVE_PATH);

    if std::env::var_os("HORIZON_BLESS_WIRE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "failed to read the committed wire-schema artifact at {}: {error}\n\
             regenerate it with: HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run \
             -p horizon-sessiond wire_schema",
            path.display()
        )
    });
    assert_eq!(
        committed, generated,
        "the committed wire-schema artifact is stale. A wire type changed shape; \
         regenerate with `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-sessiond \
         wire_schema` and commit the artifact diff alongside the change \
         (scripts/check-wire-schema.sh classifies it as additive or reshape)."
    );
}

/// The artifact never advertises the deserialize-only catch-all: no
/// `{"const": "Unknown"}` branch survives generation.
#[test]
fn generated_schema_contains_no_unknown_catch_all() {
    let schema = generate_wire_schema();
    let text = serde_json::to_string(&schema).unwrap();
    assert!(
        !text.contains("\"const\":\"Unknown\""),
        "an Unknown catch-all branch leaked into the artifact: {text}"
    );
}

/// The artifact carries the protocol version the checker keys its
/// version-bump escape hatch on.
#[test]
fn generated_schema_embeds_the_protocol_version() {
    let schema = generate_wire_schema();
    assert_eq!(
        schema.get(PROTOCOL_VERSION_KEY),
        Some(&json!(SESSION_PROTOCOL_VERSION))
    );
}
