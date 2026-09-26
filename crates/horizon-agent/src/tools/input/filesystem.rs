use super::bounded::{NonEmpty, Number, Object, Text};
use crate::config::{
    DEFAULT_FS_GLOB_RESULT_LIMIT, DEFAULT_FS_GREP_RESULT_LIMIT, DEFAULT_FS_READ_LINE_CAP,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadFile {
    /// Absolute path of the file to read.
    pub path: String,
    /// One-based first line.
    #[serde(default)]
    pub offset: Number<1, { u64::MAX }, 1>,
    /// Maximum lines returned; output character caps apply independently.
    #[serde(default)]
    pub limit: Number<1, 2000, { DEFAULT_FS_READ_LINE_CAP as u64 }>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Glob {
    /// Absolute directory to search under.
    pub base_path: String,
    /// Glob pattern relative to base_path (e.g. **/*.rs).
    pub pattern: String,
    #[serde(default)]
    pub limit: Number<1, { u64::MAX }, { DEFAULT_FS_GLOB_RESULT_LIMIT as u64 }>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grep {
    /// Absolute file or directory to search under.
    pub base_path: String,
    /// Regular expression to match.
    pub pattern: String,
    /// Optional glob restricting files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(default)]
    pub limit: Number<1, { u64::MAX }, { DEFAULT_FS_GREP_RESULT_LIMIT as u64 }>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteFile {
    pub path: String,
    /// Full new file content.
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditFiles {
    /// Ordered edits. Read each existing target before editing it.
    pub edits: NonEmpty<Object<Edit>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Edit {
    pub path: String,
    /// Exact nonempty text to replace.
    #[schemars(length(min = 1))]
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bash {
    /// Shell command, running in the session's current working directory.
    pub command: Text,
    /// Execution timeout in seconds.
    #[serde(default)]
    pub timeout_secs: Number<
        1,
        { crate::config::DEFAULT_BASH_TIMEOUT_MAX_SECS },
        { crate::config::DEFAULT_BASH_TIMEOUT_DEFAULT_SECS },
    >,
}
