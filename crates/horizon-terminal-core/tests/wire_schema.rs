//! The **terminal** wire-schema artifact's generator and drift check — the
//! twin of `crates/horizon-agent/tests/wire_schema.rs`, and the reason the
//! terminal slice's append-only discipline
//! (`docs/terminald-split-design.md` decision 5) is diff-visible on its own.
//! Every type that crosses `horizon-terminald`'s socket derives
//! `schemars::JsonSchema`; this test regenerates the document from those
//! live types and fails on any drift from
//! `crates/horizon-terminal-core/schema/terminal-wire.json`.
//!
//! To regenerate after an intentional wire change (both artifacts in one
//! go):
//!
//! ```sh
//! HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
//!     -p horizon-terminal-core wire_schema
//! ```
//!
//! ## What this artifact documents
//!
//! - `terminal_hub`: every rtc method of `TerminalHub` mapped to its
//!   request/reply payload types (`hello`'s `ClientHello`→`TerminalHubHello`,
//!   `list_terminals`, `create_terminal`, `attach_terminal`, `drain`). Its
//!   own section, and since phase 2 its own file, because it is a separate
//!   trait served by a separate process on a separate socket since v17 —
//!   and now carries its own version pair, so an agent-side bump no longer
//!   drains this daemon's PTYs.
//!   `TerminalAttachment` carries remoc channel halves, which are chmux
//!   port references on the wire, not data — it appears here as an opaque
//!   marker.
//! - `channels`: the vocabularies those channels carry (`TerminalFrame` on
//!   the v11 frame watch, `TerminalUpdate` events, `TerminalCommand`).
//!
//! The negotiation half (`ClientHello`, `VersionRange`, `HubError`) is
//! `horizon-wire`'s and appears in the agent artifact too — identically, by
//! construction: both generators share `horizon_wire::schema_check`'s
//! sort helper, so the shared `$defs` stay byte-comparable and the two
//! documents still reassemble into the pre-split union.

use std::path::Path;

use schemars::generate::SchemaSettings;
use serde_json::{json, Value};

use horizon_terminal_core::wire::{
    TerminalAttachment, TerminalHubHello, TERMINAL_PROTOCOL_VERSION,
};
use horizon_terminal_core::{
    TerminalCommand, TerminalFrame, TerminalSpawnSpec, TerminalSummary, TerminalUpdate,
};
use horizon_wire::schema_check::{sort_object_keys, PROTOCOL_VERSION_KEY};
use horizon_wire::{ClientHello, HubError};

const ARTIFACT_RELATIVE_PATH: &str = "schema/terminal-wire.json";

/// One canonical document: the hub's method signatures, the channel
/// payload vocabularies, and every named type collected once under
/// `$defs`.
fn generate_wire_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    // The unit `()` request/reply of the argument-less / result-less
    // methods is documented as JSON `null` rather than a schema.
    let unit = json!({"type": "null"});

    // `horizon-terminald`'s hub: the terminal domain, on its own socket.
    let terminal_hub = json!({
        "hello": {
            "request": generator.subschema_for::<ClientHello>().to_value(),
            "reply": generator.subschema_for::<TerminalHubHello>().to_value(),
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
        "drain": {
            "request": unit,
            "reply": unit,
        },
    });

    let channels = json!({
        // Since v11 the frame path is an `rch::watch<TerminalFrame>` (full
        // frames, §5 Option A); the non-frame updates ride the events mpsc.
        "terminal_frames": generator.subschema_for::<TerminalFrame>().to_value(),
        "terminal_events": generator.subschema_for::<TerminalUpdate>().to_value(),
        "terminal_commands": generator.subschema_for::<TerminalCommand>().to_value(),
    });

    let defs = Value::Object(generator.take_definitions(true));

    let mut schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "horizon-terminal-wire",
        "$comment": "Generated from the live wire types (the TerminalHub rtc trait and the \
                     vocabularies its channels carry). Regenerate with \
                     `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
                     -p horizon-terminal-core wire_schema`; additive-vs-reshape classification \
                     of changes is scripts/check-wire-schema.sh \
                     (docs/remoc-adoption-design.md §4).",
        PROTOCOL_VERSION_KEY: TERMINAL_PROTOCOL_VERSION,
        "terminal_hub": terminal_hub,
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
        "the committed terminal wire-schema artifact is stale. A wire type changed shape; \
         regenerate with `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
         -p horizon-terminal-core wire_schema` and commit the artifact diff alongside the \
         change (scripts/check-wire-schema.sh classifies it as additive or reshape). \
         Remember that this slice is append-only (docs/terminald-split-design.md \
         decision 5): a reshape here kills every live PTY on the next reload."
    );
}

/// The artifact carries the protocol version the checker keys its
/// version-bump escape hatch on.
#[test]
fn generated_schema_embeds_the_protocol_version() {
    let schema = generate_wire_schema();
    assert_eq!(
        schema.get(PROTOCOL_VERSION_KEY),
        Some(&json!(TERMINAL_PROTOCOL_VERSION))
    );
}
