use std::path::PathBuf;

/// The in-context view of a bash call's captured output: `shown` is what
/// goes in the tool result (head+tail plus a truncation notice if `full`
/// was over the cap), `truncated` says whether that happened.
pub(super) struct Capped {
    pub(super) shown: String,
    pub(super) truncated: bool,
}

/// Caps `full` to `cap_chars` (`agent::config::BashToolConfig::
/// output_cap_chars`, `[agent].bash_output_cap_chars` in the config file —
/// defaults to 30k, `docs/agent-tools-design.md`'s "Bash Semantics": "~30k
/// chars, head+tail preserved"), preserving a head and a tail half with an
/// explicit truncation notice in between — the "shipping standard across
/// Claude Code, goose, Cline, Codex" per the design doc. Character-counted
/// (not byte-counted) so multi-byte UTF-8 is never split mid-codepoint.
pub(super) fn cap(full: &str, cap_chars: usize) -> Capped {
    let chars: Vec<char> = full.chars().collect();
    if chars.len() <= cap_chars {
        return Capped {
            shown: full.to_string(),
            truncated: false,
        };
    }

    let head_len = cap_chars / 2;
    let tail_len = cap_chars - head_len;
    let omitted = chars.len() - head_len - tail_len;
    let head: String = chars[..head_len].iter().collect();
    let tail: String = chars[chars.len() - tail_len..].iter().collect();

    Capped {
        shown: format!(
            "{head}\n\n[... {omitted} characters truncated; the full output was written to a \
             temp file, see `output_file` ...]\n\n{tail}"
        ),
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
