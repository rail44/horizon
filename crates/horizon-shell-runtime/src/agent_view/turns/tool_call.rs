//! A tool call's expanded-row body, from what its ACP `content` carries:
//! a change of files drawn from its Git patch, text the call gives, and,
//! when it gives none, its raw input and output.

use serde_json::Value;

use super::{ToolCallView, cap_lines_head, cap_lines_tail, tool_call_source};
use crate::agent_model::{AgentFrameItem, ToolCall, ToolContent, ToolKind};

/// One line of a diff body: `Context` lines are painted with neither role;
/// `Added`/`Removed` pair with the theme's diff colors in the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// A tool call's expanded-row body. Every line list is height-capped; the
/// view also wraps it in a scrollable, height-bounded container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallBody {
    /// A change of files, from its patch; `omitted` counts the lines past
    /// the cap.
    Diff {
        lines: Vec<DiffLine>,
        omitted: usize,
    },
    /// Lines of text: a command's output (its tail kept), a new file's
    /// content, or text the call gives (their head kept).
    Output { lines: Vec<String>, omitted: usize },
    /// One line the call gives.
    Text(String),
    /// A call that gives nothing to draw: its name, input and output as
    /// JSON.
    Raw { lines: Vec<String>, omitted: usize },
}

/// Diff body line cap.
const MAX_DIFF_LINES: usize = 300;
/// Text line cap, head-capped.
const MAX_TEXT_LINES: usize = 200;
/// A command's output line cap, tail-capped: its final summary matters
/// most.
const COMMAND_TAIL_LINES: usize = 100;

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The raw body: the call's name, input and output.
fn raw_body(call: &ToolCall) -> ToolCallBody {
    let mut text = format!("{}\ninput: {}", call.tool_id, pretty_json(&call.input));
    if let Some(output) = &call.output {
        text.push_str(&format!("\noutput: {}", pretty_json(output)));
    }
    let (lines, omitted) =
        cap_lines_head(text.lines().map(str::to_string).collect(), MAX_TEXT_LINES);
    ToolCallBody::Raw { lines, omitted }
}

/// A patch's lines as a diff body: its changed and context lines, with a
/// header naming the file before each file's lines when it changes more
/// than one, and the hunk headers when it has more than one hunk.
fn patch_lines(patch: &str) -> Vec<DiffLine> {
    let files = patch
        .lines()
        .filter(|line| line.starts_with("+++ "))
        .count();
    let hunks = patch.lines().filter(|line| line.starts_with("@@")).count();
    let context = |text: String| DiffLine {
        kind: DiffLineKind::Context,
        text,
    };
    let mut lines = Vec::new();
    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            if files > 1 {
                lines.push(context(format!("--- {path}")));
            }
        } else if line.starts_with("diff --git ")
            || line.starts_with("--- ")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
            || line.starts_with("index ")
            || line.starts_with('\\')
        {
        } else if line.starts_with("@@") {
            if hunks > 1 {
                lines.push(context(line.to_string()));
            }
        } else if let Some(text) = line.strip_prefix('+') {
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: text.to_string(),
            });
        } else if let Some(text) = line.strip_prefix('-') {
            lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                text: text.to_string(),
            });
        } else {
            lines.push(context(line.strip_prefix(' ').unwrap_or(line).to_string()));
        }
    }
    lines
}

/// The body of a finished call, from its content.
pub fn build_tool_call_body(call: &ToolCall) -> ToolCallBody {
    // Each change's files (with whether each was added) and its patch.
    type Change<'a> = (&'a [(String, bool)], Option<&'a str>);
    let diffs: Vec<Change> = (call.content.iter())
        .filter_map(|content| match content {
            ToolContent::Diff { changes, patch } => Some((changes.as_slice(), patch.as_deref())),
            ToolContent::Text(_) => None,
        })
        .collect();
    if !diffs.is_empty() {
        let patches: Vec<&str> = diffs.iter().filter_map(|(_, patch)| *patch).collect();
        if patches.is_empty() {
            // A change with no patch to draw: the files it names.
            let paths: Vec<&str> = (diffs.iter())
                .flat_map(|(changes, _)| changes.iter().map(|(path, _)| path.as_str()))
                .collect();
            return ToolCallBody::Text(paths.join(", "));
        }
        let lines: Vec<DiffLine> = patches.into_iter().flat_map(patch_lines).collect();
        // A file only added reads as its content.
        let added = (diffs.iter()).all(|(changes, _)| changes.iter().all(|(_, added)| *added));
        if added {
            let text: Vec<String> = lines.into_iter().map(|line| line.text).collect();
            let (lines, omitted) = cap_lines_head(text, MAX_TEXT_LINES);
            return ToolCallBody::Output { lines, omitted };
        }
        let (lines, omitted) = cap_lines_head(lines, MAX_DIFF_LINES);
        return ToolCallBody::Diff { lines, omitted };
    }
    let texts: Vec<&str> = (call.content.iter())
        .filter_map(|content| match content {
            ToolContent::Text(text) => Some(text.as_str()),
            ToolContent::Diff { .. } => None,
        })
        .collect();
    if texts.is_empty() {
        return raw_body(call);
    }
    let lines: Vec<String> = (texts.iter())
        .flat_map(|text| text.lines())
        .map(str::to_string)
        .collect();
    match (call.kind, lines.as_slice()) {
        (ToolKind::Execute, _) => {
            let (lines, omitted) = cap_lines_tail(lines, COMMAND_TAIL_LINES);
            ToolCallBody::Output { lines, omitted }
        }
        (_, [line]) => ToolCallBody::Text(line.clone()),
        _ => {
            let (lines, omitted) = cap_lines_head(lines, MAX_TEXT_LINES);
            ToolCallBody::Output { lines, omitted }
        }
    }
}

/// Builds a row's body from the call item `build_tool_call_views` bound the
/// view to. `items` must be the same slice used to build `call`.
pub fn tool_call_body(items: &[AgentFrameItem], call: &ToolCallView) -> Option<ToolCallBody> {
    Some(build_tool_call_body(tool_call_source(items, call)?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::build_tool_call_views;
    use super::super::test_support::*;
    use super::*;
    use crate::agent_model::ToolCallStatus;

    fn finished(kind: ToolKind, content: Vec<ToolContent>) -> ToolCall {
        ToolCall {
            occurrence_id: "c1".into(),
            tool_id: "tool".into(),
            title: "Do it".into(),
            kind,
            locations: Vec::new(),
            content,
            status: ToolCallStatus::Completed,
            input: json!({"x": 1}),
            output: Some(json!({"y": 2})),
        }
    }

    fn diff(changes: &[(&str, bool)], patch: Option<&str>) -> ToolContent {
        ToolContent::Diff {
            changes: (changes.iter())
                .map(|(path, added)| (path.to_string(), *added))
                .collect(),
            patch: patch.map(str::to_string),
        }
    }

    #[test]
    fn a_patch_is_drawn_as_its_changed_and_context_lines() {
        let patch = "diff --git /w/a /w/a\n--- /w/a\n+++ /w/a\n@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n";
        let body = build_tool_call_body(&finished(
            ToolKind::Edit,
            vec![diff(&[("/w/a", false)], Some(patch))],
        ));
        let ToolCallBody::Diff { lines, omitted: 0 } = body else {
            panic!("{body:?}");
        };
        assert_eq!(
            diff_texts(&lines),
            [
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "b"),
                (DiffLineKind::Added, "B"),
                (DiffLineKind::Context, "c"),
            ]
        );
    }

    #[test]
    fn a_patch_of_several_files_and_hunks_names_them() {
        let patch = "diff --git /w/a /w/a\n--- /w/a\n+++ /w/a\n@@ -1,1 +1,1 @@\n-x\n+y\n\
                     @@ -9,1 +9,1 @@\n-p\n+q\n\
                     diff --git /w/b /w/b\n--- /w/b\n+++ /w/b\n@@ -1,1 +1,1 @@\n-m\n+n\n";
        let body = build_tool_call_body(&finished(
            ToolKind::Edit,
            vec![diff(&[("/w/a", false), ("/w/b", false)], Some(patch))],
        ));
        let ToolCallBody::Diff { lines, .. } = body else {
            panic!("{body:?}");
        };
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "--- /w/a",
                "@@ -1,1 +1,1 @@",
                "x",
                "y",
                "@@ -9,1 +9,1 @@",
                "p",
                "q",
                "--- /w/b",
                "@@ -1,1 +1,1 @@",
                "m",
                "n"
            ]
        );
    }

    #[test]
    fn a_file_only_added_reads_as_its_content() {
        let patch = "diff --git /w/n /w/n\nnew file mode 100644\n--- /dev/null\n+++ /w/n\n\
                     @@ -0,0 +1,2 @@\n+one\n+two\n";
        let body = build_tool_call_body(&finished(
            ToolKind::Edit,
            vec![diff(&[("/w/n", true)], Some(patch))],
        ));
        assert_eq!(
            body,
            ToolCallBody::Output {
                lines: vec!["one".into(), "two".into()],
                omitted: 0
            }
        );
    }

    #[test]
    fn a_change_without_a_patch_names_its_files() {
        let body = build_tool_call_body(&finished(
            ToolKind::Edit,
            vec![diff(&[("/w/a", false)], None)],
        ));
        assert_eq!(body, ToolCallBody::Text("/w/a".into()));
    }

    #[test]
    fn a_commands_output_keeps_its_tail() {
        let output: String = (1..=150).map(|n| format!("line {n}\n")).collect();
        let body = build_tool_call_body(&finished(
            ToolKind::Execute,
            vec![ToolContent::Text(output)],
        ));
        let ToolCallBody::Output { lines, omitted } = body else {
            panic!("{body:?}");
        };
        assert_eq!(omitted, 50);
        assert_eq!(lines.first().map(String::as_str), Some("line 51"));
        assert_eq!(lines.last().map(String::as_str), Some("line 150"));
    }

    #[test]
    fn one_line_of_text_is_a_line_and_more_are_output() {
        let one = build_tool_call_body(&finished(
            ToolKind::Search,
            vec![ToolContent::Text("3 matches".into())],
        ));
        assert_eq!(one, ToolCallBody::Text("3 matches".into()));
        let more = build_tool_call_body(&finished(
            ToolKind::Other,
            vec![ToolContent::Text("a\nb".into())],
        ));
        assert_eq!(
            more,
            ToolCallBody::Output {
                lines: vec!["a".into(), "b".into()],
                omitted: 0
            }
        );
    }

    #[test]
    fn a_call_that_gives_nothing_shows_its_input_and_output() {
        let body = build_tool_call_body(&finished(ToolKind::Other, Vec::new()));
        let ToolCallBody::Raw { lines, .. } = body else {
            panic!("{body:?}");
        };
        assert_eq!(lines[0], "tool");
        assert!(lines.iter().any(|line| line.contains("\"x\": 1")));
        assert!(lines.iter().any(|line| line.contains("\"y\": 2")));
    }

    #[test]
    fn a_body_is_the_one_of_the_views_own_occurrence() {
        let items = vec![
            tool_finished(
                "dup",
                "bash",
                json!({"command": "echo first"}),
                json!({"exit_code": 0, "output": "first", "termination": "exited", "output_file": null, "truncated": false}),
            ),
            tool_requested("again", "bash", json!({"command": "echo second"})),
        ];
        let views = build_tool_call_views(&items);
        assert_eq!(
            tool_call_body(&items, &views[0]),
            Some(ToolCallBody::Output {
                lines: vec!["first".into()],
                omitted: 0
            })
        );
        let mut stale = views[1].clone();
        stale.request_index = 0;
        assert_eq!(tool_call_body(&items, &stale), None);
    }
}
