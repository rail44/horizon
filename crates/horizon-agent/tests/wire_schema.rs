//! The **agent** wire-schema artifact's generator and drift check —
//! `docs/remoc-adoption-design.md` §4 rule 3. Every type that crosses
//! `horizon-agentd`'s socket derives `schemars::JsonSchema`; this test
//! regenerates one canonical schema document from those live types and
//! fails on any drift from the committed artifact at
//! `crates/horizon-agent/schema/agent-wire.json`. The result: every wire
//! change is visible, reviewable text in its PR diff, and forgetting to
//! regenerate is a red test. The merge-time additive-vs-reshape
//! classification of that diff is `scripts/check-wire-schema.sh`
//! (pre-commit), built on `horizon_wire::schema_check`.
//!
//! To regenerate after an intentional wire change (this artifact and its
//! terminal sibling in one go):
//!
//! ```sh
//! HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
//!     -p horizon-terminal-core wire_schema
//! ```
//!
//! ## One artifact per runtime
//!
//! Through v18 a single `session-wire.json` documented both daemons' hubs,
//! because they shared one version constant, one codec, and one negotiation
//! handshake — and that generator lived in `horizon-agentd`, dev-depending
//! on `horizon-terminal-core` purely to name the terminal types.
//! `docs/runtime-crate-alignment-design.md` phase 2 split it: the shared
//! half (codec, caps, `ClientHello`/`VersionRange`, `HubError`) is
//! `horizon-wire`'s and therefore appears in *both* artifacts, while each
//! hub's own surface is documented — and version-marked — beside the crate
//! that owns it. The inner keys are deliberately unchanged from the union,
//! section for section, so the two artifacts reassemble into exactly the
//! document that preceded them; that is how `scripts/check-wire-schema.sh`
//! classifies this very split against a pre-split merge-base.
//!
//! ## What this artifact documents
//!
//! The wire is the `SessionHub` rtc trait over remoc, not JSONL envelopes
//! (`docs/remoc-adoption-design.md` §2):
//!
//! - `hub`: every rtc method of `horizon-agentd`'s `SessionHub` mapped to
//!   its request/reply payload types (`hello`'s `ClientHello`→`HubHello`,
//!   the agent attach calls, `drain`, `reload_provider_config`). The
//!   channel-bearing reply structs (`HubHello`, `AgentAttachment`) carry
//!   remoc channel halves, which are chmux port references on the wire, not
//!   data — they appear here as opaque markers.
//! - `channels`: the vocabularies those channels carry
//!   (`AgentWireEvent`/agent `Command`, `HostToolRequest`/
//!   `HostToolResponse`, the startup `skipped_lines` diagnostic). This is
//!   where every `#[serde(other)] Unknown`-guarded command/event lives.
//!
//! `horizon-terminald`'s `terminal_hub` section and its terminal channels
//! are `crates/horizon-terminal-core/schema/terminal-wire.json`.
//!
//! ## Version history, inherited from the retired pin tests
//!
//! This check replaces the four `contract_version_*` pin tests of
//! `crates/horizon-agent/src/wire.rs`. The v4–v18 bump narrative lives on in
//! `AGENT_PROTOCOL_VERSION`'s own doc comment (terminal discovery/attach,
//! owned colors, dropped `Hello.capabilities`, frame styles/selection/
//! cursor shape, `SetColorScheme`, dropped `TerminalFrame.text`, the remoc
//! cutover, the terminald split, the config-only provider reload). From
//! here on the version stays put for additive changes — the checker
//! enforces exactly that — and a reshape demands an
//! `AGENT_PROTOCOL_VERSION` bump in the same change, which the artifact
//! carries as `x-session-protocol-version`.

use std::path::Path;

use schemars::generate::SchemaSettings;
use serde_json::{json, Value};

use horizon_agent::contract::{Command, Event, SessionId};
use horizon_agent::wire::{
    AgentAttachment, AgentWireEvent, HostToolRequest, HostToolResponse, HubHello, SessionNew,
    SessionSummary, AGENT_PROTOCOL_VERSION,
};
use horizon_wire::schema_check::{
    sort_object_keys, strip_unknown_catch_alls, PROTOCOL_VERSION_KEY,
};
use horizon_wire::{ClientHello, HubError};

const ARTIFACT_RELATIVE_PATH: &str = "schema/agent-wire.json";

/// One canonical document: the hub's method signatures, the channel
/// payload vocabularies, and every named type collected once under
/// `$defs`.
fn generate_wire_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    // The unit `()` request/reply of the argument-less / result-less
    // methods is documented as JSON `null` rather than a schema.
    let unit = json!({"type": "null"});

    // `horizon-agentd`'s hub: the agent domain.
    let hub = json!({
        "hello": {
            "request": generator.subschema_for::<ClientHello>().to_value(),
            "reply": generator.subschema_for::<HubHello>().to_value(),
            "error": generator.subschema_for::<HubError>().to_value(),
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
        "reload_provider_config": {
            "request": unit,
            "reply": unit,
        },
    });

    let channels = json!({
        "agent_events": generator.subschema_for::<AgentWireEvent>().to_value(),
        "agent_commands": generator.subschema_for::<Command>().to_value(),
        "agent_event_payload": generator.subschema_for::<Event>().to_value(),
        "host_tool_requests": generator.subschema_for::<HostToolRequest>().to_value(),
        "host_tool_responses": generator.subschema_for::<HostToolResponse>().to_value(),
        // `HubHello.skipped_lines` -- the startup event-log diagnostic
        // channel (a review-found omission: every channel's payload
        // vocabulary belongs here).
        "skipped_lines": generator.subschema_for::<String>().to_value(),
    });

    let mut defs = Value::Object(generator.take_definitions(true));
    strip_unknown_catch_alls(&mut defs);

    let mut schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "horizon-agent-wire",
        "$comment": "Generated from the live wire types (the SessionHub rtc trait and the \
                     vocabularies its channels carry). Regenerate with \
                     `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
                     -p horizon-terminal-core wire_schema`; additive-vs-reshape classification \
                     of changes is scripts/check-wire-schema.sh \
                     (docs/remoc-adoption-design.md §4).",
        PROTOCOL_VERSION_KEY: AGENT_PROTOCOL_VERSION,
        "hub": hub,
        "channels": channels,
        "$defs": defs,
    });
    sort_object_keys(&mut schema);
    schema
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
             -p horizon-agent -p horizon-terminal-core wire_schema",
            path.display()
        )
    });
    assert_eq!(
        committed, generated,
        "the committed agent wire-schema artifact is stale. A wire type changed shape; \
         regenerate with `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
         -p horizon-terminal-core wire_schema` and commit the artifact diff alongside the \
         change (scripts/check-wire-schema.sh classifies it as additive or reshape)."
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
        Some(&json!(AGENT_PROTOCOL_VERSION))
    );
}
