//! One terminal session: commands in, the newest frame and the updates
//! out, and someone to wake when either comes. A session outlives a view
//! of it: the frame is kept for the next view to draw.
//!
//! What a session says is for two readers. What is drawn and what a view
//! acts on (a copy, a window of the scrollback) is the view's
//! (`take_updates`). What is about the session itself (its title, its
//! directory, a notification, that its shell has gone) is for whoever
//! keeps the session, whether a view shows it or not (`take_reports`).

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::{Receiver, Sender};
use horizon_shell_runtime::TerminalSessionHandle;
use horizon_terminal_core::{
    ClipboardDestination, TerminalCommand, TerminalCore, TerminalFrame, TerminalUpdate,
};

/// How many frames have come and the newest of them, and who to wake
/// when one comes: shared with the thread that receives frames.
type Latest = Arc<Mutex<(u64, Option<TerminalFrame>)>>;
type Wake = Arc<Mutex<Option<Waker>>>;

pub struct Session {
    commands: Sender<TerminalCommand>,
    latest: Latest,
    updates: Receiver<TerminalUpdate>,
    /// What has come and not been taken, for each reader.
    for_view: RefCell<Vec<TerminalUpdate>>,
    for_keeper: RefCell<Vec<TerminalUpdate>>,
    /// SPIKE Y. The session's own terminal: the daemon's state when it
    /// came, and the PTY's bytes since. What is drawn is read from it, and
    /// what a view does to the terminal (a selection, a key to encode, a
    /// place in the history) is done to it.
    mirror: RefCell<Option<TerminalCore>>,
    /// The state as far as it has come, and what of the daemon's stream
    /// has come and not been taken into the terminal: a view takes it in.
    state: RefCell<Vec<u8>>,
    stream: RefCell<Vec<TerminalUpdate>>,
    /// The frame read from the terminal, its number, and whether the
    /// terminal has changed since.
    read: RefCell<(u64, Option<TerminalFrame>)>,
    changed: Cell<bool>,
    /// What a view asked of the terminal before its state had come: asked
    /// of the session's terminal, in order, once it is there. (Sent to the
    /// daemon's instead, a key would go to the PTY by another way than the
    /// keys after it, and could arrive after them.)
    held: RefCell<Vec<TerminalCommand>>,
    /// How many views show the session.
    views: Cell<usize>,
    /// How many times a state has come whole, and how many bytes of output
    /// have been taken in: for a measurement.
    states: Cell<u64>,
    bytes: Cell<u64>,
    waker: Wake,
    /// The attachment to the terminal runtime, kept for as long as the
    /// view is: letting go of it detaches the session, which goes on
    /// running.
    _attachment: Option<TerminalSessionHandle>,
}

impl Session {
    /// A session of `horizon-terminald`'s, through the runtime client.
    pub fn attached(handle: TerminalSessionHandle) -> Self {
        let (latest, waker) = (Latest::default(), Wake::default());
        let (kept, woken) = (Arc::clone(&latest), Arc::clone(&waker));
        let mut frames = handle.frames();
        // The frames are a watch: its newest value is what a view draws,
        // and for a session attached to again it is there already.
        keep(&kept, frames.borrow_and_update().clone());
        // An update is drawn too (a window of the scrollback) or acted on
        // (a copy): whoever lays the page out is woken for it as for a
        // frame.
        let heard = Arc::clone(&waker);
        handle.hear_events(move || wake(&heard));
        std::thread::spawn(move || {
            futures::executor::block_on(async {
                while frames.changed().await.is_ok() {
                    keep(&kept, frames.borrow_and_update().clone());
                    wake(&woken);
                }
            });
        });
        Self {
            commands: handle.sender(),
            latest,
            updates: handle.events(),
            for_view: RefCell::default(),
            for_keeper: RefCell::default(),
            mirror: RefCell::default(),
            state: RefCell::default(),
            stream: RefCell::default(),
            read: RefCell::default(),
            changed: Cell::new(false),
            held: RefCell::default(),
            views: Cell::new(0),
            states: Cell::new(0),
            bytes: Cell::new(0),
            waker,
            _attachment: Some(handle),
        }
    }

    /// A session behind plain channels: whatever runs the terminal core
    /// itself (a test's own PTY).
    pub fn from_channels(
        commands: Sender<TerminalCommand>,
        frames: Receiver<TerminalFrame>,
        updates: Receiver<TerminalUpdate>,
    ) -> Self {
        let (latest, waker) = (Latest::default(), Wake::default());
        let (kept, woken) = (Arc::clone(&latest), Arc::clone(&waker));
        std::thread::spawn(move || {
            while let Ok(frame) = frames.recv() {
                keep(&kept, frame);
                wake(&woken);
            }
        });
        Self {
            commands,
            latest,
            updates,
            for_view: RefCell::default(),
            for_keeper: RefCell::default(),
            mirror: RefCell::default(),
            state: RefCell::default(),
            stream: RefCell::default(),
            read: RefCell::default(),
            changed: Cell::new(false),
            held: RefCell::default(),
            views: Cell::new(0),
            states: Cell::new(0),
            bytes: Cell::new(0),
            waker,
            _attachment: None,
        }
    }

    /// A view shows the session: the daemon is asked for the terminal's
    /// state and what follows it, and the session has a terminal of its
    /// own from when the state comes until [`Self::hide`].
    pub fn show(&self) {
        self.views.set(self.views.get() + 1);
        if self.views.get() == 1 {
            let _ = self.commands.send(TerminalCommand::WantState);
        }
    }

    /// A view that showed the session is gone. With the last of them the
    /// session's terminal is let go of, and the daemon sends no more: a
    /// session nothing shows costs the daemon's terminal alone.
    pub fn hide(&self) {
        self.views.set(self.views.get().saturating_sub(1));
        if self.views.get() > 0 {
            return;
        }
        let _ = self.commands.send(TerminalCommand::StopState);
        *self.mirror.borrow_mut() = None;
        self.state.borrow_mut().clear();
        self.stream.borrow_mut().clear();
        *self.read.borrow_mut() = (0, None);
        // What was held for a terminal that never came is the daemon's.
        for command in std::mem::take(&mut *self.held.borrow_mut()) {
            let _ = self.commands.send(command);
        }
    }

    /// Whether the session has a terminal of its own now.
    pub fn has_a_terminal(&self) -> bool {
        self.mirror.borrow().is_some()
    }

    /// What a view asks of the terminal. What is the terminal's own to
    /// answer is asked of the session's (a selection, the bytes for a key,
    /// a window of the history), and the daemon is sent only what is for
    /// the PTY: bytes, a size, the colors it answers a program with.
    pub fn send(&self, command: TerminalCommand) {
        let mut mirror = self.mirror.borrow_mut();
        let Some(terminal) = mirror.as_mut() else {
            if self.views.get() == 0 {
                // Nothing shows the session: the daemon's terminal is
                // asked, the only one there is.
                let _ = self.commands.send(command);
                return;
            }
            match command {
                TerminalCommand::Resize(_)
                | TerminalCommand::SetColorScheme(_)
                | TerminalCommand::Input(_)
                | TerminalCommand::Shutdown => {
                    let _ = self.commands.send(command);
                }
                for_the_terminal => self.held.borrow_mut().push(for_the_terminal),
            }
            return;
        };
        let to_the_pty = |bytes: Vec<u8>| {
            if !bytes.is_empty() {
                let _ = self.commands.send(TerminalCommand::Input(bytes));
            }
        };
        // The selection is the primary selection as it is made.
        let selected = |terminal: &TerminalCore| {
            if let Some(text) = terminal.selected_text().filter(|text| !text.is_empty()) {
                self.for_view.borrow_mut().push(TerminalUpdate::Clipboard {
                    text,
                    destination: ClipboardDestination::Primary,
                });
            }
        };
        match command {
            TerminalCommand::KeyInput(input) => to_the_pty(terminal.key_input(
                input.key,
                input.modifiers,
                input.kind,
                input.text.as_deref(),
            )),
            TerminalCommand::TextInput(text) => to_the_pty(terminal.text_input(&text)),
            TerminalCommand::Paste(text) => to_the_pty(terminal.paste_input(&text)),
            TerminalCommand::Mouse(report) => {
                to_the_pty(terminal.handle_mouse_report(report).unwrap_or_default());
            }
            TerminalCommand::Focus(focused) => {
                to_the_pty(terminal.focus_input(focused).unwrap_or_default());
            }
            TerminalCommand::Scroll(scroll) => match terminal.handle_scroll(scroll) {
                Some(bytes) => to_the_pty(bytes),
                None => self.changed.set(true),
            },
            TerminalCommand::SelectionStart { point, kind } => {
                terminal.start_selection(point, kind);
                selected(terminal);
                self.changed.set(true);
            }
            TerminalCommand::SelectionUpdate(point) => {
                terminal.update_selection(point);
                selected(terminal);
                self.changed.set(true);
            }
            TerminalCommand::CopySelection => {
                if let Some(text) = terminal.selected_text() {
                    self.for_view.borrow_mut().push(TerminalUpdate::Clipboard {
                        text,
                        destination: ClipboardDestination::Clipboard,
                    });
                }
            }
            TerminalCommand::RequestScrollWindow { anchor, height } => {
                let window = terminal.snapshot_window(anchor, height);
                (self.for_view.borrow_mut()).push(TerminalUpdate::ScrollWindow(window));
            }
            for_the_daemon => {
                let _ = self.commands.send(for_the_daemon);
                return;
            }
        }
        // What was done here is drawn from here: nothing arrives for it.
        drop(mirror);
        self.redraw();
    }

    /// Takes what has come of the daemon's stream into the session's
    /// terminal, and tells the daemon how much.
    fn take_in(&self) {
        self.sort();
        let stream = std::mem::take(&mut *self.stream.borrow_mut());
        if stream.is_empty() {
            return;
        }
        let mut mirror = self.mirror.borrow_mut();
        let mut taken = 0u64;
        for update in stream {
            match update {
                TerminalUpdate::State { bytes, more } => {
                    let mut state = self.state.borrow_mut();
                    state.extend_from_slice(&bytes);
                    if !more {
                        *mirror = TerminalCore::from_state(&state);
                        state.clear();
                        self.states.set(self.states.get() + 1);
                    }
                }
                TerminalUpdate::Output(bytes) => {
                    taken += bytes.len() as u64;
                    if let Some(terminal) = mirror.as_mut() {
                        terminal.take_in(&bytes);
                    }
                }
                TerminalUpdate::Resized(size) => {
                    if let Some(terminal) = mirror.as_mut() {
                        terminal.resize(size);
                    }
                }
                _ => {}
            }
        }
        let held = match mirror.is_some() {
            true => std::mem::take(&mut *self.held.borrow_mut()),
            false => Vec::new(),
        };
        drop(mirror);
        for command in held {
            self.send(command);
        }
        if taken > 0 {
            self.bytes.set(self.bytes.get() + taken);
            let _ = self.commands.send(TerminalCommand::OutputTaken(taken));
        }
        self.changed.set(true);
    }

    /// How many times the daemon's state has come, and how many bytes of
    /// its output have been taken in.
    pub fn states_taken(&self) -> u64 {
        self.states.get()
    }

    pub fn bytes_taken(&self) -> u64 {
        self.bytes.get()
    }

    /// The newest frame and its number, if it is not the one numbered
    /// `seen`: a view that has drawn nothing yet has seen frame 0. Read
    /// from the session's own terminal.
    pub fn frame_after(&self, seen: u64) -> Option<(u64, TerminalFrame)> {
        self.take_in();
        if self.mirror.borrow().is_none() {
            // No state has come yet: the daemon's own frame.
            let latest = self.latest.lock().unwrap();
            let frame = latest.1.as_ref().filter(|_| latest.0 != seen)?;
            return Some((latest.0, frame.clone()));
        }
        let mut read = self.read.borrow_mut();
        if self.changed.replace(false)
            && let Some(terminal) = self.mirror.borrow().as_ref()
        {
            // (Numbered apart from the daemon's frames, which a view may
            // have seen before the state came.)
            *read = (read.0.max(1 << 32) + 1, Some(terminal.snapshot_frame()));
        }
        let frame = read.1.as_ref().filter(|_| read.0 != seen)?;
        Some((read.0, frame.clone()))
    }

    /// Puts what has come in front of the reader it is for.
    fn sort(&self) {
        for update in self.updates.try_iter() {
            let about_the_session = matches!(
                update,
                TerminalUpdate::Title(_)
                    | TerminalUpdate::WorkingDirectory(_)
                    | TerminalUpdate::Notification(_)
                    | TerminalUpdate::ProgramStatus(_)
                    | TerminalUpdate::Exited
            );
            let of_the_stream = matches!(
                update,
                TerminalUpdate::State { .. }
                    | TerminalUpdate::Output(_)
                    | TerminalUpdate::Resized(_)
            );
            match (about_the_session, of_the_stream) {
                (true, _) => self.for_keeper.borrow_mut().push(update),
                // (Sent before the daemon heard that no view is here.)
                (_, true) if self.views.get() == 0 => {}
                (_, true) => self.stream.borrow_mut().push(update),
                _ => self.for_view.borrow_mut().push(update),
            }
        }
    }

    /// What a view draws or acts on, that arrived since the last call.
    pub fn take_updates(&self) -> Vec<TerminalUpdate> {
        self.take_in();
        std::mem::take(&mut *self.for_view.borrow_mut())
    }

    /// What is about the session itself, that arrived since the last
    /// call: its title, its directory, a notification, its end.
    pub fn take_reports(&self) -> Vec<TerminalUpdate> {
        self.sort();
        std::mem::take(&mut *self.for_keeper.borrow_mut())
    }

    /// Who to wake when a frame or an update arrives.
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }

    /// Has the page laid out and drawn again: for a view whose own state
    /// changed with nothing arriving and nothing of the document written
    /// (its place in the scrollback, what the input method composes).
    pub fn redraw(&self) {
        wake(&self.waker);
    }
}

fn keep(latest: &Latest, frame: TerminalFrame) {
    let mut latest = latest.lock().unwrap();
    *latest = (latest.0 + 1, Some(frame));
}

fn wake(waker: &Mutex<Option<Waker>>) {
    if let Some(waker) = waker.lock().unwrap().as_ref() {
        waker.wake_by_ref();
    }
}
