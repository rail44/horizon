//! The terminal session's byte-driven loop: takes the PTY's bytes and the
//! commands from a client into one `TerminalCore`, sends the PTY what the
//! terminal answers, and sends a client that asks the terminal's state and
//! then the bytes the terminal took in (`docs/terminal-stream-design.md`).
//! The loop is driven purely by channels, with no `portable-pty`
//! dependency: the PTY reader/writer threads that feed and drain these
//! channels live in `horizon-terminald`
//! (`crates/horizon-terminald/src/terminal.rs`), since PTY ownership is a
//! host/process concern. It makes no frames: a view draws from a terminal
//! of its own.

use std::time::{Duration, Instant};

mod channels;

pub use channels::{CoreReceivers, CoreSenders, StreamCommand, core_channels};

use crossbeam_channel::{Receiver, Sender};

use crate::contract::{ClipboardDestination, TerminalUpdate};
use crate::core::{TerminalColorScheme, TerminalCore, TerminalEvents};
use crate::types::TerminalSize;

/// Construction-time options a real session feeds into `TerminalCore`,
/// mirroring host-config-derived values the crate itself has no way to read
/// (`docs/session-daemon-design.md` decision 9). A bare `TerminalCore::new`
/// (every test in this crate) uses its own built-in defaults instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalCoreOptions {
    pub scrollback_lines: usize,
    pub color_scheme: TerminalColorScheme,
}

impl Default for TerminalCoreOptions {
    fn default() -> Self {
        Self {
            scrollback_lines: crate::core::DEFAULT_SCROLLBACK_LINES,
            color_scheme: TerminalColorScheme::default(),
        }
    }
}

/// Re-arm (or disarm) the synchronized-update failsafe timer against the
/// core's current window state. Called after every core mutation that can
/// open, extend, or close a sync window — i.e. every `write_vt` and every
/// `flush_sync_update` — so `sync_flush_rx` always reflects live state:
/// `Some(deadline)` schedules a wakeup at that instant (mirroring
/// alacritty's own event loop, which polls with this same deadline as its
/// timeout — see `TerminalCore::sync_flush_deadline`'s doc comment); `None`
/// (no window open) parks it on a channel that never fires, so an idle
/// terminal causes no extra wakeups.
fn rearm_sync_flush(core: &TerminalCore, sync_flush_rx: &mut Receiver<Instant>) {
    *sync_flush_rx = match core.sync_flush_deadline() {
        Some(deadline) => crossbeam_channel::at(deadline),
        None => crossbeam_channel::never(),
    };
}

/// Applies the colors that have arrived and not been applied.
fn take_color_schemes(core: &mut TerminalCore, schemes: &Receiver<TerminalColorScheme>) {
    while let Ok(scheme) = schemes.try_recv() {
        core.set_color_scheme(scheme);
    }
}

/// What a view is sent of the terminal: its state when it asks,
/// and from there what the PTY writes, held against what the view says it
/// has taken in.
#[derive(Default)]
struct Stream {
    /// A view asked, and is being sent the bytes.
    wanted: bool,
    /// Bytes sent that the view has not said it took in.
    outstanding: u64,
    /// The view fell too far behind: nothing is sent until it has taken
    /// in what it was sent, and then the screen's part of the state.
    behind: bool,
    /// The view was sent the screen's part of the state and is owed the
    /// whole of it, history and all: when the PTY last wrote since.
    owed: Option<Instant>,
}

/// How many bytes a view may have been sent and not taken in.
const STREAM_AHEAD: u64 = 2 * 1024 * 1024;
/// The largest piece a state is sent in: under the events channel's cap
/// for an item.
const STATE_PIECE: usize = 1024 * 1024;
/// How long the PTY is to have written nothing before a view that was sent
/// the screen's part of the state is sent the whole of it.
const CALM: Duration = Duration::from_millis(300);

impl Stream {
    fn state(&mut self, state: Vec<u8>, update_tx: &Sender<TerminalUpdate>) {
        let pieces = state.chunks(STATE_PIECE).count().max(1);
        let mut sent = 0;
        for piece in state.chunks(STATE_PIECE) {
            sent += 1;
            let _ = update_tx.send(TerminalUpdate::State {
                bytes: piece.to_vec(),
                more: sent < pieces,
            });
        }
        (self.outstanding, self.behind) = (0, false);
    }

    fn said(
        &mut self,
        command: StreamCommand,
        core: &mut TerminalCore,
        update_tx: &Sender<TerminalUpdate>,
    ) {
        match command {
            StreamCommand::Want => {
                self.wanted = true;
                self.owed = None;
                self.state(core.state(), update_tx);
            }
            StreamCommand::Stop => *self = Self::default(),
            StreamCommand::Taken(bytes) => {
                self.outstanding = self.outstanding.saturating_sub(bytes);
                // The screen's part of the state: what the view was not
                // sent is in it, and it is read at once. (The whole state
                // here would be read slowly by a view that is behind
                // because it is slow, and it would fall behind again for
                // reading it.) The history comes once the output calms.
                if self.behind && self.outstanding == 0 {
                    self.state(core.state_of_the_screen(), update_tx);
                    self.owed = Some(Instant::now());
                }
            }
        }
    }

    fn wrote(&mut self, bytes: Vec<u8>, update_tx: &Sender<TerminalUpdate>) {
        if !self.wanted || bytes.is_empty() {
            return;
        }
        if let Some(wrote) = &mut self.owed {
            *wrote = Instant::now();
        }
        if self.behind {
            return;
        }
        if self.outstanding > STREAM_AHEAD {
            // What it has been sent stands; what it is not sent now it
            // gets as the state, once it has caught up.
            self.behind = true;
            return;
        }
        self.outstanding += bytes.len() as u64;
        let _ = update_tx.send(TerminalUpdate::Output(bytes));
    }

    /// When the whole state is due to a view owed it.
    fn owed_at(&self) -> Option<Instant> {
        (self.owed)
            .filter(|_| self.wanted)
            .map(|wrote| wrote + CALM)
    }

    /// The whole state, to a view owed it, if the output has calmed and
    /// the view has taken in what it was sent; otherwise later.
    fn pay(&mut self, core: &mut TerminalCore, update_tx: &Sender<TerminalUpdate>) {
        if !self.wanted || self.owed.is_none() {
            return;
        }
        if self.behind || self.outstanding > 0 {
            self.owed = Some(Instant::now());
            return;
        }
        self.owed = None;
        self.state(core.state(), update_tx);
    }

    fn released(&mut self, update_tx: &Sender<TerminalUpdate>) {
        if self.wanted && !self.behind {
            let _ = update_tx.send(TerminalUpdate::HoldReleased);
        }
    }

    fn resized(&mut self, size: TerminalSize, update_tx: &Sender<TerminalUpdate>) {
        if self.wanted && !self.behind {
            let _ = update_tx.send(TerminalUpdate::Resized(size));
        }
    }
}

/// Parse one PTY-output chunk and emit everything it produced: query/DA/DSR
/// replies back into the PTY input (`TerminalCommand::Input`), bell/title/
/// clipboard updates to the client, and the bytes the terminal took in to a
/// client that asked for them. Shared by [`drain_pty_output`]'s priority
/// drain and the `pty_rx` `select!` arm.
fn process_pty_chunk(
    core: &mut TerminalCore,
    bytes: &[u8],
    to_pty: &Sender<Vec<u8>>,
    update_tx: &Sender<TerminalUpdate>,
    sync_flush_rx: &mut Receiver<Instant>,
    stream: &mut Stream,
) {
    let mut events = core.write_vt(bytes);
    stream.wrote(std::mem::take(&mut events.output), update_tx);
    send_terminal_events(events, to_pty, update_tx);
    rearm_sync_flush(core, sync_flush_rx);
}

/// Publish parser side effects in the same order for PTY input and timed flushes.
fn send_terminal_events(
    events: TerminalEvents,
    to_pty: &Sender<Vec<u8>>,
    update_tx: &Sender<TerminalUpdate>,
) {
    for bytes in events.pty_writes {
        let _ = to_pty.send(bytes);
    }
    if events.bell_count > 0 {
        let _ = update_tx.send(TerminalUpdate::Bell);
    }
    if events.title.is_some() {
        let _ = update_tx.send(TerminalUpdate::Title(events.title));
    }
    for text in events.clipboard_writes {
        let _ = update_tx.send(TerminalUpdate::Clipboard {
            text,
            destination: ClipboardDestination::Clipboard,
        });
    }
    for notification in events.notifications {
        let _ = update_tx.send(TerminalUpdate::Notification(notification));
    }
    for status in events.program_status {
        let _ = update_tx.send(TerminalUpdate::ProgramStatus(status));
    }
}

/// Upper bound on PTY-output chunks processed by one [`drain_pty_output`]
/// call. Bounding keeps a sustained output flood from starving keystroke
/// forwarding entirely: once the budget is spent the caller falls back to
/// the randomized `select!`, which still services `pty_rx`.
const PTY_DRAIN_BUDGET: usize = 8;

/// Parse queued PTY-output chunks with priority over UI-originated input,
/// so each chunk's terminal-query replies (`TerminalCommand::Input`, e.g.
/// an OSC 11 background-color answer) are enqueued for the PTY writer
/// *before* whatever keystroke/text/paste prompted this drain is forwarded.
///
/// Why that ordering is load-bearing: `select!` picks randomly among ready
/// receivers, so a keystroke can otherwise be encoded and handed to the
/// shell before an earlier-arrived PTY chunk is even parsed. That chunk may
/// contain a terminal query -- fish 4.x probes the background color with
/// `OSC 11;?` around every prompt -- and the reply must reach the process
/// that asked while that process is still the one reading stdin. If the
/// Enter keystroke that execs the next program (`gh auth login`, a TUI,
/// ...) wins the race instead, the reply is written into the *new*
/// process's stdin, where survey-style prompt loops abort with
/// "unexpected escape sequence from terminal: ['\x1b' ']']". Native
/// terminals answer queries inline in their read handler; this drain
/// restores that ordering.
///
/// Returns `false` once `pty_rx` is disconnected (the session is tearing
/// down) so the caller can return; otherwise drains at most
/// [`PTY_DRAIN_BUDGET`] chunks and returns `true`.
fn drain_pty_output(
    core: &mut TerminalCore,
    pty_rx: &Receiver<Vec<u8>>,
    to_pty: &Sender<Vec<u8>>,
    update_tx: &Sender<TerminalUpdate>,
    sync_flush_rx: &mut Receiver<Instant>,
    stream: &mut Stream,
) -> bool {
    let mut budget = PTY_DRAIN_BUDGET;
    while budget > 0 {
        match pty_rx.try_recv() {
            Ok(bytes) => {
                process_pty_chunk(core, &bytes, to_pty, update_tx, sync_flush_rx, stream);
                budget -= 1;
            }
            Err(crossbeam_channel::TryRecvError::Empty) => return true,
            Err(crossbeam_channel::TryRecvError::Disconnected) => return false,
        }
    }
    true
}

pub fn run_terminal_core(
    size: TerminalSize,
    options: TerminalCoreOptions,
    pty_rx: Receiver<Vec<u8>>,
    receivers: CoreReceivers,
    to_pty: Sender<Vec<u8>>,
    update_tx: Sender<TerminalUpdate>,
) {
    let CoreReceivers {
        resize_rx,
        input_rx,
        color_scheme_rx,
        stream_rx,
    } = receivers;
    let mut core = TerminalCore::with_scrollback(size, options.scrollback_lines);
    core.set_color_scheme(options.color_scheme);
    // what a view that asked is sent.
    let mut stream = Stream::default();

    // Failsafe for a synchronized-update window (BSU/ESU, private mode 2026)
    // left open by a PTY chunk that never delivers its closing ESU — see
    // `TerminalCore::sync_flush_deadline`'s doc comment. Starts disarmed;
    // `rearm_sync_flush` only schedules a wakeup while a window is actually
    // open.
    let mut sync_flush_rx: Receiver<Instant> = crossbeam_channel::never();

    loop {
        // a view's keys go to the PTY without passing here, so
        // what they have a program ask can be read before colors that were
        // sent earlier are: the colors are taken first.
        take_color_schemes(&mut core, &color_scheme_rx);
        // PTY output drains with priority over UI-originated input -- see
        // `drain_pty_output` for why that reply ordering is load-bearing.
        if !drain_pty_output(
            &mut core,
            &pty_rx,
            &to_pty,
            &update_tx,
            &mut sync_flush_rx,
            &mut stream,
        ) {
            return;
        }

        let owed_rx = match stream.owed_at() {
            Some(at) => crossbeam_channel::at(at),
            None => crossbeam_channel::never(),
        };
        crossbeam_channel::select! {
            recv(owed_rx) -> _ => stream.pay(&mut core, &update_tx),
            recv(resize_rx) -> size => {
                let Ok(size) = size else {
                    return;
                };
                core.resize(size);
                stream.resized(size, &update_tx);
            }
            recv(stream_rx) -> command => {
                let Ok(command) = command else {
                    return;
                };
                stream.said(command, &mut core, &update_tx);
            }
            recv(input_rx) -> input => {
                let Ok(input) = input else { return; };
                // What the PTY has already said is answered before a
                // client's bytes are written: a reply to a query written
                // after the Enter that starts the next program would be
                // that program's input (`drain_pty_output`).
                take_color_schemes(&mut core, &color_scheme_rx);
                if !drain_pty_output(
                    &mut core, &pty_rx, &to_pty, &update_tx,
                    &mut sync_flush_rx, &mut stream,
                ) { return; }
                let _ = to_pty.send(input);
            }
            recv(color_scheme_rx) -> scheme => {
                let Ok(scheme) = scheme else {
                    return;
                };
                // What the program is answered with when it asks the
                // colors (OSC 4/10/11/12).
                core.set_color_scheme(scheme);
            }
            recv(pty_rx) -> bytes => {
                let Ok(bytes) = bytes else {
                    return;
                };
                take_color_schemes(&mut core, &color_scheme_rx);
                process_pty_chunk(
                    &mut core,
                    &bytes,
                    &to_pty,
                    &update_tx,
                    &mut sync_flush_rx,
                    &mut stream,
                );
            }
            recv(sync_flush_rx) -> _ => {
                let events = core.flush_sync_update();
                tracing::debug!(
                    target: "horizon_terminal_core::session_loop",
                    "sync_flush_fired"
                );
                send_terminal_events(events, &to_pty, &update_tx);
                stream.released(&update_tx);
                rearm_sync_flush(&core, &mut sync_flush_rx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Has the loop send what it takes in, and waits for the state that
    /// comes first: proof the loop is running and has done what was sent
    /// it before.
    fn watch(stream_tx: &Sender<StreamCommand>, updates: &Receiver<TerminalUpdate>) {
        stream_tx.send(StreamCommand::Want).unwrap();
        next(updates, "the state", |update| {
            matches!(update, TerminalUpdate::State { more: false, .. })
        });
    }

    /// The first update `met` holds for, within two seconds; the others
    /// are passed by.
    fn next(
        updates: &Receiver<TerminalUpdate>,
        what: &str,
        met: impl Fn(&TerminalUpdate) -> bool,
    ) -> TerminalUpdate {
        // (Only tells "never" apart: a loaded machine is slow, not stuck.)
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match updates.recv_timeout(left) {
                Ok(update) if met(&update) => return update,
                Ok(_) => {}
                Err(_) => panic!("no {what}"),
            }
        }
    }

    /// The end-to-end failsafe for a synchronized update left open: a PTY
    /// chunk that opens one and never closes it is let go by the loop's
    /// own clock, with no further PTY data at all, and a client that is
    /// sent the stream is told where (`TerminalUpdate::HoldReleased`). The
    /// parser's side effects of what was held are forwarded as they came.
    #[test]
    fn sync_update_failsafe_lets_a_stuck_hold_go_and_says_where() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (senders, receivers) = core_channels();
        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(40, 40),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });
        watch(&senders.stream_tx, &update_rx);

        let held = b"\x1b[?2026h\x1b[H\x1b[K\x1b]2;flushed\x07\x07\x1b]52;c;aGVsbG8=\x07\x1b[6n";
        // (Timed from before the send: a stall of this thread only makes
        // it longer.)
        let started = Instant::now();
        pty_tx.send(held.to_vec()).unwrap();
        let mut seen = Vec::new();
        loop {
            match update_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(TerminalUpdate::HoldReleased) => break,
                Ok(TerminalUpdate::Output(bytes)) => assert_eq!(bytes, held),
                Ok(update) => seen.push(update),
                Err(_) => panic!("the hold was never let go"),
            }
        }
        // Not before the failsafe's time.
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert_eq!(
            seen,
            vec![
                TerminalUpdate::Bell,
                TerminalUpdate::Title(Some("flushed".into())),
                TerminalUpdate::Clipboard {
                    text: "hello".into(),
                    destination: ClipboardDestination::Clipboard,
                },
            ]
        );
        assert_eq!(command_rx.try_recv().unwrap(), b"\x1b[1;1R".to_vec());
        assert!(command_rx.try_recv().is_err());
    }

    /// A client too far behind is sent nothing more until it has taken in
    /// what it was sent, and then the screen's part of the state; once the
    /// output has calmed, the whole of it.
    #[test]
    fn a_client_too_far_behind_is_sent_the_state_again_once_it_has_caught_up() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, _command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (senders, receivers) = core_channels();
        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(80, 24),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });
        watch(&senders.stream_tx, &update_rx);

        let chunk = "flooding\r\n".repeat(10_000).into_bytes();
        let rounds = (STREAM_AHEAD as usize / chunk.len()) + 4;
        for _ in 0..rounds {
            pty_tx.send(chunk.clone()).unwrap();
        }
        pty_tx.send(b"after".to_vec()).unwrap();
        // What is sent goes past the bound, with nothing taken in...
        let mut sent = 0u64;
        while sent <= STREAM_AHEAD {
            match next(&update_rx, "output past the bound", |_| true) {
                TerminalUpdate::Output(bytes) => sent += bytes.len() as u64,
                other => panic!("{other:?} while behind"),
            }
        }
        // ...and stops there. (A late item would slip past this wait; one
        // in it is caught.)
        if let Ok(update) = update_rx.recv_timeout(Duration::from_millis(300)) {
            panic!("{update:?} sent past the bound, after {sent}");
        }
        assert!(sent <= STREAM_AHEAD + chunk.len() as u64, "{sent}");
        // Taken in: the screen's part of the state, which has what was not
        // sent.
        let asked = Instant::now();
        senders.stream_tx.send(StreamCommand::Taken(sent)).unwrap();
        let state = |what: &str| {
            let mut state = Vec::new();
            loop {
                match next(&update_rx, what, |_| true) {
                    TerminalUpdate::State { bytes, more } => {
                        state.extend_from_slice(&bytes);
                        if !more {
                            return state;
                        }
                    }
                    other => panic!("{other:?} before {what}"),
                }
            }
        };
        let screen = state("the screen's part");
        let view = TerminalCore::from_state(&screen).expect("a terminal from the screen's part");
        assert!(
            view.snapshot_text().trim_end().ends_with("after"),
            "{}",
            view.snapshot_text()
        );
        // The output has stopped: the whole of it, history and all, once
        // the PTY has been calm. (Timed from the asking, before the screen's
        // part: a stall of this thread only makes it longer.)
        let whole = state("the whole state");
        assert!(asked.elapsed() >= CALM / 2);
        assert!(
            whole.len() > screen.len() * 4,
            "{} after {}",
            whole.len(),
            screen.len()
        );
        let view = TerminalCore::from_state(&whole).expect("a terminal from the whole state");
        assert!(
            view.snapshot_text().trim_end().ends_with("after"),
            "{}",
            view.snapshot_text()
        );
    }

    #[test]
    fn disconnecting_pty_or_command_inputs_stops_the_core_with_the_other_input_alive() {
        for close_commands in [false, true] {
            let (senders, receivers) = core_channels();
            let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
            let (command_tx, _command_rx) = crossbeam_channel::unbounded();
            let (update_tx, update_rx) = crossbeam_channel::unbounded();
            let mut senders = Some(senders);
            let mut pty_tx = Some(pty_tx);
            let worker = std::thread::spawn(move || {
                run_terminal_core(
                    TerminalSize::new(20, 5),
                    TerminalCoreOptions::default(),
                    pty_rx,
                    receivers,
                    command_tx,
                    update_tx,
                );
            });
            watch(&senders.as_ref().unwrap().stream_tx, &update_rx);
            if close_commands {
                drop(senders.take());
            } else {
                drop(pty_tx.take());
            }
            assert_eq!(
                update_rx.recv_timeout(Duration::from_secs(2)),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected),
                "the core must exit without waiting for the other input owner"
            );
            worker.join().unwrap();
        }
    }

    /// End-to-end regression coverage for OSC 52 clipboard-write plumbing
    /// (`docs/tasks/backlog.md` item 4): a PTY chunk carrying an OSC 52
    /// write sequence must come out the other side of `run_terminal_core`
    /// as a `TerminalUpdate::Clipboard`, exercised through the real
    /// `pty_rx` -> `TerminalCore::write_vt` -> `update_tx` path rather than
    /// calling `write_vt` directly (see `crate::tests` for the core-level
    /// event-firing/cap tests this complements). Deliberately stops at the
    /// channel boundary: writing to the real system clipboard only happens
    /// in `app::runtime::terminal`, outside this crate entirely.
    #[test]
    fn run_terminal_core_forwards_osc52_clipboard_writes_as_updates() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, _command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (_senders, receivers) = core_channels();

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(40, 40),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        // base64("hello") == "aGVsbG8="
        pty_tx.send(b"\x1b]52;c;aGVsbG8=\x07".to_vec()).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut clipboard_text = None;
        while Instant::now() < deadline {
            if let Ok(TerminalUpdate::Clipboard { text, destination }) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                assert_eq!(destination, ClipboardDestination::Clipboard);
                clipboard_text = Some(text);
                break;
            }
        }

        assert_eq!(clipboard_text.as_deref(), Some("hello"));
    }

    #[test]
    fn run_terminal_core_forwards_osc_notifications_as_updates() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, _command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (_senders, receivers) = core_channels();

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(20, 5),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        // One OSC 9 (BEL-terminated), plain output between, and one OSC 777
        // `notify` (ST-terminated) — the full daemon-side pipeline: PTY
        // bytes in, `TerminalUpdate::Notification`s out on the events mpsc.
        pty_tx
            .send(b"\x1b]9;build finished\x07plain text\x1b]777;notify;Deploy;ok\x1b\\".to_vec())
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut saw_body = false;
        let mut saw_titled = false;
        while Instant::now() < deadline && !(saw_body && saw_titled) {
            match update_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(TerminalUpdate::Notification(notification)) => {
                    if notification.title.is_none() && notification.body == "build finished" {
                        saw_body = true;
                    }
                    if notification.title.as_deref() == Some("Deploy") && notification.body == "ok"
                    {
                        saw_titled = true;
                    }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(
            saw_body,
            "the OSC 9 body should arrive as a Notification update"
        );
        assert!(
            saw_titled,
            "the OSC 777 notify should arrive as a Notification update"
        );
    }

    /// End-to-end regression coverage for the live theme-apply re-push
    /// (see `TerminalCommand::SetColorScheme`'s doc comment): a scheme sent
    /// on `color_scheme_tx` demuxes onto `color_scheme_rx` and reaches
    /// `TerminalCore::set_color_scheme` on an already-running session, so a
    /// subsequent OSC 11 query replies with the newly pushed background
    /// instead of the spawn-time default. Also covers the precedent this
    /// re-push must preserve: an app-set OSC 11 override (`Term::colors()`)
    /// still wins over a *second* re-push, exactly as it already wins over
    /// the very first (spawn-time) scheme.
    ///
    /// There is no update the push itself produces to synchronize on (a
    /// color-scheme swap alone never touches the visible grid -- see the
    /// `color_scheme_rx` arm's doc comment), so each assertion below polls
    /// with a fresh query rather than assuming the push has already landed
    /// by the time the very first query is sent.
    #[test]
    fn run_terminal_core_repushes_the_color_scheme_to_a_running_session() {
        use crate::core::Rgb;

        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, _update_rx) = crossbeam_channel::unbounded();
        let (senders, receivers) = core_channels();
        let color_scheme_tx = senders.color_scheme_tx.clone();

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(20, 10),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        // Push a scheme distinct from `TerminalColorScheme::default()` and
        // poll an OSC 11 query until the reply reflects it.
        let repushed = Rgb {
            r: 10,
            g: 20,
            b: 30,
        };
        color_scheme_tx
            .send(TerminalColorScheme {
                background: repushed,
                ..TerminalColorScheme::default()
            })
            .unwrap();
        assert!(
            poll_query_reply(
                &pty_tx,
                &command_rx,
                b"\x1b]11;?\x07",
                &osc_reply(11, repushed)
            ),
            "OSC 11 query should reply with the re-pushed background"
        );

        // The attached app now sets its own OSC 11 override.
        pty_tx.send(b"\x1b]11;#010203\x07".to_vec()).unwrap();

        // A second re-push, changing the *foreground* this time (not the
        // background the override above targets). `color_scheme_tx` and
        // `pty_tx` are different channels with no ordering guarantee
        // between them, so the override-set above and this push could be
        // applied in either order -- polling OSC 10 (below) until it
        // reflects this push's foreground is a positive, deterministic
        // confirmation that the push has actually landed, rather than
        // just assuming the send order above is also the processing
        // order. Without that confirmation, an implementation that
        // clobbered live overrides on every push could still pass this
        // test purely by luck of which channel the select loop happened
        // to service first.
        let new_foreground = Rgb {
            r: 200,
            g: 201,
            b: 202,
        };
        color_scheme_tx
            .send(TerminalColorScheme {
                foreground: new_foreground,
                background: Rgb {
                    r: 90,
                    g: 90,
                    b: 90,
                },
                ..TerminalColorScheme::default()
            })
            .unwrap();
        assert!(
            poll_query_reply(
                &pty_tx,
                &command_rx,
                b"\x1b]10;?\x07",
                &osc_reply(10, new_foreground)
            ),
            "OSC 10 query should reply with the second re-push's foreground"
        );

        // The push is now confirmed applied (and `poll_query_reply` left
        // no straggler reply behind it), so this query -- sent only after
        // that confirmation -- is guaranteed to be answered against the
        // already-landed push, not racing it: the override must still
        // win.
        pty_tx.send(b"\x1b]11;?\x07".to_vec()).unwrap();
        let reply = command_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("OSC 11 query reply after the confirmed re-push");
        assert_eq!(
            reply, b"\x1b]11;rgb:0101/0202/0303\x07",
            "an app-set OSC 11 override must keep winning over a re-pushed scheme"
        );
    }

    /// Formats the OSC `osc` (10/11/12) query-reply escape sequence
    /// `rgb` should produce -- shared by the color-scheme re-push test's
    /// assertions against both OSC 10 (foreground) and OSC 11
    /// (background).
    fn osc_reply(osc: u8, rgb: crate::core::Rgb) -> Vec<u8> {
        format!(
            "\x1b]{osc};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07",
            osc = osc,
            r = rgb.r,
            g = rgb.g,
            b = rgb.b
        )
        .into_bytes()
    }

    /// Sends a fresh `query` and retries (bounded by a 2s deadline) until a
    /// reply matching `expected` arrives -- the color-scheme re-push
    /// test's synchronization primitive, since a scheme push alone
    /// produces no `TerminalUpdate` to wait on instead (see that test's
    /// doc comment). On success, also drains any straggler reply a
    /// slow-to-arrive earlier iteration's duplicate query left queued
    /// behind the matching one, so a caller that treats this return as
    /// "state confirmed as of now" can rely on the channel holding nothing
    /// older.
    fn poll_query_reply(
        pty_tx: &Sender<Vec<u8>>,
        command_rx: &Receiver<Vec<u8>>,
        query: &[u8],
        expected: &[u8],
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let _ = pty_tx.send(query.to_vec());
            if let Ok(bytes) = command_rx.recv_timeout(Duration::from_millis(50))
                && bytes == expected
            {
                while command_rx.try_recv().is_ok() {}
                return true;
            }
        }
        false
    }

    /// Regression test for query-reply ordering: a PTY chunk containing a
    /// terminal query (fish 4.x probes the background color with
    /// `OSC 11;?` around every prompt) that reaches the loop before a
    /// client's keystroke must be answered before that keystroke is written
    /// to the shell. Otherwise the Enter that execs the next program could
    /// win the race, and the reply was then written into the *new*
    /// process's stdin -- gh's survey prompt loop aborts on it with
    /// "unexpected escape sequence from terminal: ['\x1b' ']']". A view's
    /// terminal writes the keys; the loop writes them after its priority
    /// drain of the PTY.
    #[test]
    fn pending_query_is_answered_before_a_queued_keystroke_is_forwarded() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (to_pty, written) = crossbeam_channel::unbounded();
        let (update_tx, _update_rx) = crossbeam_channel::unbounded();
        let (senders, receivers) = core_channels();
        let input_tx = senders.input_tx.clone();

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(20, 10),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                to_pty,
                update_tx,
            );
        });

        // Queue the query first, then the keystrokes it must not lose to.
        pty_tx.send(b"\x1b]11;?\x07".to_vec()).unwrap();
        input_tx.send(b"\r".to_vec()).unwrap();
        input_tx.send(b"a".to_vec()).unwrap();

        let first = written
            .recv_timeout(Duration::from_millis(500))
            .expect("the query must be answered");
        assert!(
            first.starts_with(b"\x1b]11;rgb:"),
            "the OSC 11 reply must precede every queued keystroke, got {first:?}"
        );
        let second = written
            .recv_timeout(Duration::from_millis(500))
            .expect("the Enter keystroke must still be written");
        assert_eq!(second, b"\r", "the Enter keystroke right after the reply");
        let third = written
            .recv_timeout(Duration::from_millis(500))
            .expect("the 'a' keystroke must still be written");
        assert_eq!(third, b"a", "the 'a' keystroke keeps its queue position");
        assert!(
            written.recv_timeout(Duration::from_millis(100)).is_err(),
            "the query must be answered exactly once"
        );
    }

    #[test]
    fn a_client_s_bytes_keep_their_order_before_the_core_starts() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (to_pty, written) = crossbeam_channel::unbounded();
        let (update_tx, _update_rx) = crossbeam_channel::unbounded();
        let (senders, receivers) = core_channels();
        let mut expected = Vec::new();
        // Queued before the consumer starts, to cover a busy core without a
        // timing race between the test's setup and the loop.
        for index in 0..32 {
            for bytes in [
                format!("input-{index}").into_bytes(),
                "日本語".as_bytes().to_vec(),
                b"\r".to_vec(),
            ] {
                senders.input_tx.send(bytes.clone()).unwrap();
                expected.push(bytes);
            }
        }
        let worker = std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(20, 10),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                to_pty,
                update_tx,
            )
        });
        let actual: Vec<_> = (0..expected.len())
            .map(|_| {
                written
                    .recv_timeout(Duration::from_secs(2))
                    .expect("the bytes written")
            })
            .collect();
        drop(senders);
        drop(pty_tx);
        worker.join().unwrap();
        assert_eq!(
            actual, expected,
            "Enter must not overtake text or pasted input"
        );
    }

    #[test]
    fn run_terminal_core_answers_a_status_query_and_forwards_the_reports() {
        let (pty_tx, pty_rx) = crossbeam_channel::unbounded();
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (update_tx, update_rx) = crossbeam_channel::unbounded();
        let (_senders, receivers) = core_channels();

        std::thread::spawn(move || {
            run_terminal_core(
                TerminalSize::new(20, 5),
                TerminalCoreOptions::default(),
                pty_rx,
                receivers,
                command_tx,
                update_tx,
            );
        });

        // The question, a report, a background command's own record (which
        // is not the program's), and the report taken back.
        pty_tx
            .send(
                b"\x1b]7501;?\x1b\\\x1b]7501;state=blocked:app=claude-code:kind=question\x1b\\\
                  \x1b]7501;state=working:id=bt7g05417\x1b\\\x1b]7501;state=clear\x1b\\"
                    .to_vec(),
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut statuses = Vec::new();
        while Instant::now() < deadline && statuses.len() < 2 {
            if let Ok(TerminalUpdate::ProgramStatus(status)) =
                update_rx.recv_timeout(Duration::from_millis(50))
            {
                statuses.push(status);
            }
        }
        let [Some(status), None] = &statuses[..] else {
            panic!("forwarded {statuses:?}");
        };
        assert_eq!(status.state, crate::ProgramState::Blocked);
        assert_eq!(status.blocked_on, Some(crate::ProgramBlockedOn::Question));
        assert_eq!(status.app.as_deref(), Some("claude-code"));

        // The answer goes to the program as any other reply does.
        let mut answered = false;
        while let Ok(bytes) = command_rx.recv_timeout(Duration::from_millis(200)) {
            answered |= bytes.starts_with(b"\x1b]7501;?:states=");
        }
        assert!(answered, "the status query is answered on the PTY");
    }
}
