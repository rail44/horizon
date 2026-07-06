use std::path::{Path, PathBuf};

/// The in-context view of a bash call's captured output: `shown` is what
/// goes in the tool result (head+tail plus a truncation notice if `full`
/// was over the cap), `truncated` says whether that happened.
pub(super) struct Capped {
    pub(super) shown: String,
    pub(super) truncated: bool,
}

/// Share of `cap_chars` given to the head when truncating; the remainder
/// goes to the tail. A chatty tool like `cargo test --workspace` front-loads
/// compile spew ahead of any test result and closes with the one line that
/// matters most for a quick verdict (the final pass/fail summary) — so the
/// tail is worth more of the fixed budget than a 50/50 split gives it.
/// Docs/tasks/backlog.md item 11: a 50/50 split still let a run's *head*
/// budget get eaten by compile output before reaching the first suite's
/// results, so widening the tail doesn't trade away much that a 50/50 split
/// was actually delivering.
const HEAD_SHARE_NUM: usize = 1;
const HEAD_SHARE_DEN: usize = 3;

/// Caps `full` to `cap_chars` (`agent::config::BashToolConfig::
/// output_cap_chars`, `[agent].bash_output_cap_chars` in the config file —
/// defaults to 30k, `docs/agent-tools-design.md`'s "Bash Semantics": "~30k
/// chars, head+tail preserved"), preserving a head (`HEAD_SHARE_NUM`/
/// `HEAD_SHARE_DEN` of the cap) and a tail (the remainder) with an explicit
/// truncation notice in between — the "shipping standard across Claude
/// Code, goose, Cline, Codex" per the design doc. Character-counted (not
/// byte-counted) so multi-byte UTF-8 is never split mid-codepoint. `
/// spill_path` — the path `spill` wrote the full output to, if it
/// succeeded — is folded into the notice so the model sees where to find
/// what got cut without having to separately notice the result's
/// `output_file` field.
pub(super) fn cap(full: &str, cap_chars: usize, spill_path: Option<&Path>) -> Capped {
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= cap_chars {
        return Capped {
            shown: full.to_string(),
            truncated: false,
        };
    }

    let head_len = cap_chars * HEAD_SHARE_NUM / HEAD_SHARE_DEN;
    let tail_len = cap_chars - head_len;
    let omitted = chars.len() - head_len - tail_len;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();

    let location = match spill_path {
        Some(path) => format!("full output at {}", path.display()),
        None => "the full output could not be saved to a temp file".to_string(),
    };

    Capped {
        shown: format!("{head}\n\n[... {omitted} chars omitted — {location} ...]\n\n{tail}"),
        truncated: true,
    }
}

/// Spills the full (uncapped) output to a fresh temp file so the agent can
/// re-read it selectively, per `docs/agent-tools-design.md`. Returns `None`
/// (rather than failing the whole call) if the write itself fails — a
/// harness failure to persist a debugging aid shouldn't turn an otherwise
/// successful command into an `is_error` result.
pub(super) fn spill(full: &str) -> Option<PathBuf> {
    let path = std::env::temp_dir().join(format!("horizon-bash-{}.log", uuid::Uuid::new_v4()));
    std::fs::write(&path, full).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests;
