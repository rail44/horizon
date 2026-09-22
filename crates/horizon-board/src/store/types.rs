//! The vocabulary every store source shares: where a write places an item,
//! what a list returns, and how a store call fails.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::Item;

/// Where to place a new or moved item in the rank queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Position {
    Top,
    After(u64),
    Before(u64),
    Bottom,
}

/// The result of `list`: filtered items, the full set of statuses seen
/// across all items (for vocabulary-drift visibility), and an optional
/// skipped-line summary from the tolerant reader.
#[derive(Debug)]
pub struct ListResult {
    pub items: Vec<Item>,
    pub statuses: Vec<String>,
    pub skipped: Option<String>,
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    ItemNotFound(u64),
    RankExhausted,
    NotInGitRepo,
    /// The store's source accepts no writes: its envelopes are fixed at
    /// construction (`Store::in_memory`), so nothing was sent anywhere.
    ReadOnly,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Json(e) => write!(f, "{e}"),
            Self::ItemNotFound(id) => write!(f, "item {id} not found"),
            Self::RankExhausted => write!(f, "rank space exhausted (rebalance needed)"),
            Self::NotInGitRepo => write!(f, "not inside a git repository"),
            Self::ReadOnly => write!(f, "this store is read-only"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
