//! Durable planning and execution over ordinary board items.

mod planning;
mod scheduling;
mod state;
mod types;

pub use scheduling::{eligible_work, ordered_items, scopes_conflict, task_waits_for_decision};
pub use state::apply;
pub use types::*;

fn nonempty(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("Missing {name}"))
    } else {
        Ok(())
    }
}

fn nonempty_list(values: &[String], name: &str) -> Result<(), String> {
    if values.is_empty() || values.iter().any(|s| s.trim().is_empty()) {
        Err(format!("Provide nonempty {name}"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
