//! CLI wrapper around [`horizon_session_protocol::schema_check`] for the
//! pre-commit quality gate: `check_wire_schema <old.json> <new.json>`
//! classifies every difference between two versions of the wire-schema
//! artifact and exits non-zero if any change is a reshape without a
//! `SESSION_PROTOCOL_VERSION` bump. Invoked by `scripts/check-wire-schema.sh`
//! with the merge-base's copy of the artifact beside the working tree's.

use std::process::ExitCode;

use horizon_session_protocol::schema_check::classify_schema_change;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(old_path), Some(new_path)) = (args.next(), args.next()) else {
        eprintln!("usage: check_wire_schema <old-schema.json> <new-schema.json>");
        return ExitCode::FAILURE;
    };

    let old = match read_schema(&old_path) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("check_wire_schema: {old_path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let new = match read_schema(&new_path) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("check_wire_schema: {new_path}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let result = classify_schema_change(&old, &new);
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
             SESSION_PROTOCOL_VERSION bump in the same change (docs/remoc-adoption-design.md §4); \
             additive alternatives (a parallel `#[serde(default)]` field, an appended variant) \
             usually exist."
        );
        ExitCode::FAILURE
    }
}

fn read_schema(path: &str) -> Result<serde_json::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}
