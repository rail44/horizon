//! Detects when an incoming bash command is a re-filter of a command whose
//! full output was already captured — the "same command, different pipe"
//! pattern that wastes a full re-run just to see a different slice of the
//! same output (`docs/research/agent-editing-phase-analysis-2026-07-28.md`,
//! "検証重複 20〜31 往復": one session re-ran `cargo nextest` 10 times, varying
//! only the trailing `| tail`/`| grep` filter, burning ~1.2M input tokens).
//!
//! The mechanism is a short-circuit in `tools::execution::execute_tier1_bash`:
//! before spawning a sandboxed bash call, the live frame is scanned for a
//! prior `bash` result whose *base command* (the part before the first pipe)
//! matches the incoming command's base, whose `output_file` spill still
//! exists, and where no file-modifying tool call (`fs.write`/`fs.edit`, or a
//! `bash` call with a *different* base) has run in between. On a match,
//! execution is skipped and a guidance result is returned instead, pointing
//! the model at the existing spill file so it can re-filter with `fs.read`/
//! `fs.grep` without re-running the command.
//!
//! This never changes what `classify_call` or the approval gate would have
//! decided — the short-circuit runs after classification returned `Contained`
//! and before `spawn_sandboxed`, so the sandbox verdict and any approval
//! judgment are untouched. A stale spill file (deleted temp file) falls
//! through to a real run, and a file-modifying event between the prior run
//! and the current request blocks the short-circuit so genuinely changed
//! code always gets a fresh execution.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::frame::{AgentFrame, AgentFrameItem};

/// Maximum number of recent frame items to scan when looking for a reusable
/// prior result. Re-filters happen within a few items of the original run;
/// 200 is a generous bound that keeps the scan fast even for very long
/// sessions.
const SCAN_LIMIT: usize = 200;

/// The part of a bash command before the first single pipe `|` (not `||`),
/// trimmed. This is the "base command" — the part that does the actual work,
/// as opposed to a trailing `| tail`/`| grep`/`| head` filter that only
/// shapes the output. Two commands with the same base but different pipe
/// filters produce the same underlying output; the filters just select
/// different slices of it.
///
/// Does not parse quotes — a `|` inside a quoted string is treated as a pipe.
/// This is a heuristic for detecting re-filters, not a shell parser; a false
/// positive (splitting inside quotes) at worst makes the short-circuit miss,
/// falling through to a real run.
fn base_command(command: &str) -> &str {
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' {
            let next = i + 1 < bytes.len() && bytes[i + 1] == b'|';
            let prev = i > 0 && bytes[i - 1] == b'|';
            if !next && !prev {
                return command[..i].trim_end();
            }
            // Skip the second `|` of `||` so it isn't re-examined.
            if next {
                i += 1;
            }
        }
        i += 1;
    }
    command.trim_end()
}

/// A prior bash result whose full output can be reused instead of re-running.
#[derive(Debug)]
pub(crate) struct ReusableOutput {
    /// The prior command string (for the guidance message).
    pub command: String,
    /// Path to the spill file containing the full, uncapped output.
    pub output_file: PathBuf,
    /// The prior run's exit code, if the result was a success.
    pub exit_code: Option<i64>,
}

/// Scans `frame` for a prior `bash` result whose base command matches
/// `incoming_command`'s base, whose `output_file` spill still exists, and
/// where no file-modifying tool call intervened between that prior result and
/// the current request (which is not yet in the frame). Returns the reusable
/// output if found, or `None` to fall through to a real run.
///
/// "File-modifying" means `fs.write`, `fs.edit`, or a `bash` call whose base
/// command *differs* from the incoming one — a same-base bash call between
/// the prior match and now is a re-filter (e.g. `cmd | tail -30` between
/// `cmd` and `cmd | tail -5`), not a modification. Read-only tools (`fs.read`,
/// `fs.grep`, `recall.*`, etc.) never count.
pub(crate) fn find_reusable_output(
    frame: &AgentFrame,
    incoming_command: &str,
) -> Option<ReusableOutput> {
    let incoming_base = base_command(incoming_command);
    if incoming_base.is_empty() {
        return None;
    }

    let mut found_prior: Option<ReusableOutput> = None;
    let mut has_intervening_modification = false;

    for item in frame.items.iter().rev().take(SCAN_LIMIT) {
        if let AgentFrameItem::ToolCallFinished(result) = item {
            let Some(req) = frame.tool_call_request(&result.call_id) else {
                continue;
            };

            // Items seen before `found_prior` is set (in reverse iteration)
            // are *newer* than the prior match. Check whether any of them
            // modified files.
            if found_prior.is_none() {
                let command = req.input.get("command").and_then(|v| v.as_str());
                if is_intervening_modification(&req.tool_id, command, incoming_base) {
                    has_intervening_modification = true;
                }
            }

            // Look for a prior bash run with the same base command whose
            // spill file still exists.
            if found_prior.is_none() && req.tool_id == "bash" {
                if let Some(cmd) = req.input.get("command").and_then(|v| v.as_str()) {
                    if base_command(cmd) == incoming_base {
                        if let Some(path) =
                            result.output.get("output_file").and_then(|v| v.as_str())
                        {
                            if Path::new(path).exists() {
                                let exit_code =
                                    result.output.get("exit_code").and_then(|v| v.as_i64());
                                found_prior = Some(ReusableOutput {
                                    command: cmd.to_string(),
                                    output_file: PathBuf::from(path),
                                    exit_code,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    if has_intervening_modification {
        return None;
    }
    found_prior
}

/// Builds the bash-shaped `Value` returned as the tool result when a
/// same-base re-filter is detected — `{ exit_code, output, truncated,
/// output_file, reused_output }`. `output` is the guidance text the model sees
/// in-context; `output_file` points at the prior run's spill file so the
/// model can re-filter it with `fs.read`/`fs.grep` without re-running.
/// `reused_output: true` distinguishes this from a real run for the audit
/// trail.
pub(crate) fn guidance_output(incoming_command: &str, prior: &ReusableOutput) -> Value {
    let base = base_command(incoming_command);
    let exit_str = prior
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "not recorded".to_string());
    let output = format!(
        "This command was not re-run: its base (`{base}`) matches a prior run whose \
         full output was already captured to a temp file. Re-running would only \
         reproduce the same output with a different pipe filter.
\
         \n\
         The full output from the prior run (`{prior_cmd}`) is at:
  {path}
\
         \n\
         Read or re-filter that file directly with `fs.read` (to view a slice) \
         or `fs.grep` (to search within it) instead of re-running the command. \
         If the code or tests have changed since that run, re-run normally — this \
         short-circuit only fires when no file-modifying tool ran in between.
\
         \nPrior exit code: {exit_str}",
        base = base,
        prior_cmd = prior.command,
        path = prior.output_file.display(),
        exit_str = exit_str,
    );
    json!({
        "exit_code": prior.exit_code,
        "output": output,
        "truncated": false,
        "output_file": prior.output_file.display().to_string(),
        "reused_output": true,
    })
}

/// Whether a completed tool call between the prior match and the current
/// request counts as a file modification that should block the short-circuit.
fn is_intervening_modification(tool_id: &str, command: Option<&str>, incoming_base: &str) -> bool {
    match tool_id {
        "fs.write" | "fs.edit" => true,
        "bash" => match command {
            // A bash call with the same base is a re-filter, not a modification.
            Some(cmd) => base_command(cmd) != incoming_base,
            None => true,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{JsonValue, ToolCallId, ToolCallRequest, ToolCallResult};
    use crate::frame::{AgentFrame, AgentFrameItem};
    use serde_json::json;

    // --- base_command --------------------------------------------------------

    #[test]
    fn base_command_no_pipe() {
        assert_eq!(base_command("cargo nextest run"), "cargo nextest run");
    }

    #[test]
    fn base_command_single_pipe() {
        assert_eq!(
            base_command("cargo nextest run | tail -30"),
            "cargo nextest run"
        );
    }

    #[test]
    fn base_command_multiple_pipes() {
        assert_eq!(
            base_command("cargo nextest run | grep FAIL | awk '{print $1}'"),
            "cargo nextest run"
        );
    }

    #[test]
    fn base_command_logical_or_is_not_a_pipe() {
        assert_eq!(
            base_command("cargo build || echo failed"),
            "cargo build || echo failed"
        );
    }

    #[test]
    fn base_command_pipe_then_logical_or() {
        assert_eq!(base_command("cmd | tail -5 || echo failed"), "cmd");
    }

    #[test]
    fn base_command_trims_trailing_whitespace() {
        assert_eq!(base_command("cmd   "), "cmd");
        assert_eq!(base_command("cmd | tail   "), "cmd");
    }

    #[test]
    fn base_command_empty() {
        assert_eq!(base_command(""), "");
        assert_eq!(base_command("   "), "");
    }

    // --- find_reusable_output ------------------------------------------------

    fn bash_request(call_id: &str, command: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: "bash".to_string(),
            input: JsonValue::new(json!({ "command": command })),
            occurrence_id: None,
        })
    }

    fn bash_result(
        call_id: &str,
        output_file: Option<&str>,
        exit_code: Option<i64>,
    ) -> AgentFrameItem {
        let mut output = json!({
            "exit_code": exit_code,
            "output": "some output",
            "truncated": output_file.is_some(),
        });
        if let Some(path) = output_file {
            output["output_file"] = json!(path);
        }
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            None,
            output,
        ))
    }

    fn other_request(call_id: &str, tool_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallRequested(ToolCallRequest {
            call_id: ToolCallId(call_id.to_string()),
            tool_id: tool_id.to_string(),
            input: JsonValue::new(json!({})),
            occurrence_id: None,
        })
    }

    fn other_result(call_id: &str) -> AgentFrameItem {
        AgentFrameItem::ToolCallFinished(ToolCallResult::new(
            ToolCallId(call_id.to_string()),
            None,
            json!({ "output": "ok" }),
        ))
    }

    fn frame(items: Vec<AgentFrameItem>) -> AgentFrame {
        AgentFrame { state: None, items }
    }

    fn temp_spill(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("horizon-test-{name}.log"));
        std::fs::write(&path, "full output").unwrap();
        path
    }

    #[test]
    fn no_prior_run() {
        let f = frame(vec![]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_none());
    }

    #[test]
    fn matching_base_with_spill_file() {
        let spill = temp_spill("matching");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
        ]);

        let r = find_reusable_output(&f, "cargo nextest run | tail -5").expect("should match");
        assert_eq!(r.command, "cargo nextest run");
        assert_eq!(r.output_file, spill);
        assert_eq!(r.exit_code, Some(0));

        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn no_spill_file_falls_through() {
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", None, Some(0)),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_none());
    }

    #[test]
    fn stale_spill_file_falls_through() {
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some("/nonexistent/horizon-bash-xxx.log"), Some(0)),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_none());
    }

    #[test]
    fn intervening_fs_edit_blocks() {
        let spill = temp_spill("fs-edit");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
            other_request("c2", "fs.edit"),
            other_result("c2"),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_none());
        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn intervening_fs_write_blocks() {
        let spill = temp_spill("fs-write");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
            other_request("c2", "fs.write"),
            other_result("c2"),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_none());
        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn same_base_bash_between_does_not_block() {
        // Prior un-piped run with spill, then a piped re-run (no spill),
        // then a new piped request — the intervening piped re-run has the
        // same base, so it should NOT block the short-circuit.
        let spill = temp_spill("same-base");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
            bash_request("c2", "cargo nextest run | tail -30"),
            bash_result("c2", None, Some(0)),
        ]);

        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_some());
        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn different_base_bash_between_blocks() {
        let spill = temp_spill("diff-base");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
            bash_request("c2", "echo hello > file.txt"),
            bash_result("c2", None, Some(0)),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_none());
        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn read_only_tool_between_does_not_block() {
        let spill = temp_spill("readonly");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
            other_request("c2", "fs.read"),
            other_result("c2"),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run | tail -5").is_some());
        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn exact_same_command_matches() {
        let spill = temp_spill("exact");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
        ]);
        assert!(find_reusable_output(&f, "cargo nextest run").is_some());
        std::fs::remove_file(&spill).ok();
    }

    #[test]
    fn uses_most_recent_matching_run() {
        // Two prior runs with the same base; the second (more recent) one's
        // spill file should be returned.
        let old_spill = temp_spill("old");
        let new_spill = temp_spill("new");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(old_spill.to_str().unwrap()), Some(1)),
            bash_request("c2", "cargo nextest run"),
            bash_result("c2", Some(new_spill.to_str().unwrap()), Some(0)),
        ]);

        let r = find_reusable_output(&f, "cargo nextest run | tail -5").unwrap();
        assert_eq!(r.exit_code, Some(0), "should use the most recent run");
        assert_eq!(r.output_file, new_spill);

        std::fs::remove_file(&old_spill).ok();
        std::fs::remove_file(&new_spill).ok();
    }

    #[test]
    fn empty_command_does_not_match() {
        let spill = temp_spill("empty");
        let f = frame(vec![
            bash_request("c1", "cargo nextest run"),
            bash_result("c1", Some(spill.to_str().unwrap()), Some(0)),
        ]);
        assert!(find_reusable_output(&f, "").is_none());
        std::fs::remove_file(&spill).ok();
    }
}
