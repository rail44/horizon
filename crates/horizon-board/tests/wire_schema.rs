//! The **log** wire-schema artifact's generator and drift check — the third
//! sibling of `crates/horizon-agent/tests/wire_schema.rs` and
//! `crates/horizon-terminal-core/tests/wire_schema.rs`. Every type that
//! crosses `horizon-logd`'s socket derives `schemars::JsonSchema`; this test
//! regenerates the document from those live types and fails on any drift from
//! `crates/horizon-board/schema/log-wire.json`.
//!
//! To regenerate after an intentional wire change (all three artifacts in one
//! go):
//!
//! ```sh
//! HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
//!     -p horizon-terminal-core -p horizon-board wire_schema
//! ```

use std::path::Path;

use schemars::generate::SchemaSettings;
use serde_json::{json, Value};

use horizon_board::wire::{
    IngestReply, IngestRequest, LogError, LogHubHello, LOG_PROTOCOL_VERSION,
};
use horizon_wire::schema_check::{sort_object_keys, PROTOCOL_VERSION_KEY};
use horizon_wire::{ClientHello, HubError};

const ARTIFACT_RELATIVE_PATH: &str = "schema/log-wire.json";

fn generate_wire_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    let unit = json!({"type": "null"});

    let log_hub = json!({
        "hello": {
            "request": generator.subschema_for::<ClientHello>().to_value(),
            "reply": generator.subschema_for::<LogHubHello>().to_value(),
            "error": generator.subschema_for::<HubError>().to_value(),
        },
        "ingest": {
            "request": {
                "path": generator.subschema_for::<String>().to_value(),
                "request": generator.subschema_for::<IngestRequest>().to_value(),
            },
            "reply": generator.subschema_for::<IngestReply>().to_value(),
            "error": generator.subschema_for::<LogError>().to_value(),
        },
        "drain": {
            "request": unit,
            "reply": unit,
        },
    });

    let defs = Value::Object(generator.take_definitions(true));

    let mut schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "horizon-log-wire",
        "$comment": "Generated from the live wire types (the LogHub rtc trait and the \
                     IngestRequest/IngestReply vocabularies). Regenerate with \
                     `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
                     -p horizon-terminal-core -p horizon-board wire_schema`; \
                     additive-vs-reshape classification of changes is \
                     scripts/check-wire-schema.sh (docs/remoc-adoption-design.md §4).",
        PROTOCOL_VERSION_KEY: LOG_PROTOCOL_VERSION,
        "log_hub": log_hub,
        "$defs": defs,
    });
    sort_object_keys(&mut schema);
    schema
}

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
             -p horizon-agent -p horizon-terminal-core -p horizon-board wire_schema",
            path.display()
        )
    });
    assert_eq!(
        committed, generated,
        "the committed log wire-schema artifact is stale. A wire type changed shape; \
         regenerate with `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-agent \
         -p horizon-terminal-core -p horizon-board wire_schema` and commit the artifact \
         diff alongside the change (scripts/check-wire-schema.sh classifies it as \
         additive or reshape)."
    );
}

#[test]
fn generated_schema_embeds_the_protocol_version() {
    let schema = generate_wire_schema();
    assert_eq!(
        schema.get(PROTOCOL_VERSION_KEY),
        Some(&json!(LOG_PROTOCOL_VERSION))
    );
}
