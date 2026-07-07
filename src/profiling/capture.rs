use std::io::Write as _;
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::path::log_path;
use super::record::ProfileRecord;

/// Opt-in switch for UI-thread profiling capture -- any non-empty value
/// enables it, unset or empty disables it (same truthy convention as the
/// rest of Horizon's env-gated flags). Default off, per this module's
/// "no overhead on a normal run" constraint.
const ENABLE_VAR: &str = "HORIZON_UI_PROFILE";

/// Whether capture is enabled for this process, cached after the first
/// check: an env var read on every observed event would itself be overhead
/// on the disabled-by-default path this is designed to avoid.
pub(crate) fn is_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(ENABLE_VAR)
            .ok()
            .is_some_and(|value| !value.is_empty())
    })
}

/// Runs `f`, and -- only when [`is_enabled`] -- records how long it took
/// against `trigger`'s name. The `is_enabled` check is the only cost paid
/// on a normal (disabled) run: a cached bool read, no `Instant::now` call,
/// no channel send, no allocation.
pub(crate) fn timed<T>(trigger: &'static str, f: impl FnOnce() -> T) -> T {
    if !is_enabled() {
        return f();
    }
    let start = Instant::now();
    let result = f();
    record_event(trigger, start.elapsed());
    result
}

enum WriterCommand {
    Append(ProfileRecord),
    /// Lets a caller (only tests, today) block until every `Append` sent
    /// before this has been serialized and flushed to disk -- mirrors
    /// `crates/horizon-agent`'s event-log writer's own `flush`, simplified
    /// (no sequence numbers, no reply-carried error: this is best-effort
    /// telemetry, not durable session history).
    #[allow(dead_code)]
    Flush(Sender<()>),
}

fn writer() -> &'static Sender<WriterCommand> {
    static WRITER: OnceLock<Sender<WriterCommand>> = OnceLock::new();
    WRITER.get_or_init(spawn_writer)
}

/// Spawns the single background thread that owns the UI-profile log file
/// for this process. Lazily created on the first enabled `record_event`
/// call (not at startup), so a disabled run never opens the file or spawns
/// the thread at all.
fn spawn_writer() -> Sender<WriterCommand> {
    let (tx, rx) = channel::<WriterCommand>();
    let path = log_path();
    std::thread::Builder::new()
        .name("horizon-ui-profile-writer".to_string())
        .spawn(move || run_writer(&path, rx))
        .expect("spawn horizon-ui-profile-writer thread");
    tx
}

fn run_writer(path: &std::path::Path, rx: std::sync::mpsc::Receiver<WriterCommand>) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path);
    let mut file = match file {
        Ok(file) => file,
        Err(err) => {
            eprintln!(
                "horizon: failed to open UI profile log {} ({err}); capture disabled for this run",
                path.display()
            );
            return;
        }
    };

    for command in rx {
        match command {
            WriterCommand::Append(record) => {
                if let Ok(line) = serde_json::to_string(&record) {
                    let _ = writeln!(file, "{line}");
                    // Flushed immediately, same rationale as the agent event
                    // log's writer: a hard kill can only lose events that
                    // hadn't reached this thread yet, never ones already
                    // written. Not an `fsync` -- page-cache-level durability
                    // only, which is plenty for best-effort UI telemetry.
                    let _ = file.flush();
                }
            }
            WriterCommand::Flush(reply) => {
                let _ = file.flush();
                let _ = reply.send(());
            }
        }
    }
}

/// Sends one captured event to the background writer thread -- never
/// touches the filesystem on the calling (UI) thread itself, so timing the
/// UI thread's own event handling doesn't also time disk I/O. Never called
/// unless [`is_enabled`] (via [`timed`]).
fn record_event(trigger: &'static str, duration: Duration) {
    let created_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let record = ProfileRecord::new(trigger, duration, created_at_unix_ms);
    // The channel's receiver only disconnects if the writer thread failed
    // to open the file -- best-effort telemetry, so a dropped record here
    // isn't worth propagating as an error.
    let _ = writer().send(WriterCommand::Append(record));
}

/// Blocks until every event sent before this call has been written and
/// flushed. Test-only: production capture is fire-and-forget (see
/// [`record_event`]'s doc comment) -- nothing in the app itself needs to
/// wait for a write to land before reading it back, since the one reader
/// (`app::external_commands::dispatch_query`'s `"profile"` query) is fine
/// seeing a snapshot that doesn't yet include an event still in flight.
#[cfg(test)]
pub(crate) fn flush_for_test() {
    let (tx, rx) = channel();
    let _ = writer().send(WriterCommand::Flush(tx));
    let _ = rx.recv();
}
