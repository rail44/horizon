//! Built-in result bodies shared by execution and history projections. These
//! serialize inside ToolCallResult's JSON payload; lifecycle meaning stays in
//! its explicit outcome. Tool handlers never use JSON as mutable working state.
mod context;
mod filesystem;
mod process;
mod web;

pub(crate) use context::*;
pub use filesystem::*;
pub use process::{BashOutput, BashTermination};
pub(crate) use process::{TaskOutput, TaskReport};
pub(crate) use web::*;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolError {
    pub message: String,
}

/// Read a known body at the persisted JSON boundary. An invalid or unrelated
/// payload remains available as raw output; it must not fabricate tool facts.
pub fn decode<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}
