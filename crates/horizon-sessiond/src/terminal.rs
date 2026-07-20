use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use horizon_terminal_core::{
    compute_frame_diff, run_terminal_core, CoreReceivers, CoreSenders, SelectionCommand,
    TerminalCommand, TerminalCoreOptions, TerminalFrame, TerminalSpawnSpec, TerminalSummary,
    TerminalUpdate,
};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

/// How long [`TerminalHost::create`] waits for a PTY spawn before reporting
/// an error instead of blocking forever -- see that method's doc comment
/// for the suspected `portable-pty` fork-safety hazard this guards
/// against.
const TERMINAL_SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub(crate) struct TerminalHost {
    sessions: Arc<Mutex<HashMap<Uuid, HostedTerminal>>>,
    /// One live subscriber per attached terminal session — installed by
    /// the hub's `create_terminal`/`attach_terminal` (a re-attach
    /// replaces), removed when the session exits or the subscriber's
    /// bridge dies. Replaces the JSONL era's per-connection
    /// `attached`/`baselines` maps: the baseline is per-*attachment* state
    /// now, carried by the subscriber itself.
    subscribers: Arc<Mutex<HashMap<Uuid, Subscriber>>>,
}

#[derive(Clone)]
struct HostedTerminal {
    command_tx: Sender<TerminalCommand>,
    latest_frame: Arc<Mutex<Option<TerminalFrame>>>,
    pid: Option<u32>,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
}

/// The local half of one attachment's update bridge: an unbounded,
/// sync-sendable queue the PTY-side threads push into; the hub's async
/// pump drains it into the attachment's remote channel. `baseline` is the
/// last frame delivered to *this* attachment — the diff base for the next
/// one, established by the seeding `Snapshot`.
struct Subscriber {
    updates: UnboundedSender<TerminalUpdate>,
    baseline: Option<TerminalFrame>,
}

impl TerminalHost {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Whether a live terminal session with this id exists — the hub's
    /// `attach_terminal` not-found check.
    pub(crate) fn has_session(&self, session_id: Uuid) -> bool {
        self.sessions.lock().unwrap().contains_key(&session_id)
    }

    /// Every live terminal session, sorted by id — the hub's
    /// `list_terminals` body (the request-id correlation the JSONL `List`
    /// control needed is gone; the rtc call returns this directly).
    pub(crate) fn list(&self) -> Vec<TerminalSummary> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap()
            .keys()
            .copied()
            .map(|session_id| TerminalSummary { session_id })
            .collect::<Vec<_>>();
        sessions.sort_unstable_by_key(|summary| summary.session_id);
        sessions
    }

    /// Installs a fresh subscriber for `session_id` (replacing any
    /// previous attachment's) and returns the local receiving half the hub
    /// pumps into the attachment's remote channel. If the session already
    /// has a retained latest frame, it is delivered immediately as the
    /// seeding `Snapshot` and becomes the diff baseline — the same attach
    /// contract the JSONL wire had (attach result, then snapshot, then
    /// diffs).
    pub(crate) fn subscribe(&self, session_id: Uuid) -> UnboundedReceiver<TerminalUpdate> {
        let (tx, rx) = unbounded_channel();
        let latest = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|session| session.latest_frame.clone());
        let latest = latest.and_then(|frame| frame.lock().unwrap().clone());
        let mut subscriber = Subscriber {
            updates: tx,
            baseline: None,
        };
        if let Some(frame) = latest {
            let _ = subscriber
                .updates
                .send(TerminalUpdate::Snapshot(frame.clone()));
            subscriber.baseline = Some(frame);
        }
        self.subscribers
            .lock()
            .unwrap()
            .insert(session_id, subscriber);
        rx
    }

    /// Removes `session_id`'s subscriber — the hub's cleanup when a
    /// `create_terminal` fails after having subscribed optimistically.
    pub(crate) fn unsubscribe(&self, session_id: Uuid) {
        self.subscribers.lock().unwrap().remove(&session_id);
    }

    /// Drops every subscriber — called when the client connection ends, so
    /// PTY-side threads stop paying for sends into bridges whose pumps are
    /// gone (they would be lazily dropped on first failed send anyway).
    pub(crate) fn clear_subscribers(&self) {
        self.subscribers.lock().unwrap().clear();
    }

    pub(crate) fn handle_command(&self, session_id: Uuid, command: TerminalCommand) {
        let sender = self
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|session| session.command_tx.clone());
        if let Some(sender) = sender {
            let _ = sender.send(command);
        }
    }

    pub(crate) fn shutdown_all(&self) {
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .map(|session| (session.command_tx.clone(), session.killer.clone()))
            .collect::<Vec<_>>();
        for (sender, killer) in sessions {
            let _ = killer.lock().unwrap().kill();
            let _ = sender.send(TerminalCommand::Shutdown);
        }
    }

    /// Spawns a PTY for `session_id`, retrying a bounded number of times if
    /// an individual attempt doesn't report back within [`TERMINAL_SPAWN_
    /// TIMEOUT`], and installs the session on the first successful attempt.
    ///
    /// This exists because of a suspected (code-verified, but not captured
    /// live -- see `docs/tasks/backlog.md`'s entry on it) hazard in
    /// `portable-pty` 0.9.0: its `spawn_command` sets a `pre_exec` closure
    /// that calls `close_random_fds`, which does a heap allocation
    /// (`std::fs::read_dir`) in the forked child *before* `exec` -- not
    /// fork-safe in a process this multi-threaded. If another thread held
    /// e.g. glibc's malloc lock at the instant of `fork`, the child
    /// inherits it permanently locked and can never reach `exec`; `std::
    /// process::Command::spawn` blocks the *calling* thread until the
    /// child execs or reports failure, so a wedged child would wedge the
    /// caller too, forever, with no way to interrupt it from the outside.
    /// During this fix's own validation, the same terminal-creation call
    /// was observed hanging past even a 120s ceiling under contention, at
    /// a failure rate too high (up to ~20% of runs under sustained load)
    /// to treat as negligible or purely a test artifact -- but a follow-up
    /// run instrumented with fine-grained diagnostics did not reproduce it
    /// live, so the exact mechanism remains a well-evidenced hypothesis,
    /// not a confirmed root cause.
    ///
    /// Regardless of the precise mechanism, bounding this call is correct
    /// on its own merits: nothing should let one connection's terminal
    /// creation block that connection's entire message loop forever. Each
    /// attempt runs on its own thread with a bounded wait -- there is no
    /// API to cancel a blocked `Command::spawn`, so a timed-out attempt's
    /// thread (and, if it's genuinely wedged, the half-started child
    /// process) is deliberately abandoned rather than joined. If the
    /// hazard above is the real cause, a *fresh* attempt (a new `fork` at
    /// a different instant) has good odds of avoiding the same collision,
    /// so retrying should convert an occasional failure into a rarer one
    /// without ever surfacing it to the caller.
    ///
    /// Install is **first-wins**: if an abandoned attempt does eventually
    /// succeed after a later retry already installed a session for this
    /// `session_id`, the late duplicate is killed and discarded (see the
    /// `Entry::Occupied` branch below) rather than overwriting the
    /// winning `HostedTerminal` or having both threads call
    /// `forward_updates` for the same id, which would interleave two
    /// different shells' output into one pane. `HostedTerminal` has no
    /// `Drop` impl -- it's a cheap, `Clone`-able handle, not an owner --
    /// so a discarded session's real child process and background
    /// threads are killed/signalled explicitly rather than relying on it
    /// going out of scope. One edge case needs no extra code, just
    /// awareness: if *every* attempt has already timed out and this
    /// method has reported the final error to the client, a late success
    /// still wins the (now-empty) entry and installs normally -- the pane
    /// saw an error, but the session is alive daemon-side and recoverable
    /// via Manage Sessions, which is an acceptable outcome.
    pub(crate) fn create(&self, session_id: Uuid, spec: TerminalSpawnSpec) -> Result<(), String> {
        if self.sessions.lock().unwrap().contains_key(&session_id) {
            // A create against an id that already runs degrades to attach
            // semantics: the caller's `subscribe` (installed before this
            // ran) already seeded the latest snapshot.
            return Ok(());
        }
        let cwd = self.resolve_cwd(&spec);

        const MAX_SPAWN_ATTEMPTS: u32 = 3;
        for attempt in 1..=MAX_SPAWN_ATTEMPTS {
            let host = self.clone();
            let spec_for_thread = spec.clone();
            let cwd_for_thread = cwd.clone();
            let (result_tx, result_rx) = crossbeam_channel::bounded(1);
            thread::spawn(move || {
                match spawn_terminal(session_id, &spec_for_thread, &cwd_for_thread) {
                    Ok((session, update_rx)) => {
                        if host.install_if_vacant(session_id, session) {
                            let _ = result_tx.send(Ok(()));
                            host.forward_updates(session_id, update_rx);
                        }
                        // Occupied: a different attempt (an earlier one that
                        // timed out here but finished late, or a fresh
                        // `create` for the same id that raced this one) has
                        // already won -- `install_if_vacant` already killed
                        // and discarded this duplicate, so there is nothing
                        // left to do with `update_rx` but let it drop.
                    }
                    Err(error) => {
                        let _ = result_tx.send(Err(error.to_string()));
                    }
                }
            });

            match result_rx.recv_timeout(TERMINAL_SPAWN_TIMEOUT) {
                Ok(Ok(())) => return Ok(()),
                // A real spawn error (bad shell, permissions, ...), not a
                // hang -- retrying won't help, report it immediately. What
                // the JSONL wire delivered as a `TerminalUpdate::Error` is
                // the create call's own error result now.
                Ok(Err(error)) => return Err(error),
                Err(_timeout) => {
                    eprintln!(
                        "horizon-sessiond: terminal spawn attempt {attempt}/\
                         {MAX_SPAWN_ATTEMPTS} for {session_id} did not report back within \
                         {TERMINAL_SPAWN_TIMEOUT:?}; retrying with a fresh attempt"
                    );
                }
            }
        }

        Err(format!(
            "terminal failed to start after {MAX_SPAWN_ATTEMPTS} attempts (this is rare; \
             retrying the command usually works)"
        ))
    }

    /// The first-wins decision [`Self::create`]'s spawn threads share: installs
    /// `session` for `session_id` and returns `true` if no session was
    /// already installed for that id, or discards `session` (killing its
    /// real child process and telling its background loop to shut down --
    /// see the note on `HostedTerminal` having no `Drop` impl) and returns
    /// `false` if one already was. The lock is held only long enough to
    /// decide; the losing session's teardown happens after releasing it.
    fn install_if_vacant(&self, session_id: Uuid, session: HostedTerminal) -> bool {
        let discarded = {
            let mut sessions = self.sessions.lock().unwrap();
            match sessions.entry(session_id) {
                Entry::Vacant(entry) => {
                    entry.insert(session);
                    None
                }
                Entry::Occupied(_) => Some(session),
            }
        };
        match discarded {
            None => true,
            Some(discarded) => {
                eprintln!(
                    "horizon-sessiond: discarding a late duplicate terminal spawn for \
                     {session_id} (an earlier attempt already won)"
                );
                let _ = discarded.killer.lock().unwrap().kill();
                let _ = discarded.command_tx.send(TerminalCommand::Shutdown);
                false
            }
        }
    }

    fn resolve_cwd(&self, spec: &TerminalSpawnSpec) -> PathBuf {
        spec.spawn_source_session_id
            .and_then(|source| {
                self.sessions
                    .lock()
                    .unwrap()
                    .get(&source)
                    .and_then(|session| session.pid)
            })
            .and_then(sample_cwd)
            .unwrap_or_else(|| spec.fallback_cwd.clone())
    }

    /// Pushes one non-frame update to `session_id`'s subscriber, if any,
    /// dropping the subscriber entry when its bridge is gone.
    fn send_update(&self, session_id: Uuid, update: TerminalUpdate) {
        let mut subscribers = self.subscribers.lock().unwrap();
        if subscribers
            .get(&session_id)
            .is_some_and(|subscriber| subscriber.updates.send(update).is_err())
        {
            subscribers.remove(&session_id);
        }
    }

    fn forward_updates(&self, session_id: Uuid, update_rx: Receiver<TerminalUpdate>) {
        let host = self.clone();
        thread::spawn(move || {
            while let Ok(update) = update_rx.recv() {
                match update {
                    TerminalUpdate::Snapshot(frame) => {
                        if let Some(session) = host.sessions.lock().unwrap().get(&session_id) {
                            *session.latest_frame.lock().unwrap() = Some(frame.clone());
                        }
                        let mut subscribers = host.subscribers.lock().unwrap();
                        let Some(subscriber) = subscribers.get_mut(&session_id) else {
                            continue;
                        };
                        // v10 keeps the frame-delivery semantics unchanged
                        // (docs/remoc-adoption-design.md par.6 step 2):
                        // snapshot to a baseline-less attachment, diff
                        // against the per-attachment baseline otherwise.
                        let update = match subscriber.baseline.take() {
                            Some(old) => {
                                TerminalUpdate::FrameDiff(compute_frame_diff(&old, &frame))
                            }
                            None => TerminalUpdate::Snapshot(frame.clone()),
                        };
                        if subscriber.updates.send(update).is_ok() {
                            subscriber.baseline = Some(frame);
                        } else {
                            subscribers.remove(&session_id);
                        }
                    }
                    TerminalUpdate::FrameDiff(_) => {}
                    TerminalUpdate::Exited => {
                        host.send_update(session_id, TerminalUpdate::Exited);
                        host.sessions.lock().unwrap().remove(&session_id);
                        host.subscribers.lock().unwrap().remove(&session_id);
                        return;
                    }
                    other => host.send_update(session_id, other),
                }
            }
        });
    }
}

fn spawn_terminal(
    session_id: Uuid,
    spec: &TerminalSpawnSpec,
    cwd: &Path,
) -> anyhow::Result<(HostedTerminal, Receiver<TerminalUpdate>)> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: spec.initial_size.rows,
        cols: spec.initial_size.cols,
        pixel_width: spec.initial_size.pixel_width,
        pixel_height: spec.initial_size.pixel_height,
    })?;

    let mut command = CommandBuilder::new(&spec.shell);
    command.args(&spec.args);
    command.env("TERM", &spec.term);
    // Pairs with the fixed `xterm-256color` TERM (see `workspace.rs`'s
    // `terminal_spawn_spec`, the shell crate) so truecolor detection works
    // in tools that gate on COLORTERM rather than TERM alone -- only when
    // the inherited environment doesn't already set it, so an already
    // truecolor-aware launch environment (or an explicit override) is
    // never clobbered.
    if std::env::var_os("COLORTERM").is_none() {
        command.env("COLORTERM", "truecolor");
    }
    command.env("HORIZON_SOCKET", &spec.control_socket);
    command.env("HORIZON_SESSION_ID", session_id.to_string());
    command.cwd(cwd);
    let child = pair.slave.spawn_command(command)?;
    let pid = child.process_id();
    let killer = Arc::new(Mutex::new(child.clone_killer()));
    drop(child);
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    let (command_tx, command_rx) = crossbeam_channel::unbounded();
    let (update_tx, update_rx) = crossbeam_channel::unbounded();
    let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
    let (resize_tx, resize_rx) = crossbeam_channel::unbounded();
    let (scroll_tx, scroll_rx) = crossbeam_channel::unbounded();
    let (mouse_tx, mouse_rx) = crossbeam_channel::unbounded();
    let (paste_tx, paste_rx) = crossbeam_channel::unbounded();
    let (key_tx, key_rx) = crossbeam_channel::unbounded();
    let (selection_tx, selection_rx) = crossbeam_channel::unbounded();
    let (focus_tx, focus_rx) = crossbeam_channel::unbounded();
    let (color_scheme_tx, color_scheme_rx) = crossbeam_channel::unbounded();

    let response_tx = command_tx.clone();
    let read_update_tx = update_tx.clone();
    thread::spawn(move || read_pty(&mut *reader, pty_tx, read_update_tx));
    let size = spec.initial_size;
    let options = TerminalCoreOptions {
        scrollback_lines: spec.scrollback_lines,
        color_scheme: spec.color_scheme,
    };
    thread::spawn(move || {
        run_terminal_core(
            size,
            options,
            pty_rx,
            CoreReceivers {
                resize_rx,
                scroll_rx,
                mouse_rx,
                paste_rx,
                key_rx,
                selection_rx,
                focus_rx,
                color_scheme_rx,
            },
            response_tx,
            update_tx,
        );
    });
    let writer_killer = killer.clone();
    thread::spawn(move || {
        run_writer(
            master,
            &mut *writer,
            writer_killer,
            command_rx,
            CoreSenders {
                resize_tx,
                scroll_tx,
                mouse_tx,
                paste_tx,
                key_tx,
                selection_tx,
                focus_tx,
                color_scheme_tx,
            },
        );
    });

    Ok((
        HostedTerminal {
            command_tx,
            latest_frame: Arc::new(Mutex::new(None)),
            pid,
            killer,
        },
        update_rx,
    ))
}

fn read_pty(reader: &mut dyn Read, pty_tx: Sender<Vec<u8>>, update_tx: Sender<TerminalUpdate>) {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
            Ok(read) => {
                if pty_tx.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = update_tx.send(TerminalUpdate::Error(error.to_string()));
                let _ = update_tx.send(TerminalUpdate::Exited);
                return;
            }
        }
    }
}

fn run_writer(
    master: Box<dyn MasterPty + Send>,
    writer: &mut dyn Write,
    killer: Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>,
    command_rx: Receiver<TerminalCommand>,
    senders: CoreSenders,
) {
    let CoreSenders {
        resize_tx,
        scroll_tx,
        mouse_tx,
        paste_tx,
        key_tx,
        selection_tx,
        focus_tx,
        color_scheme_tx,
    } = senders;
    let mut last_size = None;
    while let Ok(command) = command_rx.recv() {
        match command {
            TerminalCommand::Input(bytes) => {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
            TerminalCommand::Key {
                key,
                modifiers,
                event,
            } => {
                let _ = key_tx.send((key, modifiers, event));
            }
            TerminalCommand::Paste(text) => {
                let _ = paste_tx.send(text);
            }
            TerminalCommand::Resize(size) => {
                if last_size != Some(size) {
                    last_size = Some(size);
                    let _ = master.resize(PtySize {
                        rows: size.rows,
                        cols: size.cols,
                        pixel_width: size.pixel_width,
                        pixel_height: size.pixel_height,
                    });
                }
                let _ = resize_tx.send(size);
            }
            TerminalCommand::Scroll(scroll) => {
                let _ = scroll_tx.send(scroll);
            }
            TerminalCommand::Mouse(report) => {
                let _ = mouse_tx.send(report);
            }
            TerminalCommand::SelectionStart { point, kind } => {
                let _ = selection_tx.send(SelectionCommand::Start { point, kind });
            }
            TerminalCommand::SelectionUpdate(point) => {
                let _ = selection_tx.send(SelectionCommand::Update(point));
            }
            TerminalCommand::CopySelection => {
                let _ = selection_tx.send(SelectionCommand::Copy);
            }
            TerminalCommand::Focus(focused) => {
                let _ = focus_tx.send(focused);
            }
            TerminalCommand::SetColorScheme(scheme) => {
                let _ = color_scheme_tx.send(scheme);
            }
            TerminalCommand::Shutdown => {
                let _ = killer.lock().unwrap().kill();
                return;
            }
            // Skew catch-all (`TerminalCommand::Unknown`'s doc): a command
            // this build can't name is logged and dropped -- never written
            // to the PTY, never guessed at.
            TerminalCommand::Unknown => {
                eprintln!("horizon-sessiond: ignoring unknown terminal command from a newer peer");
            }
        }
    }
}

fn sample_cwd(pid: u32) -> Option<PathBuf> {
    let sysinfo_pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[sysinfo_pid]),
        false,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
    );
    system
        .process(sysinfo_pid)
        .and_then(|process| process.cwd())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A no-op `ChildKiller` standing in for a real PTY's -- these tests
    /// exercise `TerminalHost::install_if_vacant`'s first-wins *decision*
    /// (the seam this whole fix is about), not a real PTY spawn, so a real
    /// `portable_pty` child is unnecessary weight; `killed` just records
    /// whether `kill` was ever called.
    #[derive(Debug, Clone)]
    struct FakeKiller {
        killed: Arc<AtomicBool>,
    }

    impl ChildKiller for FakeKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    /// A `HostedTerminal` with a fake killer and its own command channel,
    /// plus handles to observe both -- everything `install_if_vacant`
    /// needs to decide with and, on the losing path, tear down.
    fn fake_session() -> (HostedTerminal, Receiver<TerminalCommand>, Arc<AtomicBool>) {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let killed = Arc::new(AtomicBool::new(false));
        let killer = FakeKiller {
            killed: killed.clone(),
        };
        let session = HostedTerminal {
            command_tx,
            latest_frame: Arc::new(Mutex::new(None)),
            pid: None,
            killer: Arc::new(Mutex::new(Box::new(killer))),
        };
        (session, command_rx, killed)
    }

    #[test]
    fn install_if_vacant_installs_the_first_session_for_a_fresh_id() {
        let host = TerminalHost::new();
        let session_id = Uuid::new_v4();
        let (session, _command_rx, killed) = fake_session();

        assert!(host.install_if_vacant(session_id, session));

        assert!(host.sessions.lock().unwrap().contains_key(&session_id));
        assert!(
            !killed.load(Ordering::SeqCst),
            "the only (winning) attempt must not be killed"
        );
    }

    /// The race the review comment on this fix called out: a slow-but-not-
    /// wedged attempt (`create`'s abandoned retry) can finish after a
    /// faster retry already installed a session for the same id. The late
    /// arrival must lose, not overwrite the live session or get its
    /// `forward_updates` loop started (which would interleave two shells'
    /// output into one pane).
    #[test]
    fn install_if_vacant_discards_and_kills_a_late_duplicate() {
        let host = TerminalHost::new();
        let session_id = Uuid::new_v4();
        let (winner, _winner_command_rx, winner_killed) = fake_session();
        let (loser, loser_command_rx, loser_killed) = fake_session();

        assert!(host.install_if_vacant(session_id, winner));
        assert!(!host.install_if_vacant(session_id, loser));

        assert!(
            !winner_killed.load(Ordering::SeqCst),
            "the winning session must survive"
        );
        assert!(
            loser_killed.load(Ordering::SeqCst),
            "the losing session's real process must be killed, not just dropped"
        );
        assert_eq!(
            loser_command_rx.try_recv(),
            Ok(TerminalCommand::Shutdown),
            "the losing session's background loop must be told to shut down"
        );
    }
}
