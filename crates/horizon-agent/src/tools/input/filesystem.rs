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
    // `path` is what `fs_read` calls its own; models reach for it here too.
    #[serde(alias = "path")]
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
    // `path` is what `fs_read` calls its own; models reach for it here too.
    #[serde(alias = "path")]
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

/// `fs_edit`'s arguments with every edit's file settled: what the handler
/// and the approval checks read. Decoded from [`EditFilesArgs`], the shape
/// the model is shown.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(try_from = "EditFilesArgs")]
pub(crate) struct EditFiles {
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Edit {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    pub replace_all: bool,
}

impl JsonSchema for EditFiles {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        EditFilesArgs::schema_name()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        EditFilesArgs::json_schema(generator)
    }
}

/// `fs_edit` as the model writes it. Most calls edit one file, and a model
/// asked for a path on every edit often names it once beside `edits`
/// instead, so both are accepted: an edit's own `path` wins, the top-level
/// one fills in for an edit that has none.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditFilesArgs {
    /// Absolute path of the file for every edit that names none of its own.
    #[serde(default)]
    pub path: Option<String>,
    /// Ordered edits. Read each existing target before editing it.
    pub edits: NonEmpty<Object<EditArgs>>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditArgs {
    /// Absolute path of the file; may be left out when the top-level
    /// `path` names it.
    #[serde(default)]
    pub path: Option<String>,
    /// Exact nonempty text to replace.
    #[schemars(length(min = 1))]
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

impl TryFrom<EditFilesArgs> for EditFiles {
    type Error = String;

    fn try_from(args: EditFilesArgs) -> Result<Self, String> {
        let settle = |(index, edit): (usize, &Object<EditArgs>)| {
            let path = edit.path.as_ref().or(args.path.as_ref()).ok_or_else(|| {
                format!(
                    "edits[{index}] names no file: give it a `path`, or give one `path` \
                     beside `edits` for every edit"
                )
            })?;
            Ok(Edit {
                path: path.clone(),
                old_string: edit.old_string.clone(),
                new_string: edit.new_string.clone(),
                replace_all: edit.replace_all,
            })
        };
        let edits = args
            .edits
            .iter()
            .enumerate()
            .map(settle)
            .collect::<Result<_, String>>()?;
        Ok(Self { edits })
    }
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
    /// Run this call once outside the sandbox, with the host's ordinary
    /// authority. Only after a sandboxed run failed because the sandbox
    /// blocked it (a read-only file system, a refused host); it needs
    /// approval, and later calls run sandboxed again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outside_sandbox: bool,
    /// Hosts this call needs beyond the session's allowed domains
    /// (`example.com`, `*.example.com`). They open for this call only, once
    /// approved; the call stays sandboxed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
}
