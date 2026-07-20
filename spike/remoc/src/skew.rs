//! V1/V2 type pairs for the Postbag skew experiments (item 2).
//!
//! Modeled on the real evolution pattern of `TerminalFrame` /
//! `TerminalCommand`: fields get added over time, and `TerminalCommand`
//! grows new variants frequently.

use serde::{Deserialize, Serialize};

// ---- (a)/(b) struct field addition / removal --------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameMetaV1 {
    pub rows: u32,
    pub cols: u32,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameMetaV2 {
    pub rows: u32,
    pub cols: u32,
    pub title: String,
    /// Added in "V2" — receiver-side default when absent.
    #[serde(default)]
    pub zoom: Option<f32>,
    /// Added in "V2" without `Option`, plain `#[serde(default)]`.
    #[serde(default)]
    pub alt_screen: bool,
}

/// V2 variant whose added field has *no* `#[serde(default)]` — to check
/// whether Postbag itself tolerates the missing field or errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameMetaV2Strict {
    pub rows: u32,
    pub cols: u32,
    pub title: String,
    pub alt_screen: bool,
}

// ---- (c) enum variant addition ----------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandV1 {
    Key(String),
    Resize { rows: u32, cols: u32 },
    Paste(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandV2 {
    Key(String),
    Resize {
        rows: u32,
        cols: u32,
    },
    Paste(String),
    /// Added in "V2".
    Scroll(i32),
}

/// V1 with a `#[serde(other)]` catch-all — the receiver-side defense the
/// spike must validate for Postbag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CommandV1Defended {
    Key(String),
    Resize {
        rows: u32,
        cols: u32,
    },
    Paste(String),
    #[serde(other)]
    Unknown,
}
