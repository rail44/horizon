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
//! crate that hosts the artifact) because the daemon is the one crate that
//! already depends on every wire vocabulary: the shared
//! `horizon-session-protocol` envelope plus the agent and terminal sister
//! vocabularies, which deliberately never reference each other
//! (`docs/session-daemon-design.md` decision 3).
//!
//! ## Version history, inherited from the retired pin tests
//!
//! This check replaces the four `contract_version_*` pin tests of
//! `crates/horizon-agent/src/wire.rs` — hand-maintained
//! `assert_eq!(CONTRACT_VERSION, 9)`s whose doc comments recorded, change
//! by change, whether a wire edit was a bump or additive. That narrative
//! lives on in two places: `SESSION_PROTOCOL_VERSION`'s own doc comment
//! (the v4–v9 bump-by-bump history — terminal discovery/attach, owned
//! colors, dropped `Hello.capabilities`, frame styles/selection/cursor
//! shape, `SetColorScheme`, dropped `TerminalFrame.text`), and the
//! additive precedents the pin tests defended (`SessionNew.workspace_root`,
//! the lineage fields, `Control::WorkspaceRootResolved`) which are now
//! simply *visible* as `#[serde(default)]` optional properties and trailing
//! variants in the artifact itself. From here on, v9 stays put for
//! additive changes — the checker enforces exactly that — and a reshape
//! demands a `SESSION_PROTOCOL_VERSION` bump in the same change, which the
//! artifact carries as `x-session-protocol-version`.

use std::path::Path;

use schemars::generate::SchemaSettings;
use serde_json::{json, Map, Value};

use horizon_agent::contract::{Command, Event};
use horizon_agent::wire::{Control, AGENT_COMMAND_KIND, AGENT_CONTROL_KIND, AGENT_EVENT_KIND};
use horizon_session_protocol::{
    schema_check::PROTOCOL_VERSION_KEY, Envelope, SessionControl, SESSION_CONTROL_KIND,
    SESSION_PROTOCOL_VERSION,
};
use horizon_terminal_core::{
    TerminalCommand, TerminalControl, TerminalUpdate, TERMINAL_COMMAND_KIND, TERMINAL_CONTROL_KIND,
    TERMINAL_UPDATE_KIND,
};

const ARTIFACT_RELATIVE_PATH: &str = "../horizon-session-protocol/schema/session-wire.json";

/// One canonical document: the shared envelope, plus each envelope `kind`
/// mapped to the payload vocabulary that decodes it, with every named type
/// collected once under `$defs`.
fn generate_wire_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    let envelope = generator.subschema_for::<Envelope>().to_value();
    let kinds = [
        (
            SESSION_CONTROL_KIND,
            generator.subschema_for::<SessionControl>().to_value(),
        ),
        (
            AGENT_COMMAND_KIND,
            generator.subschema_for::<Command>().to_value(),
        ),
        (
            AGENT_EVENT_KIND,
            generator.subschema_for::<Event>().to_value(),
        ),
        (
            AGENT_CONTROL_KIND,
            generator.subschema_for::<Control>().to_value(),
        ),
        (
            TERMINAL_CONTROL_KIND,
            generator.subschema_for::<TerminalControl>().to_value(),
        ),
        (
            TERMINAL_COMMAND_KIND,
            generator.subschema_for::<TerminalCommand>().to_value(),
        ),
        (
            TERMINAL_UPDATE_KIND,
            generator.subschema_for::<TerminalUpdate>().to_value(),
        ),
    ]
    .into_iter()
    .map(|(kind, schema)| (kind.to_string(), schema))
    .collect::<Map<String, Value>>();

    let mut defs = Value::Object(generator.take_definitions(true));
    strip_unknown_catch_alls(&mut defs);

    let mut schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "horizon-session-wire",
        "$comment": "Generated from the live wire types. Regenerate with \
                     `HORIZON_BLESS_WIRE_SCHEMA=1 cargo nextest run -p horizon-sessiond \
                     wire_schema`; additive-vs-reshape classification of changes is \
                     scripts/check-wire-schema.sh (docs/remoc-adoption-design.md §4).",
        PROTOCOL_VERSION_KEY: SESSION_PROTOCOL_VERSION,
        "envelope": envelope,
        "kinds": kinds,
        "$defs": defs,
    });
    sort_object_keys(&mut schema);
    schema
}

/// Removes the deserialize-only `Unknown` catch-all branches (and the
/// `UnknownPayload` definition backing them) from the generated schema: the
/// artifact documents what a peer may *send*, and `Unknown` never legally
/// crosses the wire — see `horizon_session_protocol::UnknownPayload`.
/// Stripping also keeps the checker's appended-variant rule simple: a new
/// variant, declared above the catch-all in code, lands as a genuine
/// trailing element here.
fn strip_unknown_catch_alls(value: &mut Value) {
    // The branch is a `$ref` to `UnknownPayload`, possibly annotated with
    // the variant's own doc comment as `description` — match on the ref.
    let is_catch_all = |variant: &Value| {
        variant.get("$ref").and_then(Value::as_str) == Some("#/$defs/UnknownPayload")
    };
    match value {
        Value::Object(map) => {
            map.remove("UnknownPayload");
            for (key, child) in map.iter_mut() {
                if matches!(key.as_str(), "anyOf" | "oneOf") {
                    if let Value::Array(variants) = &mut *child {
                        variants.retain(|variant| !is_catch_all(variant));
                    }
                }
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

/// The artifact never advertises the deserialize-only catch-all: neither
/// the `UnknownPayload` definition nor any `$ref` to it survives
/// generation.
#[test]
fn generated_schema_contains_no_unknown_catch_all() {
    let schema = generate_wire_schema();
    let text = serde_json::to_string(&schema).unwrap();
    assert!(!text.contains("UnknownPayload"), "{text}");
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
