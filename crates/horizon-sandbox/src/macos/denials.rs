//! macOS containment-denial recovery from the kernel's unified log.
//!
//! Linux learns what a sandboxed command needed from seccomp user-notify
//! (`crate::linux`'s supervised helper). On macOS, seatbelt denies silently:
//! the sandboxed process just observes an ordinary failure. The kernel does,
//! however, record every seatbelt violation in the unified log, e.g.
//!
//! ```text
//! 2026-09-10 13:06:05.618 E  kernel[0:4c65736] (Sandbox) Sandbox: security(89425) deny(1) mach-lookup com.apple.SecurityServer
//! ```
//!
//! [`DenialCollector`] recovers those records for one sandboxed run and
//! shapes them into the same [`crate::ContainmentDenials`] the Linux
//! supervisor produces, so the deny -> approve -> sandboxed-rerun loop works
//! unchanged across both platforms
//! (`docs/macos-containment-denial-reporting-design.md`).
//!
//! Attribution: records are accepted only when their pid is the sandboxed
//! root (the helper `exec()`s in place, so `SandboxedChild::child`'s pid is
//! the recorded pid) or a descendant sampled while the command ran. Every
//! record this module produces is `DenialEvidence::SeatbeltUnifiedLog` by
//! construction -- see `evidence.rs`'s authority criterion for what that
//! permits. The kernel coalesces duplicate violations ("31 duplicate
//! reports for ..."), so a denial can be summarized rather than individually
//! logged; anything still missed simply re-denies on the chained rerun.
//!
//! Collection is **live**, not a post-hoc store query: a process-shared
//! `log stream` subscription (see [`shared_stream`]) receives each record
//! the moment logd sees it, and `collect()` filters the run's window out of
//! the accumulated buffer. This replaced a `log show --last <window>` query
//! run at command exit (2026-09-10): the kernel's records reach the
//! *datastore* that `log show` reads only after an unbounded, load-dependent
//! delay (measured: a denial from 18:18:24 was still invisible to a query
//! run around 18:19, visible by 18:40), so an immediate post-exit query
//! raced the store and returned nothing -- exactly when commands failed.
//! The store path also degraded to minutes-long scans under the denial
//! flood every sandboxed command produces. `log stream`'s live delivery is
//! documented ("Stream live log messages") and was verified same-second on
//! the owner's machine; the datastore path carries no timing guarantee by
//! spec (`man log` documents loss events and a separate "inflight" data
//! state) and none in practice.

use crate::error::SandboxError;
use crate::policy::{
    ContainmentDenials, FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope,
    NetworkDenial, UngrantableDenial,
};
use std::collections::{HashSet, VecDeque};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// How often the descendant-pid sampler walks the process table while the
/// sandboxed command runs. Purely a discovery bound -- a grandchild that
/// spawns and exits between two samples is lost, which the chained retry
/// tolerates.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// Upper bound on the shared stream's record buffer. The kernel's sandbox
/// records are dominated by per-process noise (`/dev/dtracehelper` writes,
/// one to three per spawned process, so a cargo/clippy run emits hundreds
/// per minute); the cap bounds memory when a long build runs under a
/// collector. Drop-oldest: attribution only needs records from the current
/// run's window, and a real boundary crossing in the first seconds of a
/// half-hour build is not recoverable evidence anyway by the time it
/// finishes.
const STREAM_BUFFER_CAP: usize = 8192;

/// Report caps, mirroring the Linux report channel's bounded-output
/// discipline (`horizon-sandbox-runtime`'s drop-oldest overflow behavior).
const MAX_FILESYSTEM_DENIALS: usize = 128;
const MAX_UNGRANTABLE: usize = 64;
const MAX_NETWORK_DENIALS: usize = 128;
const MAX_MACH_SERVICES: usize = 32;

/// File operations the parser maps into filesystem denials. Everything else
/// (`syscall-unix`, `user-preference-read`, `system-fsctl`, ...) is ignored:
/// unmediated noise, either universal or not actionable through a grant.
const READ_OPERATIONS: [&str; 3] = ["file-read-data", "file-read-metadata", "file-ioctl"];
const WRITE_OPERATIONS: [&str; 3] = ["file-write-data", "file-write-metadata", "file-write-ioctl"];

/// Recovers one sandboxed run's containment denials from the unified log.
///
/// Started right after the sandboxed child spawns (with the child's pid and
/// the spawn instant); [`Self::collect`] is called once the child has
/// exited. Dropping the collector without collecting (the timeout path)
/// stops the sampler via `Drop`.
pub struct DenialCollector {
    root_pid: u32,
    started_at: SystemTime,
    /// Every pid attributed to this run so far: the root plus all
    /// descendants seen by any sample. Accumulate-only, so a process that
    /// reparents to launchd after its parent exits stays attributed.
    sampled: Arc<Mutex<HashSet<u32>>>,
    stop: Arc<AtomicBool>,
    sampler: Option<JoinHandle<()>>,
    /// The shared stream's generation when this run started, so `collect`
    /// can tell whether the subscription was alive for the whole window.
    stream_generation: u64,
    /// Why the shared stream could not be (re)started for this run, if it
    /// could not -- reported by `collect` as the soft-degrade annotation.
    stream_error: Option<String>,
}

impl DenialCollector {
    /// Starts the descendant sampler and makes sure the shared `log stream`
    /// subscription is alive. Never fails: if the sampler thread cannot
    /// spawn, attribution degrades to the root pid alone; if the stream
    /// cannot start, the error is carried to [`Self::collect`] (which turns
    /// it into the caller's soft-degrade annotation).
    pub fn start(root_pid: u32, started_at: SystemTime) -> Self {
        let sampled = Arc::new(Mutex::new(HashSet::from([root_pid])));
        let stop = Arc::new(AtomicBool::new(false));
        let sampler = std::thread::Builder::new()
            .name("macos-denial-pids".to_string())
            .spawn({
                let sampled = Arc::clone(&sampled);
                let stop = Arc::clone(&stop);
                move || {
                    while !stop.load(Ordering::Relaxed) {
                        if let Some(groups) = process_groups() {
                            if let Ok(mut attributed) = sampled.lock() {
                                mark_by_group(&groups, root_pid, &mut attributed);
                            }
                        }
                        std::thread::sleep(SAMPLE_INTERVAL);
                    }
                }
            })
            .ok();
        let (stream_generation, stream_error) = match ensure_stream_started() {
            Ok(generation) => (generation, None),
            Err(error) => (0, Some(error.to_string())),
        };
        Self {
            root_pid,
            started_at,
            sampled,
            stop,
            sampler,
            stream_generation,
            stream_error,
        }
    }

    /// Stops the sampler and shapes the live stream's records for this
    /// run's window and process tree into the same structure the Linux
    /// supervisor reports. Fails (soft-degrade at the caller) when the
    /// stream was not alive for the whole window: evidence that may be
    /// missing must say so rather than present absence as absence.
    pub fn collect(mut self) -> Result<ContainmentDenials, SandboxError> {
        self.stop_sampler();
        if let Some(error) = &self.stream_error {
            return Err(SandboxError::DenialReport(error.clone()));
        }
        let (records, generation, healthy) = stream_snapshot();
        if !healthy || generation != self.stream_generation {
            return Err(SandboxError::DenialReport(
                "the macOS denial stream was not alive for the whole of this command".to_string(),
            ));
        }
        let attributed_pids = self
            .sampled
            .lock()
            .map(|set| set.clone())
            .unwrap_or_else(|_| HashSet::from([self.root_pid]));
        Ok(denials_from_buffered(
            &records,
            &attributed_pids,
            self.started_at,
        ))
    }

    fn stop_sampler(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.sampler.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DenialCollector {
    fn drop(&mut self) {
        self.stop_sampler();
    }
}

/// The enforcement grants behind an approved (or config-declared) mach
/// service grant: `Read`/`File` grants on the keychain database files whose
/// presence makes nono's macOS profile skip its security-service
/// `mach-lookup` denies. Only database files that currently exist are
/// granted (best-effort, the same posture as the harness's default
/// grants); always empty outside macOS. The grant itself is the ordinary
/// filesystem-grant mechanism -- the service approval's *meaning* ("the
/// security/keychain service group, all-or-nothing") lives in the approval
/// request's reason text, not here.
pub fn security_service_grants(services: &[String]) -> Vec<FilesystemGrant> {
    let mut grants = Vec::new();
    if !services
        .iter()
        .any(|service| crate::KNOWN_SECURITY_SERVICES.contains(&service.as_str()))
    {
        return grants;
    }
    let mut candidates = Vec::new();
    if let Some(home) = crate::home_dir() {
        candidates.push(home.join("Library/Keychains/login.keychain-db"));
        candidates.push(home.join("Library/Keychains/metadata.keychain-db"));
    }
    candidates.push(PathBuf::from("/Library/Keychains/login.keychain-db"));
    candidates.push(PathBuf::from("/Library/Keychains/metadata.keychain-db"));
    for path in candidates {
        let Ok(resolved) = path.canonicalize() else {
            continue;
        };
        grants.push(FilesystemGrant {
            path: resolved,
            access: FilesystemGrantAccess::Read,
            scope: FilesystemGrantScope::File,
            excluded_subpaths: Vec::new(),
        });
    }
    grants
}

/// One parsed kernel record, held with the instant our reader received it.
/// Receipt time (not the record's own log timestamp) scopes the run window:
/// it is monotonic against `started_at` on the same clock and needs no
/// format parsing beyond what [`parse_record`] already did.
#[derive(Clone)]
struct BufferedRecord {
    received_at: SystemTime,
    record: LogRecord,
}

#[derive(Clone)]
struct LogRecord {
    pid: u32,
    operation: String,
    target: String,
}

/// Parses one compact unified-log line into the seatbelt record it carries,
/// anchored on the kernel's `Sandbox: <process>(<pid>) deny(<n>) <operation>
/// <target>` shape (captured 2026-09-10, see the design doc). Lines without
/// such a record -- headers, `System Policy:` App-Sandbox violations from
/// other applications, blank padding -- parse to `None`. Process names may
/// contain spaces (only the parenthesized pid is load-bearing); `rfind`
/// anchors on the *last* `Sandbox: ` marker so coalescing prefixes
/// ("7 duplicate reports for Sandbox: ...") parse as the record they
/// summarize.
fn parse_record(line: &str) -> Option<LogRecord> {
    let marker = line.rfind("Sandbox: ")?;
    let rest = &line[marker + "Sandbox: ".len()..];
    let open = rest.find('(')?;
    let close = open + rest[open..].find(')')?;
    let pid: u32 = rest[open + 1..close].parse().ok()?;
    let after_deny = rest[close + 1..].trim_start().strip_prefix("deny(")?;
    let close_deny = after_deny.find(')')?;
    let tail = after_deny[close_deny + 1..].trim_start();
    let (operation, target) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
    Some(LogRecord {
        pid,
        operation: operation.to_string(),
        target: target.trim().to_string(),
    })
}

/// Maps one attributed record into `denials`, mirroring the Linux
/// supervisor parse's error discipline
/// (`crate::linux`'s `containment_denials`): grantable attempts are deduped
/// into `filesystem`, ungrantable ones are carried as guidance rather than
/// dropping the report, and protected/unusable targets
/// (`SandboxError::UnsupportedGrantTarget` -- every process denies
/// `/dev/dtracehelper` writes) are silently not grant candidates.
fn apply_record(record: LogRecord, denials: &mut ContainmentDenials) {
    let access = if READ_OPERATIONS.contains(&record.operation.as_str()) {
        Some(FilesystemGrantAccess::Read)
    } else if WRITE_OPERATIONS.contains(&record.operation.as_str()) {
        Some(FilesystemGrantAccess::ReadWrite)
    } else {
        None
    };
    if let Some(access) = access {
        match crate::grant::resolve_denial(PathBuf::from(&record.target), access) {
            Ok(denial) => {
                if denials.filesystem.len() < MAX_FILESYSTEM_DENIALS
                    && !denials.filesystem.contains(&denial)
                {
                    denials.filesystem.push(denial);
                }
            }
            // One ungrantable attempt must not cost the grantable ones its
            // report also carries (the Linux supervisor parse's own
            // discipline), and its Display *is* the guidance: it names the
            // refusal and the supported alternative ($TMPDIR).
            Err(error) => {
                if let SandboxError::UngrantableDenial { attempted, .. } = &error {
                    let entry = UngrantableDenial {
                        attempted_path: attempted.clone(),
                        guidance: error.to_string(),
                    };
                    if denials.ungrantable.len() < MAX_UNGRANTABLE
                        && !denials.ungrantable.contains(&entry)
                    {
                        denials.ungrantable.push(entry);
                    }
                }
                // `UnsupportedGrantTarget` (protected paths like
                // /dev/dtracehelper, denied by every process) and anything
                // else are simply not grant candidates.
            }
        }
        return;
    }
    if record.operation == "mach-lookup" {
        let Some(service) = record.target.split_whitespace().next() else {
            return;
        };
        let service = service.to_string();
        if denials.mach_services.len() < MAX_MACH_SERVICES
            && !denials.mach_services.contains(&service)
        {
            denials.mach_services.push(service.clone());
        }
        let denial = NetworkDenial {
            target: service,
            operation: record.operation,
            reason: "refused by the seatbelt sandbox (kernel unified-log record)".to_string(),
        };
        if denials.network.len() < MAX_NETWORK_DENIALS && !denials.network.contains(&denial) {
            denials.network.push(denial);
        }
    }
}

/// One snapshot of the system process table as `(pid, pgid)` pairs, or
/// `None` when it cannot be read this tick (the sampler treats that as
/// "nothing new"; the next tick retries). Shelled out to `ps` rather than
/// walking `sysctl(KERN_PROC)`: `kinfo_proc`'s layout is not exposed by the
/// `libc` crate on macOS, and reconstructing it by hand couples the
/// collector to Apple's ABI for no benefit -- a group snapshot is the same
/// information without the struct.
fn process_groups() -> Option<Vec<(u32, u32)>> {
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut groups = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut tokens = line.split_whitespace();
        let (Some(pid), Some(pgid)) = (tokens.next(), tokens.next()) else {
            continue;
        };
        if let (Ok(pid), Ok(pgid)) = (pid.parse::<u32>(), pgid.parse::<u32>()) {
            groups.push((pid, pgid));
        }
    }
    Some(groups)
}

/// Marks every process whose group is the sandboxed root's as attributed.
/// The sandboxed spawn is a process-group leader (`macos::spawn_with_grants`
/// sets `process_group(0)`), and every descendant inherits that group at
/// spawn -- including orphans, which keep their group after reparenting to
/// launchd. Accumulate-only on the caller's set.
fn mark_by_group(groups: &[(u32, u32)], root_pid: u32, attributed: &mut HashSet<u32>) {
    for (pid, pgid) in groups {
        if *pgid == root_pid {
            attributed.insert(*pid);
        }
    }
}

/// The process-shared `log stream` subscription backing every collector in
/// this process.
///
/// One subscription per process (per agentd), not per command: attaching a
/// fresh `log stream` costs tens of milliseconds, which is the same order
/// as a fast-failing command's whole life -- the exact case this recovery
/// exists for. Attached once, it is already subscribed before any command
/// spawns, so a denial is received by the reader in the same instant logd
/// sees it. This also takes the *datastore* -- the delayed, lossy path
/// `log show` reads -- entirely off the collection critical path.
struct SharedStream {
    inner: Mutex<StreamState>,
}

#[derive(Default)]
struct StreamState {
    records: VecDeque<BufferedRecord>,
    /// Bumped on every successful (re)spawn. A collector captures it at
    /// start and requires it unchanged at collect, so a stream that died
    /// mid-run fails the run's collection rather than reporting a silent
    /// gap as absence.
    generation: u64,
    healthy: bool,
    /// The subscription's child. Reaped on respawn (the reader is the only
    /// side that knows it exited); when this process exits, the pipe the
    /// child writes to closes and its EPIPE death reaps the subscription
    /// without us.
    child: Option<Child>,
}

/// The live subscription's predicate: seatbelt denials from the kernel,
/// identical to the retired `log show` query's (and to the shape
/// [`parse_record`] anchors on, verified against live output 2026-09-10).
const STREAM_PREDICATE: &str = "sender == \"kernel\" AND eventMessage CONTAINS \"Sandbox:\"";

fn shared_stream() -> &'static SharedStream {
    static STREAM: LazyLock<SharedStream> = LazyLock::new(|| SharedStream {
        inner: Mutex::new(StreamState::default()),
    });
    &STREAM
}

/// Makes sure the shared subscription is running, returning its current
/// generation. Idempotent while healthy; respawns after the reader reports
/// the child gone (logd restart, crash). The spawn must not be sandboxed
/// -- `log` refuses under seatbelt ("Cannot run while sandboxed"), which
/// holds for the collector's host (agentd) and is exactly why this lives
/// host-side rather than in the sandboxed child.
fn ensure_stream_started() -> Result<u64, SandboxError> {
    let shared = shared_stream();
    let mut state = shared
        .inner
        .lock()
        .map_err(|_| SandboxError::DenialReport("denial stream state poisoned".to_string()))?;
    if state.healthy {
        return Ok(state.generation);
    }
    // The previous child is dead (the reader only clears `healthy` on its
    // exit); reap it before replacing the handle.
    if let Some(mut dead) = state.child.take() {
        let _ = dead.wait();
    }
    let mut child = std::process::Command::new("/usr/bin/log")
        .args([
            "stream",
            "--style",
            "compact",
            "--predicate",
            STREAM_PREDICATE,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| {
            SandboxError::DenialReport(format!("failed to run `log stream`: {error}"))
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SandboxError::DenialReport("`log stream` stdout was not captured".into()))?;
    state.generation += 1;
    state.healthy = true;
    state.child = Some(child);
    let generation = state.generation;
    // The reader takes the same lock per record; holding ours across the
    // spawn only ever blocks it briefly, never cycles.
    let reader_spawned = std::thread::Builder::new()
        .name("macos-denial-stream".to_string())
        .spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break; // child exited -- the writer's side is gone
                };
                let Some(record) = parse_record(&line) else {
                    // Banner, column header, `System Policy:` lines from
                    // other applications -- parse_record's documented no's.
                    continue;
                };
                if let Ok(mut state) = shared_stream().inner.lock() {
                    // The shared buffer also carries other runs' and system
                    // daemons' records; drop-oldest keeps it bounded and the
                    // per-run attribution at collect time is what keeps
                    // foreign records out of any report.
                    if state.records.len() >= STREAM_BUFFER_CAP {
                        state.records.pop_front();
                    }
                    state.records.push_back(BufferedRecord {
                        received_at: SystemTime::now(),
                        record,
                    });
                }
            }
            if let Ok(mut state) = shared_stream().inner.lock() {
                state.healthy = false;
                // Reap before the handle is replaced (the reader is the only
                // side that knows the child exited).
                if let Some(mut dead) = state.child.take() {
                    let _ = dead.wait();
                }
            }
        });
    if let Err(error) = reader_spawned {
        // The child is subscribed but nothing would ever drain it; take the
        // subscription down with us rather than report a half-attached
        // stream as healthy. Same lock, same thread -- no re-entry.
        state.healthy = false;
        if let Some(mut child) = state.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        return Err(SandboxError::DenialReport(format!(
            "failed to spawn the denial stream reader: {error}"
        )));
    }
    Ok(generation)
}

/// A consistent snapshot of the shared buffer plus the subscription's
/// liveness, taken under one lock.
fn stream_snapshot() -> (Vec<BufferedRecord>, u64, bool) {
    match shared_stream().inner.lock() {
        Ok(state) => (
            state.records.iter().cloned().collect(),
            state.generation,
            state.healthy,
        ),
        Err(_) => (Vec::new(), 0, false),
    }
}

/// Folds the run's window of buffered records, restricted to the run's
/// attributed pids, into the Linux-shaped denial report. Free-standing so
/// the window/attribution rules are testable without a subscription.
fn denials_from_buffered(
    records: &[BufferedRecord],
    attributed_pids: &HashSet<u32>,
    started_at: SystemTime,
) -> ContainmentDenials {
    let mut denials = ContainmentDenials::default();
    for buffered in records {
        if buffered.received_at < started_at {
            continue;
        }
        if !attributed_pids.contains(&buffered.record.pid) {
            continue;
        }
        apply_record(buffered.record.clone(), &mut denials);
    }
    denials
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECURITY_LOOKUP: &str = "2026-09-10 13:06:05.618 E  kernel[0:4c65736] (Sandbox) Sandbox: security(89425) deny(1) mach-lookup com.apple.SecurityServer";

    #[test]
    fn parses_the_verified_kernel_record_shape() {
        let record = parse_record(SECURITY_LOOKUP).expect("parses");
        assert_eq!(record.pid, 89425);
        assert_eq!(record.operation, "mach-lookup");
        assert_eq!(record.target, "com.apple.SecurityServer");
    }

    #[test]
    fn parses_file_operations_and_duplicate_report_prefixes() {
        let write = parse_record(
            "2026-09-10 13:06:05.613 E  kernel[0:4c663ad] (Sandbox) Sandbox: security(89425) deny(1) file-write-data /dev/dtracehelper",
        )
        .expect("parses");
        assert_eq!(write.operation, "file-write-data");
        assert_eq!(write.target, "/dev/dtracehelper");

        let coalesced = parse_record(
            "2026-09-10 13:06:05.607 E  kernel[0:4c663ab] (Sandbox) 31 duplicate reports for Sandbox: sharingd(695) deny(1) syscall-unix 545",
        )
        .expect("parses");
        assert_eq!(coalesced.pid, 695);
        assert_eq!(coalesced.operation, "syscall-unix");
        assert_eq!(coalesced.target, "545");
    }

    #[test]
    fn app_sandbox_policy_lines_and_headers_do_not_parse() {
        // App Sandbox (System Policy) violations from other applications are
        // not seatbelt records from our sandbox.
        assert!(parse_record(
            "2026-09-10 13:06:05 E  kernel[0:x] (Sandbox) System Policy: com.crowdstrike.falcon.Agent(70959) deny(1) file-read-data /Library"
        )
        .is_none());
        assert!(
            parse_record("2026-09-10 13:06:05 E  kernel[0:x] (Sandbox) === log show ===").is_none()
        );
    }

    #[test]
    fn process_names_with_spaces_do_not_confuse_the_pid() {
        let record = parse_record(
            "2026-09-10 13:06:05 E  kernel[0:x] (Sandbox) Sandbox: Google Chrome(4242) deny(1) mach-lookup com.apple.SecurityServer",
        )
        .expect("parses");
        assert_eq!(record.pid, 4242);
        assert_eq!(record.target, "com.apple.SecurityServer");
    }

    #[test]
    fn mach_lookups_become_service_denials_and_network_records() {
        let mut denials = ContainmentDenials::default();
        apply_record(parse_record(SECURITY_LOOKUP).expect("parses"), &mut denials);
        assert_eq!(denials.mach_services, vec!["com.apple.SecurityServer"]);
        assert_eq!(denials.network.len(), 1);
        assert_eq!(denials.network[0].target, "com.apple.SecurityServer");
        assert_eq!(denials.network[0].operation, "mach-lookup");
        assert!(denials.filesystem.is_empty());
    }

    #[test]
    fn protected_file_targets_are_dropped_without_costing_the_report() {
        let mut denials = ContainmentDenials::default();
        apply_record(
            parse_record(
                "2026-09-10 13:06:05 E  kernel[0:x] (Sandbox) Sandbox: bash(89424) deny(1) file-write-data /dev/dtracehelper",
            )
            .expect("parses"),
            &mut denials,
        );
        assert!(denials.filesystem.is_empty());
        assert!(denials.ungrantable.is_empty());
        // The record's operation mapped, but resolve_denial refused the
        // protected target -- nothing leaked into any list.
    }

    #[test]
    fn grantable_file_targets_resolve_through_resolve_denial() {
        let scratch =
            std::env::temp_dir().join(format!("horizon-denials-test-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("mkdir");
        let target = scratch.join("denied-file.txt");
        let mut denials = ContainmentDenials::default();
        apply_record(
            parse_record(&format!(
                "2026-09-10 13:06:05 E  kernel[0:x] (Sandbox) Sandbox: bash(89424) deny(1) file-read-data {}",
                target.display()
            ))
            .expect("parses"),
            &mut denials,
        );
        std::fs::remove_dir_all(&scratch).ok();
        assert_eq!(denials.filesystem.len(), 1);
        assert_eq!(denials.filesystem[0].attempted_path, target);
        assert_eq!(
            denials.filesystem[0].grant.access,
            FilesystemGrantAccess::Read
        );
    }

    #[test]
    fn security_service_grants_require_a_known_service_and_existing_dbs() {
        assert!(security_service_grants(&["com.apple.apsd".to_string()]).is_empty());
        let grants = security_service_grants(&["com.apple.SecurityServer".to_string()]);
        // On a real macOS account at least the user keychain database
        // exists; this module is cfg'd to macOS, so Linux CI never runs it.
        let user_keychain_exists = std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join("Library/Keychains/login.keychain-db"))
            .is_some_and(|path| path.exists());
        if user_keychain_exists {
            assert!(!grants.is_empty());
            for grant in &grants {
                assert_eq!(grant.access, FilesystemGrantAccess::Read);
                assert_eq!(grant.scope, FilesystemGrantScope::File);
            }
        }
    }

    #[test]
    fn processes_in_the_sandbox_group_are_marked() {
        // The sandboxed root is a group leader; descendants inherit the
        // group, and orphans keep it after reparenting to launchd.
        let groups = vec![(1u32, 1u32), (10u32, 10u32), (11u32, 10u32), (12u32, 10u32)];
        let mut attributed = HashSet::from([10u32]);
        mark_by_group(&groups, 10, &mut attributed);
        assert!(attributed.contains(&11));
        assert!(attributed.contains(&12));
        assert!(!attributed.contains(&1));
    }

    #[test]
    fn process_table_snapshot_is_available() {
        // The sampler's data source works on a real host.
        let groups = process_groups().expect("process table readable");
        assert!(!groups.is_empty());
        assert!(groups.iter().any(|(pid, _)| *pid == std::process::id()));
    }

    #[test]
    fn stream_banner_and_column_headers_do_not_parse() {
        // `log stream`'s own preamble lines, as emitted on the live pipe
        // (captured 2026-09-10). parse_record's rfind anchor makes the
        // banner's quoted predicate fall out on its own; the column header
        // has no record shape at all.
        assert!(parse_record(
            "Filtering the log data using \"composedMessage CONTAINS \"Sandbox:\"\""
        )
        .is_none());
        assert!(parse_record("Timestamp               Ty Process[PID:TID]").is_none());
    }

    #[test]
    fn buffered_records_filter_by_window_and_attribution() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let attributed = HashSet::from([7u32]);
        let buffered = |at: u64, pid: u32| BufferedRecord {
            received_at: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
            record: LogRecord {
                pid,
                operation: "mach-lookup".to_string(),
                target: "com.apple.SecurityServer".to_string(),
            },
        };
        let records = vec![
            buffered(999, 7),   // received before the run's window -> out
            buffered(1_001, 7), // in-window, attributed -> in
            buffered(1_001, 9), // in-window, foreign pid -> out
            buffered(2_000, 7), // trailing record, same attributed pid -> in
        ];
        let denials = denials_from_buffered(&records, &attributed, started);
        assert_eq!(denials.mach_services, vec!["com.apple.SecurityServer"]);
        assert_eq!(denials.network.len(), 1);
        assert!(denials.filesystem.is_empty());
    }
}
