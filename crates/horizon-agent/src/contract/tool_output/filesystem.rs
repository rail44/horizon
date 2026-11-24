use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileRead {
    /// The file that was read.
    pub path: String,
    /// The version the file was at when read, to pass back when editing.
    pub content_version: Option<String>,
    /// The first line number this window covers.
    pub start_line: usize,
    /// The last line it covers.
    pub end_line: usize,
    /// How many lines the file has in total.
    pub total_lines: usize,
    /// Whether the window stops before the end of the file.
    pub truncated: bool,
    /// The offset that continues this read, or absent at the end.
    pub next_offset: Option<usize>,
    /// How many characters of content this result carries, after the cap.
    pub content_chars: usize,
    /// An explanation of any detail the result could not carry.
    pub notice: Option<String>,
    /// The content itself.
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Matches<T> {
    /// The directory the search started from.
    pub base_path: String,
    /// The pattern that was searched for.
    pub pattern: String,
    /// The matches themselves.
    pub matches: Vec<T>,
    /// How many matches this result carries.
    pub returned_count: usize,
    /// How many exist in total, which exceeds `returned_count` when capped.
    pub total_matches: usize,
    /// Whether the cap dropped matches that exist.
    pub truncated: bool,
    /// An explanation of anything the result could not carry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Location {
    /// The file the match is in.
    pub path: String,
    /// The line it is on, one-based.
    pub line_number: usize,
    /// What the line says, cut at a fixed length (`truncated` says when).
    pub line: String,
    /// Whether `line` was cut short.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub line_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileWritten {
    /// The file that was written.
    pub path: String,
    /// How much content was written.
    pub bytes_written: usize,
    /// Whether the write created the file rather than replacing one.
    pub created: bool,
    /// The version the file is at after the write, as `FileRead` gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileEdits {
    /// One receipt per edit, in the order they were submitted.
    pub edits: Vec<EditReceipt>,
    /// How many edits were applied: all of them, or none when one failed.
    pub applied_count: usize,
    /// How many distinct files the edits touched.
    pub file_count: usize,
    /// The first edit that failed, when one did; then no edit was applied.
    pub failed_index: Option<usize>,
    /// What happened to the call, when it failed.
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditReceipt {
    /// The position of this edit in the request.
    pub index: usize,
    /// The file this edit targeted.
    pub path: String,
    #[serde(flatten)]
    pub outcome: EditOutcome,
    /// The version the file is at after the call wrote it, as `FileRead`
    /// gives it; only for an applied edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EditOutcome {
    Applied {
        occurrences: usize,
        /// The match was found only once whitespace and punctuation were
        /// normalized; the lines it covers were written in that form.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        normalized: bool,
    },
    Failed {
        message: String,
    },
    NotAttempted,
}
