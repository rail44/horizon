//! Permanent, env-gated diagnostic trace for the input pipeline: winit's
//! own key/IME arrival, through gpui dispatch, to the terminal's PTY-send
//! decision (`src/terminal/mod.rs` carries the other half). Sibling to
//! `HORIZON_GPUI_DUMP`/`HORIZON_GPUI_DRIVE`
//! (`src/terminal/session.rs`) — see AGENTS.md's GUI Verification section.
//!
//! Set `HORIZON_INPUT_TRACE=1` for stderr, or to a file path to append
//! there instead. Unset (the default) costs one `OnceLock` read per call
//! site and nothing else — no formatting, no I/O, no allocation. Every
//! emitted line is prefixed `input-trace:` so `grep` finds the whole
//! chain for one keystroke across both crates' output.
//!
//! Traces hop *metadata* only — key names, event kinds, lengths, verdicts.
//! `Ime::Preedit`/`Commit` carry the user's actual typed/composed text;
//! this module never logs more of it than the first character (to confirm
//! *something* arrived) plus its length (to confirm how much) — see
//! `describe_text`.

use std::io::Write as _;
use std::sync::{Mutex, OnceLock};

pub(crate) enum Sink {
    Stderr,
    File(Mutex<std::fs::File>),
}

static SINK: OnceLock<Option<Sink>> = OnceLock::new();

pub(crate) fn sink() -> Option<&'static Sink> {
    SINK.get_or_init(|| match std::env::var("HORIZON_INPUT_TRACE") {
        Ok(value) if value == "1" => Some(Sink::Stderr),
        Ok(value) if !value.is_empty() => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&value)
            .map(|file| Sink::File(Mutex::new(file)))
            .ok(),
        _ => None,
    })
    .as_ref()
}

/// Redacts a piece of IME text to "first character + length" — enough to
/// confirm delivery and size without logging what was actually typed.
pub(crate) fn describe_text(text: &str) -> String {
    let len = text.chars().count();
    match text.chars().next() {
        Some(first) => format!("{first:?}+{}more", len.saturating_sub(1)),
        None => "empty".to_string(),
    }
}

/// Writes one line to the configured sink, if any. Called only from the
/// [`trace`] macro, which checks [`sink`] first so `format_args!` never
/// runs when tracing is disabled.
pub(crate) fn write_line(sink: &Sink, args: std::fmt::Arguments) {
    match sink {
        Sink::Stderr => eprintln!("input-trace: {args}"),
        Sink::File(file) => {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "input-trace: {args}");
            }
        }
    }
}

/// `input_trace!("...", args)` — formats and emits one line iff
/// `HORIZON_INPUT_TRACE` is set; otherwise a single cheap `OnceLock` read
/// and nothing more (the `format_args!` inside the `if let` is never
/// evaluated when the sink is `None`).
macro_rules! input_trace {
    ($($arg:tt)*) => {
        if let Some(sink) = $crate::input_trace::sink() {
            $crate::input_trace::write_line(sink, format_args!($($arg)*));
        }
    };
}
pub(crate) use input_trace;

#[cfg(test)]
mod tests {
    use super::describe_text;

    #[test]
    fn empty_text_is_reported_as_empty() {
        assert_eq!(describe_text(""), "empty");
    }

    #[test]
    fn single_char_reports_first_char_and_zero_more() {
        assert_eq!(describe_text("a"), "'a'+0more");
    }

    #[test]
    fn multi_char_reports_only_the_first_char_and_a_count() {
        // The rest of a composed/committed word must never appear in the
        // description -- only its first character and how many more.
        let described = describe_text("えっと");
        assert_eq!(described, "'え'+2more");
        assert!(!described.contains('っ'));
        assert!(!described.contains('と'));
    }
}
