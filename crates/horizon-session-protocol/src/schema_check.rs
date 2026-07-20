//! Merge-time classifier for the committed wire-schema artifact — the
//! mechanical half of `docs/remoc-adoption-design.md` §4's skew
//! discipline ("additive only", rule 3), and the successor to the four
//! hand-maintained `CONTRACT_VERSION` pin tests that used to live in
//! `crates/horizon-agent/src/wire.rs` (§4 rule 4).
//!
//! The division of labor:
//!
//! - `crates/horizon-sessiond/tests/wire_schema.rs` regenerates the schema
//!   from the live wire types and fails on any drift from the committed
//!   artifact (`crates/horizon-session-protocol/schema/session-wire.json`),
//!   so every wire change is visible, reviewable text in its PR diff.
//! - `scripts/check-wire-schema.sh` (run by `hooks/pre-commit`) feeds this
//!   module the merge-base's copy of the artifact next to the current one;
//!   [`classify_schema_change`] then classifies every difference as
//!   *additive* (pass) or *reshape* (fail).
//!
//! What counts as additive, mirroring §4 rule 1:
//!
//! - a new definition, property, enum value, or `anyOf`/`oneOf` variant —
//!   provided new enum values and variants are **appended** (declaration
//!   order is wire-meaningful under index-based codecs like Postbag, the
//!   remoc target), and a new property is not also newly `required`;
//! - a dropped `required` entry (a field gaining `#[serde(default)]` is a
//!   pure receive-side loosening);
//! - annotation-only changes (`description`, `title`, `default`,
//!   `examples`, `deprecated`, `$comment`) — doc-comment editing must
//!   never read as a wire reshape.
//!
//! Everything else — removing or renaming anything, reordering or retyping,
//! new `required` entries, changed constraints — is a reshape, and fails
//! unless the same change bumps `SESSION_PROTOCOL_VERSION` (the artifact
//! embeds it as `x-session-protocol-version`; a differing value is the §4
//! "explicit version-bump marker" that waves the whole diff through, to be
//! judged by the owner in review instead of by this classifier).

use serde_json::{Map, Value};

/// The artifact key carrying [`crate::SESSION_PROTOCOL_VERSION`]. A change
/// to this value between the two compared schemas is the explicit
/// version-bump marker that legitimizes an otherwise-forbidden reshape.
pub const PROTOCOL_VERSION_KEY: &str = "x-session-protocol-version";

/// Annotation keys whose changes never affect what decodes on the wire.
const ANNOTATION_KEYS: [&str; 6] = [
    "$comment",
    "default",
    "deprecated",
    "description",
    "examples",
    "title",
];

/// The outcome of comparing two versions of the schema artifact. Every
/// difference lands in exactly one of the two buckets; an empty
/// `violations` means the change is additive and may land without a
/// version bump.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Classification {
    /// Human-readable descriptions of the additive changes found.
    pub additive: Vec<String>,
    /// Human-readable descriptions of the reshape violations found.
    pub violations: Vec<String>,
}

impl Classification {
    pub fn is_additive_only(&self) -> bool {
        self.violations.is_empty()
    }
}

/// Classifies every difference between two versions of the wire-schema
/// artifact as additive or reshape — see the module doc for the rules. If
/// the embedded [`PROTOCOL_VERSION_KEY`] differs between the two, the
/// change carries its version-bump marker and passes wholesale.
pub fn classify_schema_change(old: &Value, new: &Value) -> Classification {
    let mut result = Classification::default();
    if old.get(PROTOCOL_VERSION_KEY) != new.get(PROTOCOL_VERSION_KEY) {
        result.additive.push(format!(
            "{PROTOCOL_VERSION_KEY} changed ({} -> {}): version-bump marker present, \
             reshapes are the owner's call in review",
            old.get(PROTOCOL_VERSION_KEY).unwrap_or(&Value::Null),
            new.get(PROTOCOL_VERSION_KEY).unwrap_or(&Value::Null),
        ));
        return result;
    }
    diff_value("#", old, new, &mut result);
    result
}

fn diff_value(path: &str, old: &Value, new: &Value, out: &mut Classification) {
    if old == new {
        return;
    }
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => diff_object(path, old, new, out),
        _ => out
            .violations
            .push(format!("{path}: changed from `{old}` to `{new}` (reshape)")),
    }
}

fn diff_object(
    path: &str,
    old: &Map<String, Value>,
    new: &Map<String, Value>,
    out: &mut Classification,
) {
    for (key, old_value) in old {
        let child = format!("{path}/{key}");
        let Some(new_value) = new.get(key) else {
            if key == "required" {
                out.additive
                    .push(format!("{child}: every member became optional"));
            } else if ANNOTATION_KEYS.contains(&key.as_str()) {
                out.additive.push(format!("{child}: annotation removed"));
            } else {
                out.violations.push(format!("{child}: removed (reshape)"));
            }
            continue;
        };
        if old_value == new_value {
            continue;
        }
        match key.as_str() {
            "properties" | "$defs" | "definitions" => {
                diff_named_map(&child, old_value, new_value, out)
            }
            "required" => diff_required(&child, old_value, new_value, out),
            "enum" => diff_appended_values(&child, old_value, new_value, out),
            "anyOf" | "oneOf" => diff_appended_subschemas(&child, old_value, new_value, out),
            key if ANNOTATION_KEYS.contains(&key) => {
                out.additive.push(format!("{child}: annotation changed"));
            }
            _ => diff_value(&child, old_value, new_value, out),
        }
    }
    for (key, _) in new.iter().filter(|(key, _)| !old.contains_key(*key)) {
        let child = format!("{path}/{key}");
        if ANNOTATION_KEYS.contains(&key.as_str()) {
            out.additive.push(format!("{child}: annotation added"));
        } else {
            // A brand-new structural keyword — including a `required` array
            // appearing where there was none — tightens or reshapes what
            // decodes; only annotations may appear freely.
            out.violations
                .push(format!("{child}: added constraint (reshape)"));
        }
    }
}

/// `properties` / `$defs`: entries may be added (a new field, a new named
/// type), never removed; surviving entries are recursed into. A new
/// property is only additive because a *newly required* one is caught by
/// [`diff_required`] (or by the added-`required`-keyword rule) on the same
/// object.
fn diff_named_map(path: &str, old: &Value, new: &Value, out: &mut Classification) {
    let (Value::Object(old), Value::Object(new)) = (old, new) else {
        out.violations.push(format!(
            "{path}: expected an object on both sides (reshape)"
        ));
        return;
    };
    for (key, old_value) in old {
        let child = format!("{path}/{key}");
        match new.get(key) {
            None => out.violations.push(format!("{child}: removed (reshape)")),
            Some(new_value) => diff_value(&child, old_value, new_value, out),
        }
    }
    for (key, _) in new.iter().filter(|(key, _)| !old.contains_key(*key)) {
        out.additive.push(format!("{path}/{key}: added"));
    }
}

/// `required`: compared as string sets. Entries may be dropped (a field
/// gained `#[serde(default)]`), never added.
fn diff_required(path: &str, old: &Value, new: &Value, out: &mut Classification) {
    let (Value::Array(old), Value::Array(new)) = (old, new) else {
        out.violations
            .push(format!("{path}: expected an array on both sides (reshape)"));
        return;
    };
    for entry in new.iter().filter(|entry| !old.contains(entry)) {
        out.violations
            .push(format!("{path}: {entry} became required (reshape)"));
    }
    for entry in old.iter().filter(|entry| !new.contains(entry)) {
        out.additive
            .push(format!("{path}: {entry} no longer required"));
    }
}

/// `enum`: append-only, order-preserving — an inserted, removed, or
/// reordered value is a reshape (order is wire-meaningful under
/// index-based codecs).
fn diff_appended_values(path: &str, old: &Value, new: &Value, out: &mut Classification) {
    let (Value::Array(old), Value::Array(new)) = (old, new) else {
        out.violations
            .push(format!("{path}: expected an array on both sides (reshape)"));
        return;
    };
    if new.len() < old.len() {
        out.violations
            .push(format!("{path}: enum values removed (reshape)"));
        return;
    }
    for (index, (old_value, new_value)) in old.iter().zip(new).enumerate() {
        if old_value != new_value {
            out.violations.push(format!(
                "{path}/{index}: enum value changed or reordered, `{old_value}` -> `{new_value}` \
                 (reshape; new values append at the end)"
            ));
        }
    }
    for value in &new[old.len()..] {
        out.additive
            .push(format!("{path}: appended enum value {value}"));
    }
}

/// `anyOf` / `oneOf`: variants append at the end; surviving variants are
/// recursed into so an existing variant may itself evolve additively.
fn diff_appended_subschemas(path: &str, old: &Value, new: &Value, out: &mut Classification) {
    let (Value::Array(old), Value::Array(new)) = (old, new) else {
        out.violations
            .push(format!("{path}: expected an array on both sides (reshape)"));
        return;
    };
    if new.len() < old.len() {
        out.violations
            .push(format!("{path}: variants removed (reshape)"));
        return;
    }
    for (index, (old_value, new_value)) in old.iter().zip(new).enumerate() {
        diff_value(&format!("{path}/{index}"), old_value, new_value, out);
    }
    for _ in &new[old.len()..] {
        out.additive.push(format!("{path}: appended variant"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assert_additive(old: Value, new: Value) {
        let result = classify_schema_change(&old, &new);
        assert!(
            result.is_additive_only(),
            "expected additive, got violations: {:?}",
            result.violations
        );
    }

    fn assert_reshape(old: Value, new: Value) {
        let result = classify_schema_change(&old, &new);
        assert!(
            !result.is_additive_only(),
            "expected a reshape violation, got additive: {:?}",
            result.additive
        );
    }

    #[test]
    fn identical_schemas_are_additive() {
        let schema = json!({"x-session-protocol-version": 9, "$defs": {"A": {"type": "string"}}});
        assert_additive(schema.clone(), schema);
    }

    #[test]
    fn a_new_optional_property_is_additive() {
        assert_additive(
            json!({"$defs": {"S": {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]}}}),
            json!({"$defs": {"S": {"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": ["string", "null"], "default": null}}, "required": ["a"]}}}),
        );
    }

    #[test]
    fn a_new_required_property_is_a_reshape() {
        assert_reshape(
            json!({"$defs": {"S": {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]}}}),
            json!({"$defs": {"S": {"type": "object", "properties": {"a": {"type": "string"}, "b": {"type": "string"}}, "required": ["a", "b"]}}}),
        );
    }

    #[test]
    fn dropping_a_required_entry_is_additive() {
        assert_additive(
            json!({"$defs": {"S": {"required": ["a", "b"]}}}),
            json!({"$defs": {"S": {"required": ["a"]}}}),
        );
    }

    #[test]
    fn removing_a_property_is_a_reshape() {
        assert_reshape(
            json!({"$defs": {"S": {"properties": {"a": {"type": "string"}, "b": {"type": "string"}}}}}),
            json!({"$defs": {"S": {"properties": {"a": {"type": "string"}}}}}),
        );
    }

    #[test]
    fn an_appended_enum_value_is_additive_but_insertion_is_a_reshape() {
        let old = json!({"$defs": {"E": {"type": "string", "enum": ["a", "b"]}}});
        assert_additive(
            old.clone(),
            json!({"$defs": {"E": {"type": "string", "enum": ["a", "b", "c"]}}}),
        );
        assert_reshape(
            old.clone(),
            json!({"$defs": {"E": {"type": "string", "enum": ["a", "c", "b"]}}}),
        );
        assert_reshape(
            old,
            json!({"$defs": {"E": {"type": "string", "enum": ["a"]}}}),
        );
    }

    #[test]
    fn an_appended_variant_is_additive_and_a_removed_variant_is_a_reshape() {
        let old = json!({"$defs": {"E": {"anyOf": [{"type": "string"}, {"type": "object"}]}}});
        assert_additive(
            old.clone(),
            json!({"$defs": {"E": {"anyOf": [{"type": "string"}, {"type": "object"}, {"type": "integer"}]}}}),
        );
        assert_reshape(
            old,
            json!({"$defs": {"E": {"anyOf": [{"type": "string"}]}}}),
        );
    }

    #[test]
    fn an_existing_variant_may_itself_evolve_additively() {
        assert_additive(
            json!({"$defs": {"E": {"anyOf": [{"type": "object", "properties": {"a": {}}}]}}}),
            json!({"$defs": {"E": {"anyOf": [{"type": "object", "properties": {"a": {}, "b": {}}}]}}}),
        );
    }

    #[test]
    fn retyping_a_field_is_a_reshape() {
        assert_reshape(
            json!({"$defs": {"S": {"properties": {"a": {"type": "string"}}}}}),
            json!({"$defs": {"S": {"properties": {"a": {"type": "integer"}}}}}),
        );
    }

    #[test]
    fn a_new_definition_is_additive() {
        assert_additive(
            json!({"$defs": {"A": {"type": "string"}}}),
            json!({"$defs": {"A": {"type": "string"}, "B": {"type": "integer"}}}),
        );
    }

    #[test]
    fn a_removed_definition_is_a_reshape() {
        assert_reshape(
            json!({"$defs": {"A": {"type": "string"}, "B": {"type": "integer"}}}),
            json!({"$defs": {"A": {"type": "string"}}}),
        );
    }

    #[test]
    fn description_churn_is_additive() {
        assert_additive(
            json!({"$defs": {"S": {"description": "old words", "type": "string"}}}),
            json!({"$defs": {"S": {"description": "new words", "type": "string"}}}),
        );
    }

    #[test]
    fn a_version_bump_marker_waves_a_reshape_through() {
        assert_additive(
            json!({"x-session-protocol-version": 9, "$defs": {"S": {"type": "string"}}}),
            json!({"x-session-protocol-version": 10, "$defs": {"S": {"type": "integer"}}}),
        );
        // ...but without the bump the same reshape fails.
        assert_reshape(
            json!({"x-session-protocol-version": 9, "$defs": {"S": {"type": "string"}}}),
            json!({"x-session-protocol-version": 9, "$defs": {"S": {"type": "integer"}}}),
        );
    }
}
