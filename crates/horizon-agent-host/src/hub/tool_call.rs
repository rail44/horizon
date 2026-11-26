//! A tool call as ACP's standard fields carry it: a `title` a person
//! reads whole ("Read src/main.rs", "Run cargo test"), the files it works
//! on (`locations`), and what it produced (`content`): the change it made
//! as a `Diff` with Git's patch text, or a few lines of text. A client
//! draws these and needs to know none of this agent's tools.

use std::path::{Path, PathBuf};

use agent_client_protocol::v2::schema as v2;
use horizon_agent::contract::tool_output::{BashOutput, FileRead, Location, Matches, decode};
use horizon_agent::transcript::{ToolCallClassification, classify, edit_entries, str_field};
use serde_json::Value;

/// What a person reads for the call: what it does, then what to.
pub(super) fn title(tool_id: &str, input: &Value) -> String {
    let ToolCallClassification {
        known,
        verb,
        target,
        ..
    } = classify(tool_id, input, None);
    let first_line = |text: &str| text.lines().next().unwrap_or_default().to_string();
    let target = match tool_id {
        "bash" => str_field(input, "command").map(first_line).or(target),
        _ => target,
    };
    let verb = match tool_id {
        "bash" => "Run".to_string(),
        "fs_grep" | "fs_glob" => "Search".to_string(),
        _ => verb,
    };
    match (known, target.filter(|target| !target.is_empty())) {
        (true, Some(target)) => format!("{verb} {target}"),
        (true, None) => verb,
        (false, _) => tool_id.to_string(),
    }
}

/// `path` as an absolute path: as it is, or under `root`.
fn absolute(root: Option<&Path>, path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    match (path.is_absolute(), root) {
        (true, _) => Some(path.to_path_buf()),
        (false, Some(root)) => Some(root.join(path)),
        (false, None) => None,
    }
}

/// The files the call works on, in the order it names them.
pub(super) fn locations(
    root: Option<&Path>,
    tool_id: &str,
    input: &Value,
) -> Vec<v2::ToolCallLocation> {
    let paths: Vec<&str> = match tool_id {
        "fs_read" | "fs_write" => str_field(input, "path").into_iter().collect(),
        "fs_edit" => {
            let mut paths: Vec<&str> = Vec::new();
            for edit in edit_entries(input) {
                if !paths.contains(&edit.path) {
                    paths.push(edit.path);
                }
            }
            paths
        }
        _ => Vec::new(),
    };
    (paths.into_iter())
        .filter_map(|path| absolute(root, path))
        .map(v2::ToolCallLocation::new)
        .collect()
}

/// The files a patch changes: each section's path, added when the section
/// has no old side.
fn changes(patch: &str) -> Vec<v2::DiffChange> {
    let mut changes = Vec::new();
    let mut created = false;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            created = false;
        } else if line == "--- /dev/null" {
            created = true;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            changes.push(match created {
                true => v2::DiffChange::add(PathBuf::from(path)),
                false => v2::DiffChange::modify(PathBuf::from(path)),
            });
        }
    }
    changes
}

/// What the finished call produced: the change it made, else a line or
/// the output a person reads for it. None for a tool this agent does not
/// describe, or an output it cannot read, which a client shows as it is.
pub(super) fn content(
    tool_id: &str,
    input: &Value,
    output: &Value,
    patch: Option<&str>,
) -> Vec<v2::ToolCallContent> {
    if let Some(patch) = patch {
        let changes = changes(patch);
        if !changes.is_empty() {
            return vec![v2::Diff::patch(patch, changes).into()];
        }
    }
    let text = |text: String| -> Vec<v2::ToolCallContent> {
        match text.is_empty() {
            true => Vec::new(),
            false => vec![v2::ContentBlock::from(text).into()],
        }
    };
    match tool_id {
        "bash" => match decode::<BashOutput>(output) {
            Some(result) => text(match &result.message {
                Some(message) => format!("{message}\n{}", result.output),
                None => result.output,
            }),
            None => Vec::new(),
        },
        "fs_read" => {
            let path = str_field(input, "path").unwrap_or_default();
            text(match decode::<FileRead>(output) {
                Some(result) => format!(
                    "{path} · lines {}-{} of {}",
                    result.start_line, result.end_line, result.total_lines
                ),
                None => path.to_string(),
            })
        }
        "fs_grep" | "fs_glob" => {
            let pattern = str_field(input, "pattern").unwrap_or_default();
            let pattern = match tool_id {
                "fs_grep" => format!("\"{pattern}\""),
                _ => pattern.to_string(),
            };
            let base = str_field(input, "base_path").unwrap_or_default();
            let count = match tool_id {
                "fs_grep" => {
                    decode::<Matches<Location>>(output).map(|result| result.returned_count)
                }
                _ => decode::<Matches<String>>(output).map(|result| result.returned_count),
            };
            text(match count {
                Some(count) => format!("{pattern} in {base} · {count} matches"),
                None => format!("{pattern} in {base}"),
            })
        }
        _ => {
            let ToolCallClassification {
                known,
                verb,
                target,
                summary,
            } = classify(tool_id, input, Some(output));
            if !known {
                return Vec::new();
            }
            text(match (target, summary) {
                (Some(target), Some(summary)) => format!("{verb} {target} · {summary}"),
                (Some(target), None) => format!("{verb} {target}"),
                (None, Some(summary)) => format!("{verb} · {summary}"),
                (None, None) => verb,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_title_says_what_the_call_does_and_to_what() {
        assert_eq!(
            title("fs_read", &json!({"path": "src/a.rs"})),
            "Read src/a.rs"
        );
        assert_eq!(
            title("bash", &json!({"command": "cargo test\necho done"})),
            "Run cargo test"
        );
        assert_eq!(title("fs_grep", &json!({"pattern": "todo"})), "Search todo");
        assert_eq!(title("board_show", &json!({"id": 3})), "board_show");
    }

    #[test]
    fn locations_are_absolute_under_the_root() {
        let located = locations(
            Some(Path::new("/w")),
            "fs_edit",
            &json!({"edits": [
                {"path": "a.rs", "old_string": "x", "new_string": "y"},
                {"path": "/abs/b.rs", "old_string": "x", "new_string": "y"},
                {"path": "a.rs", "old_string": "z", "new_string": "w"},
            ]}),
        );
        let paths: Vec<_> = located
            .iter()
            .map(|location| location.path.0.clone())
            .collect();
        assert_eq!(
            paths,
            [PathBuf::from("/w/a.rs"), PathBuf::from("/abs/b.rs")]
        );
    }

    #[test]
    fn a_patch_is_a_diff_naming_the_files_it_changes() {
        let patch = "diff --git /w/a /w/a\n--- /w/a\n+++ /w/a\n@@ -1,1 +1,1 @@\n-x\n+y\n\
                     diff --git /w/b /w/b\nnew file mode 100644\n--- /dev/null\n+++ /w/b\n\
                     @@ -0,0 +1,1 @@\n+z\n";
        let content = content("fs_edit", &json!({}), &json!({}), Some(patch));
        let [v2::ToolCallContent::Diff(diff)] = content.as_slice() else {
            panic!("{content:?}");
        };
        assert_eq!(
            diff.changes,
            [
                v2::DiffChange::modify(PathBuf::from("/w/a")),
                v2::DiffChange::add(PathBuf::from("/w/b"))
            ]
        );
        assert_eq!(
            diff.patch.as_ref().map(|patch| patch.text.as_str()),
            Some(patch)
        );
    }

    #[test]
    fn a_command_gives_its_output_and_an_unknown_tool_nothing() {
        let output = json!({"exit_code": 0, "output": "ok\n", "termination": "exited", "output_file": null, "truncated": false});
        let content = content("bash", &json!({"command": "true"}), &output, None);
        assert_eq!(
            content,
            vec![v2::ContentBlock::from("ok\n".to_string()).into()]
        );
        assert!(super::content("board_show", &json!({}), &json!({"x": 1}), None).is_empty());
    }
}
