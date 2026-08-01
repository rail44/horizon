//! CLI wrapper around [`horizon_wire::schema_check`] for the
//! pre-commit quality gate. Two modes, both driven by
//! `scripts/check-wire-schema.sh`:
//!
//! - `check_wire_schema <old.json> <new.json>` — the steady state:
//!   classifies every difference between two versions of *one* artifact and
//!   exits non-zero if any change is a reshape without a protocol-version
//!   bump.
//! - `check_wire_schema --transition <base-union.json> <artifact.json>...`
//!   — the one-time arm for a branch whose merge-base still has the
//!   pre-split `session-wire.json`
//!   (`docs/runtime-crate-alignment-design.md` phase 2). It reassembles the
//!   new artifacts (however many the script discovered — two today) into
//!   the union document they were split out of and classifies *that*
//!   against the merge-base's copy, so the split itself is checked exactly
//!   as strictly as any other change: a section that quietly lost a
//!   definition, a channel, or a hub method still fails. Skipping the check
//!   because the new files "did not exist at the merge-base" would have
//!   been the dishonest alternative.
//!
//!   Reassembly is a plain key merge (each artifact's own top-level
//!   sections, plus a union of their `channels` and `$defs`), because the
//!   split was defined to keep every inner key identical. Two rules keep it
//!   from laundering a real change: a `$defs`/`channels` key present in
//!   more than one artifact with *different* content is reported as a
//!   violation rather than silently taking one side, and the merged
//!   `x-session-protocol-version` is the **minimum** across them, so a bump
//!   on one hub can never wave a reshape on another one through.
//!
//! Once no live branch predates the artifact split, the `--transition` arm
//! and its caller in the script can be deleted.

use std::process::ExitCode;

use horizon_wire::schema_check::{classify_schema_change, Classification, PROTOCOL_VERSION_KEY};
use serde_json::{Map, Value};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (old, new) = match refs.as_slice() {
        ["--transition", base, artifacts @ ..] if !artifacts.is_empty() => {
            let base = match read_schema(base) {
                Ok(value) => value,
                Err(error) => return fail_reading(base, &error),
            };
            let mut parsed = Vec::with_capacity(artifacts.len());
            for path in artifacts {
                match read_schema(path) {
                    Ok(value) => parsed.push(value),
                    Err(error) => return fail_reading(path, &error),
                }
            }
            let mut merge_violations = Vec::new();
            let merged = reassemble_union(&parsed, &mut merge_violations);
            if !merge_violations.is_empty() {
                for violation in &merge_violations {
                    println!("RESHAPE:  {violation}");
                }
                eprintln!(
                    "the wire-schema artifacts disagree about a shared definition, so \
                     they no longer reassemble into the document they were split out of. \
                     Fix the disagreement (every hub shares horizon-wire's types by \
                     construction) before this branch can be classified against its \
                     pre-split merge-base."
                );
                return ExitCode::FAILURE;
            }
            println!(
                "wire-schema: merge-base predates the artifact split; classifying the \
                 reassembled union against it"
            );
            (base, merged)
        }
        [old_path, new_path] => {
            let old = match read_schema(old_path) {
                Ok(value) => value,
                Err(error) => return fail_reading(old_path, &error),
            };
            let new = match read_schema(new_path) {
                Ok(value) => value,
                Err(error) => return fail_reading(new_path, &error),
            };
            (old, new)
        }
        _ => {
            eprintln!(
                "usage: check_wire_schema <old-schema.json> <new-schema.json>\n       \
                 check_wire_schema --transition <base-union.json> <artifact.json>..."
            );
            return ExitCode::FAILURE;
        }
    };

    report(classify_schema_change(&old, &new))
}

fn report(result: Classification) -> ExitCode {
    for change in &result.additive {
        println!("additive: {change}");
    }
    for violation in &result.violations {
        println!("RESHAPE:  {violation}");
    }
    if result.is_additive_only() {
        println!("wire schema change is additive-only");
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "wire schema reshape detected. Reshapes need an owner decision and a \
             protocol-version bump in the same change (docs/remoc-adoption-design.md §4); \
             additive alternatives (a parallel `#[serde(default)]` field, an appended variant) \
             usually exist."
        );
        ExitCode::FAILURE
    }
}

/// Rebuilds the pre-split `session-wire.json` shape from the artifacts that
/// replaced it: each contributes its own hub section, and their
/// `channels`/`$defs` objects are unioned. See the module doc for why the
/// version marker is the minimum and why a shared key with differing
/// content is a violation rather than a silent pick.
fn reassemble_union(sources: &[Value], violations: &mut Vec<String>) -> Value {
    let mut merged = Map::new();
    for source in sources {
        let Some(object) = source.as_object() else {
            violations.push("an artifact is not a JSON object".to_string());
            continue;
        };
        for (key, value) in object {
            match key.as_str() {
                "channels" | "$defs" => {
                    let slot = merged
                        .entry(key.clone())
                        .or_insert_with(|| Value::Object(Map::new()));
                    merge_named_map(key, value, slot, violations);
                }
                PROTOCOL_VERSION_KEY => {
                    // The conservative direction: a one-sided bump must not
                    // legitimize the other hub's reshape.
                    let lower = match (merged.get(key).and_then(Value::as_u64), value.as_u64()) {
                        (Some(seen), Some(candidate)) => Value::from(seen.min(candidate)),
                        _ => value.clone(),
                    };
                    merged.insert(key.clone(), lower);
                }
                _ => {
                    // Annotations and each artifact's own hub section. The
                    // `title`/`$comment` the two artifacts differ on are
                    // annotation keys the classifier already treats as
                    // non-structural, so taking the first is enough.
                    merged.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
    }
    Value::Object(merged)
}

fn merge_named_map(section: &str, source: &Value, into: &mut Value, violations: &mut Vec<String>) {
    let (Some(source), Some(into)) = (source.as_object(), into.as_object_mut()) else {
        violations.push(format!("#/{section}: expected an object in both artifacts"));
        return;
    };
    for (key, value) in source {
        match into.get(key) {
            Some(existing) if existing != value => violations.push(format!(
                "#/{section}/{key}: the two artifacts define it differently, so the split is \
                 not content-preserving"
            )),
            Some(_) => {}
            None => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

fn fail_reading(path: &str, error: &str) -> ExitCode {
    eprintln!("check_wire_schema: {path}: {error}");
    ExitCode::FAILURE
}

fn read_schema(path: &str) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}
