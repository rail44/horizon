//! The ACP extension wire-schema artifact's generator and drift check: the
//! `_horizon/*` methods and the `_meta.horizon` payloads, regenerated from
//! the live types and compared against the committed
//! `crates/horizon-acp/schema/acp-ext-wire.json`.
//! `scripts/check-wire-schema.sh` classifies changes to it against the
//! merge-base.
//!
//! To regenerate after an intentional change:
//!
//! ```sh
//! HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-acp wire_schema
//! ```

use std::path::Path;

use schemars::generate::SchemaSettings;
use schemars::JsonSchema;
use serde_json::{json, Value};

use horizon_acp::*;
use horizon_wire::schema_check::{sort_object_keys, PROTOCOL_VERSION_KEY};

const ARTIFACT_RELATIVE_PATH: &str = "schema/acp-ext-wire.json";

fn generate_wire_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let mut schema_of = |generate: fn(&mut schemars::SchemaGenerator) -> schemars::Schema| {
        generate(&mut generator).to_value()
    };
    fn sub<T: JsonSchema>(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        generator.subschema_for::<T>()
    }

    let client_to_agent = json!({
        "_horizon/continue_turn": {
            "request": schema_of(sub::<ContinueTurnRequest>),
            "reply": schema_of(sub::<EmptyResponse>),
        },
        "_horizon/list_providers": {
            "request": schema_of(sub::<ListProvidersRequest>),
            "reply": schema_of(sub::<ListProvidersResponse>),
        },
        "_horizon/list_provider_models": {
            "request": schema_of(sub::<ListProviderModelsRequest>),
            "reply": schema_of(sub::<ListProviderModelsResponse>),
        },
        "_horizon/watch_board": {
            "request": schema_of(sub::<WatchBoardRequest>),
            "reply": schema_of(sub::<EmptyResponse>),
        },
        "_horizon/ensure_board_organizer": {
            "request": schema_of(sub::<EnsureBoardOrganizerRequest>),
            "reply": schema_of(sub::<EnsureBoardOrganizerResponse>),
        },
        "_horizon/reload_provider_config": {
            "request": schema_of(sub::<ReloadProviderConfigRequest>),
            "reply": schema_of(sub::<EmptyResponse>),
        },
        "_horizon/drain": {
            "request": schema_of(sub::<DrainRequest>),
            "reply": schema_of(sub::<EmptyResponse>),
        },
    });

    let agent_to_client = json!({
        "_horizon/host_tool": {
            "request": schema_of(sub::<HostToolRequest>),
            "reply": schema_of(sub::<HostToolResponse>),
        },
    });

    let notifications = json!({
        "_horizon/task_progress": schema_of(sub::<TaskProgressNotification>),
        "_horizon/tool_call_progress": schema_of(sub::<ToolCallProgressNotification>),
        "_horizon/memory": schema_of(sub::<MemoryNotification>),
        "_horizon/session_event": schema_of(sub::<SessionEventNotification>),
        "_horizon/provider_request": schema_of(sub::<ProviderRequestNotification>),
    });

    let meta = json!({
        "initialize": schema_of(sub::<InitializeMeta>),
        "session_new": schema_of(sub::<SessionNewMeta>),
        "session_info": schema_of(sub::<SessionInfoMeta>),
        "tool_call": schema_of(sub::<ToolCallMeta>),
        "request_permission": schema_of(sub::<ApprovalMeta>),
        "request_permission_response": schema_of(sub::<PermissionResponseMeta>),
        "message": schema_of(sub::<MessageMeta>),
        "config_option": schema_of(sub::<ModelOptionMeta>),
    });

    let stop_reasons = json!([STOP_REASON_FAILED, STOP_REASON_DOOM_LOOP]);

    let defs = Value::Object(generator.take_definitions(true));

    let mut schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "horizon-acp-ext-wire",
        "$comment": "Generated from the live horizon-acp extension types (the _horizon/* \
                     methods and the _meta.horizon payloads). Regenerate with \
                     `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-acp wire_schema`; \
                     additive-vs-reshape classification of changes is \
                     scripts/check-wire-schema.sh.",
        PROTOCOL_VERSION_KEY: HORIZON_ACP_EXT_VERSION,
        "client_to_agent": client_to_agent,
        "agent_to_client": agent_to_client,
        "notifications": notifications,
        "meta": meta,
        "stop_reasons": stop_reasons,
        "$defs": defs,
    });
    sort_object_keys(&mut schema);
    schema
}

/// The committed artifact must match what the live extension types
/// generate. Red here means a type changed without regenerating the
/// artifact — run the bless command in the module doc.
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
             -p horizon-acp wire_schema",
            path.display()
        )
    });
    assert_eq!(
        committed, generated,
        "the committed ACP extension wire-schema artifact is stale; regenerate with \
         `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-acp wire_schema` and \
         commit the artifact diff alongside the change."
    );
}

/// The artifact carries the extension version the checker keys its
/// version-bump escape hatch on.
#[test]
fn generated_schema_embeds_the_ext_version() {
    let schema = generate_wire_schema();
    assert_eq!(
        schema.get(PROTOCOL_VERSION_KEY),
        Some(&json!(HORIZON_ACP_EXT_VERSION))
    );
}
