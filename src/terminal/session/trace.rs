//! Passive PTY trace, gated by `HORIZON_PTY_TRACE`. Diagnostic-only tap for
//! chasing the "attaching a second program to the PTY corrupts the
//! terminal's layout" class of bug: when the env var is unset,
//! [`PtyTrace::from_env`] returns `None` and every call site degrades to a
//! single `Option` check — no formatting, no file I/O, no branching beyond
//! that check.
//!
//! Enable with `HORIZON_PTY_TRACE=/tmp/horizon-pty` (any path prefix); each
//! terminal session then appends newline-delimited JSON to
//! `<prefix>-<8-hex-char-session-id>.jsonl`, one record per PTY interaction:
//!
//! - `"in"` — every byte Horizon writes to the PTY (keystrokes, pastes,
//!   query responses such as color/size answers). Logged in full, one
//!   record per write, because this is the side under suspicion: it is
//!   *everything Horizon contributed* while some other observer (e.g.
//!   ghostty, when Claude Code multi-attaches) saw the layout corrupt.
//! - `"out"` — bytes read from the PTY (child process output). App output
//!   volume isn't the point, so consecutive reads within 5ms are coalesced
//!   into one record with a summed `bytes_len`; the sample fields keep the
//!   first read's data.
//! - `"resize"` — the `TIOCSWINSZ` geometry actually applied to the PTY.
//!
//! `"in"`/`"out"` records carry `bytes_len` (the real length), `bytes_hex`
//! (hex of at most the first 256 bytes), and `utf8_lossy_preview` (at most
//! the first 128 chars of a lossy UTF-8 decode of that same sample; control
//! characters come out escaped for free via JSON string serialization).

use std::env;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::terminal::types::TerminalSize;

const TRACE_ENV_VAR: &str = "HORIZON_PTY_TRACE";
const HEX_SAMPLE_LEN: usize = 256;
const PREVIEW_CHAR_LEN: usize = 128;
const OUT_COALESCE_WINDOW: Duration = Duration::from_millis(5);

pub(super) struct PtyTrace {
    file: File,
    pending_out: Option<PendingOut>,
}

struct PendingOut {
    ts_ms: u64,
    last_seen: Instant,
    bytes_hex: String,
    preview: String,
    total_len: usize,
}

#[derive(Serialize)]
struct TraceLine {
    ts_ms: u64,
    #[serde(flatten)]
    event: TraceEvent,
}

#[derive(Serialize)]
#[serde(tag = "dir")]
enum TraceEvent {
    #[serde(rename = "in")]
    In {
        bytes_len: usize,
        bytes_hex: String,
        utf8_lossy_preview: String,
    },
    #[serde(rename = "out")]
    Out {
        bytes_len: usize,
        bytes_hex: String,
        utf8_lossy_preview: String,
    },
    #[serde(rename = "resize")]
    Resize {
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
}

impl PtyTrace {
    /// `None` when `HORIZON_PTY_TRACE` is unset (the default). Callers hold
    /// an `Option<PtyTrace>` and every log call site is one branch on it.
    pub(super) fn from_env(session_short_id: &str) -> Option<Self> {
        let prefix = env::var(TRACE_ENV_VAR).ok()?;
        Self::with_prefix(&prefix, session_short_id)
    }

    /// Env-var-independent constructor so tests can exercise the trace
    /// without mutating process-wide environment state (which would race
    /// against other tests running in parallel threads).
    fn with_prefix(prefix: &str, session_short_id: &str) -> Option<Self> {
        let path = format!("{prefix}-{session_short_id}.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        Some(Self {
            file,
            pending_out: None,
        })
    }

    /// Bytes Horizon wrote to the PTY. One record per call, logged in full
    /// (subject to the 256-byte/128-char sampling caps) — never coalesced.
    pub(super) fn record_in(&mut self, bytes: &[u8]) {
        let (bytes_hex, preview) = sample(bytes);
        write_line(
            &mut self.file,
            TraceEvent::In {
                bytes_len: bytes.len(),
                bytes_hex,
                utf8_lossy_preview: preview,
            },
        );
    }

    /// Bytes read from the PTY. Consecutive calls within
    /// [`OUT_COALESCE_WINDOW`] of each other merge into a single record:
    /// the sample (hex/preview) is the first read's, `bytes_len` is the sum.
    pub(super) fn record_out(&mut self, bytes: &[u8]) {
        let now = Instant::now();
        if let Some(pending) = &mut self.pending_out {
            if now.duration_since(pending.last_seen) < OUT_COALESCE_WINDOW {
                pending.total_len += bytes.len();
                pending.last_seen = now;
                return;
            }
            self.flush_pending_out();
        }
        let (bytes_hex, preview) = sample(bytes);
        self.pending_out = Some(PendingOut {
            ts_ms: now_ms(),
            last_seen: now,
            bytes_hex,
            preview,
            total_len: bytes.len(),
        });
    }

    /// The `TIOCSWINSZ` geometry actually applied to the PTY.
    pub(super) fn record_resize(&mut self, size: TerminalSize) {
        write_line(
            &mut self.file,
            TraceEvent::Resize {
                cols: size.cols,
                rows: size.rows,
                pixel_width: size.pixel_width,
                pixel_height: size.pixel_height,
            },
        );
    }

    fn flush_pending_out(&mut self) {
        if let Some(pending) = self.pending_out.take() {
            write_line_at(
                &mut self.file,
                pending.ts_ms,
                TraceEvent::Out {
                    bytes_len: pending.total_len,
                    bytes_hex: pending.bytes_hex,
                    utf8_lossy_preview: pending.preview,
                },
            );
        }
    }
}

/// A read loop exits through several paths (EOF, error, channel close); a
/// coalesced "out" burst still in flight at that point must not be dropped
/// silently, so flush it whenever the trace itself goes away.
impl Drop for PtyTrace {
    fn drop(&mut self) {
        self.flush_pending_out();
    }
}

fn write_line(file: &mut File, event: TraceEvent) {
    write_line_at(file, now_ms(), event);
}

fn write_line_at(file: &mut File, ts_ms: u64, event: TraceEvent) {
    if let Ok(json) = serde_json::to_string(&TraceLine { ts_ms, event }) {
        let _ = writeln!(file, "{json}");
    }
}

fn sample(bytes: &[u8]) -> (String, String) {
    let head = &bytes[..bytes.len().min(HEX_SAMPLE_LEN)];
    let mut bytes_hex = String::with_capacity(head.len() * 2);
    for byte in head {
        let _ = write!(bytes_hex, "{byte:02x}");
    }
    let preview: String = String::from_utf8_lossy(head)
        .chars()
        .take(PREVIEW_CHAR_LEN)
        .collect();
    (bytes_hex, preview)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::thread;

    fn read_records(path: &std::path::Path) -> Vec<serde_json::Value> {
        let file = File::open(path).expect("trace file should exist");
        BufReader::new(file)
            .lines()
            .map(|line| serde_json::from_str(&line.expect("line should be readable")).unwrap())
            .collect()
    }

    struct TempTrace {
        path: std::path::PathBuf,
    }

    impl Drop for TempTrace {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_trace(name: &str) -> (PtyTrace, TempTrace) {
        let prefix = std::env::temp_dir().join(format!("horizon-pty-trace-test-{name}"));
        let prefix = prefix.to_str().unwrap().to_string();
        let short_id = "abcd1234";
        let path = std::path::PathBuf::from(format!("{prefix}-{short_id}.jsonl"));
        let _ = std::fs::remove_file(&path);
        let trace =
            PtyTrace::with_prefix(&prefix, short_id).expect("trace file should be creatable");
        (trace, TempTrace { path })
    }

    #[test]
    fn disabled_by_default_creates_no_file() {
        // Assumes the ambient test environment does not set
        // `HORIZON_PTY_TRACE` — the same default the feature is designed
        // around ("unset env = zero overhead"). Other tests in this module
        // use `with_prefix` directly and never touch the env var, so there
        // is no cross-test interference to guard against here.
        assert!(
            env::var(TRACE_ENV_VAR).is_err(),
            "test env should not set {TRACE_ENV_VAR}"
        );
        assert!(PtyTrace::from_env("deadbeef").is_none());
    }

    #[test]
    fn in_record_has_expected_shape() {
        let (mut trace, guard) = temp_trace("in-shape");
        trace.record_in(b"hello");
        drop(trace);

        let records = read_records(&guard.path);
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record["dir"], "in");
        assert_eq!(record["bytes_len"], 5);
        assert_eq!(record["bytes_hex"], "68656c6c6f");
        assert_eq!(record["utf8_lossy_preview"], "hello");
        assert!(record["ts_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn resize_record_has_expected_shape() {
        let (mut trace, guard) = temp_trace("resize-shape");
        trace.record_resize(TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 640,
            pixel_height: 480,
        });
        drop(trace);

        let records = read_records(&guard.path);
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record["dir"], "resize");
        assert_eq!(record["cols"], 80);
        assert_eq!(record["rows"], 24);
        assert_eq!(record["pixel_width"], 640);
        assert_eq!(record["pixel_height"], 480);
        assert!(record.get("bytes_len").is_none());
    }

    #[test]
    fn out_record_samples_first_256_bytes_and_128_char_preview() {
        let (mut trace, guard) = temp_trace("out-sample");
        let long = vec![b'a'; 1000];
        trace.record_out(&long);
        drop(trace);

        let records = read_records(&guard.path);
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record["dir"], "out");
        assert_eq!(record["bytes_len"], 1000);
        assert_eq!(record["bytes_hex"].as_str().unwrap().len(), 256 * 2);
        assert_eq!(record["utf8_lossy_preview"].as_str().unwrap().len(), 128);
    }

    #[test]
    fn consecutive_out_reads_within_window_coalesce_into_one_record() {
        let (mut trace, guard) = temp_trace("out-coalesce");
        trace.record_out(b"abc");
        trace.record_out(b"def");
        trace.record_out(b"ghi");
        drop(trace);

        let records = read_records(&guard.path);
        assert_eq!(records.len(), 1, "reads within the window should merge");
        let record = &records[0];
        assert_eq!(record["dir"], "out");
        assert_eq!(record["bytes_len"], 9);
        // Sample fields come from the first read in the burst.
        assert_eq!(record["bytes_hex"], "616263");
        assert_eq!(record["utf8_lossy_preview"], "abc");
    }

    #[test]
    fn out_reads_beyond_window_start_a_new_record() {
        let (mut trace, guard) = temp_trace("out-window-expiry");
        trace.record_out(b"first");
        thread::sleep(OUT_COALESCE_WINDOW * 3);
        trace.record_out(b"second");
        drop(trace);

        let records = read_records(&guard.path);
        assert_eq!(records.len(), 2, "reads past the window should not merge");
        assert_eq!(records[0]["bytes_len"], 5);
        assert_eq!(records[1]["bytes_len"], 6);
    }
}
