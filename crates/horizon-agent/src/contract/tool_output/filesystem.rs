use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRead {
    pub path: String,
    pub content_version: Option<String>,
    pub start_line: usize,
    pub end_line: usize,
    pub total_lines: usize,
    pub truncated: bool,
    pub next_offset: Option<usize>,
    pub content_chars: usize,
    pub notice: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Matches<T> {
    pub base_path: String,
    pub pattern: String,
    pub matches: Vec<T>,
    pub returned_count: usize,
    pub total_matches: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub line_number: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileWritten {
    pub path: String,
    pub bytes_written: usize,
    pub created: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileEdits {
    pub edits: Vec<EditReceipt>,
    pub applied_count: usize,
    pub file_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditReceipt {
    pub index: usize,
    pub path: String,
    #[serde(flatten)]
    pub outcome: EditOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EditOutcome {
    Applied { occurrences: usize },
    Failed { message: String },
    NotAttempted,
}
