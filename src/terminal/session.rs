//! One terminal session: a terminal of its own while a view shows it, the
//! updates the daemon sends, and someone to wake when they come. A session
//! outlives a view of it.
//!
//! The daemon sends a session that a view shows the terminal's state and
//! then the bytes its own terminal took in (`docs/terminal-stream-design.md`);
//! the session takes them into its terminal, and what is drawn is read
//! from there. What a view does to the terminal (a key to encode, a
//! selection, a place in the history) is done to the session's terminal;
//! the daemon is sent only what is for the PTY.
//!
//! What a session says is for two readers. What a view acts on (a copy) is
//! the view's (`take_updates`). What is about the session itself (its
//! title, its directory, a notification, that its shell has gone) is for
//! whoever keeps the session, whether a view shows it or not
//! (`take_reports`).

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::{Receiver, Sender};
use horizon_shell_runtime::TerminalSessionHandle;
use horizon_shell_runtime::terminal_glyphs as drawn_glyphs;
use horizon_terminal_core::{
    ClipboardDestination, KeyStroke, TerminalCommand, TerminalCore, TerminalFrame,
    TerminalKeyInput, TerminalScrollWindow, TerminalSelectionKind, TerminalSelectionPoint,
    TerminalUpdate,
};

type Wake = Arc<Mutex<Option<Waker>>>;
/// What the platform said of a key: the physical key's name, and the
/// text the key makes.
type Platform = Option<(String, Option<String>)>;

pub struct Session {
    commands: Sender<TerminalCommand>,
    updates: Receiver<TerminalUpdate>,
    /// What has come and not been taken, for each reader.
    for_view: RefCell<Vec<TerminalUpdate>>,
    for_keeper: RefCell<Vec<TerminalUpdate>>,
    /// The session's own terminal: the daemon's state when it came, and
    /// the PTY's bytes since.
    mirror: RefCell<Option<TerminalCore>>,
    /// The state as far as it has come, and what of the daemon's stream
    /// has come and not been taken into the terminal: a view takes it in.
    state: RefCell<Vec<u8>>,
    stream: RefCell<Vec<TerminalUpdate>>,
    /// The frame read from the terminal, its number, and whether the
    /// terminal has changed since.
    read: RefCell<(u64, Option<TerminalFrame>)>,
    changed: Cell<bool>,
    /// What a view asked for the PTY before the terminal's state had come:
    /// asked of the session's terminal, in order, once it is there. (Sent
    /// to the daemon's instead, a key would go to the PTY by another way
    /// than the keys after it, and could arrive after them.)
    held: RefCell<Vec<(TerminalCommand, Platform)>>,
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
        let waker = Wake::default();
        // Whoever lays the page out is woken for what comes.
        let heard = Arc::clone(&waker);
        handle.hear_events(move || wake(&heard));
        Self::new(handle.sender(), handle.events(), waker, Some(handle))
    }

    /// A session behind plain channels: whatever runs the terminal core
    /// itself (a test's own PTY).
    pub fn from_channels(
        commands: Sender<TerminalCommand>,
        updates: Receiver<TerminalUpdate>,
    ) -> Self {
        let waker = Wake::default();
        let (passed, kept) = crossbeam_channel::unbounded();
        let woken = Arc::clone(&waker);
        std::thread::spawn(move || {
            while let Ok(update) = updates.recv() {
                if passed.send(update).is_err() {
                    return;
                }
                wake(&woken);
            }
        });
        Self::new(commands, kept, waker, None)
    }

    fn new(
        commands: Sender<TerminalCommand>,
        updates: Receiver<TerminalUpdate>,
        waker: Wake,
        attachment: Option<TerminalSessionHandle>,
    ) -> Self {
        Self {
            commands,
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
            _attachment: attachment,
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
        // (The frame's number goes on: a view shown again is to draw the
        // next one it is given.)
        self.read.borrow_mut().1 = None;
        // What was held for a terminal that never came is the daemon's.
        for (command, _) in std::mem::take(&mut *self.held.borrow_mut()) {
            let _ = self.commands.send(command);
        }
    }

    /// Whether the session has a terminal of its own now.
    pub fn has_a_terminal(&self) -> bool {
        self.mirror.borrow().is_some()
    }

    /// What a view asks for the PTY: the bytes for a key, a paste, a
    /// report to the program, are the session's terminal's to write, and
    /// the daemon is sent the bytes; a size and the colors a program is
    /// answered with are the daemon's.
    pub fn send(&self, command: TerminalCommand) {
        self.ask(command, None);
    }

    /// A key, with what the platform said of it beside what the wire's
    /// vocabulary carries: the physical key's name and the text the key
    /// makes. The session's terminal has the engine's own encoder write
    /// it from those.
    pub fn key(&self, input: TerminalKeyInput, code: String, made: Option<String>) {
        self.ask(TerminalCommand::KeyInput(input), Some((code, made)));
    }

    /// Starts a selection at a cell of the screen. The selection is the
    /// primary selection as it is made.
    pub fn select(&self, point: TerminalSelectionPoint, kind: TerminalSelectionKind) {
        self.on_the_terminal(|terminal| terminal.start_selection(point, kind));
        self.selected();
    }

    /// Moves the selection's other end.
    pub fn extend_selection(&self, point: TerminalSelectionPoint) {
        self.on_the_terminal(|terminal| terminal.update_selection(point));
        self.selected();
    }

    /// Copies the selection to the clipboard.
    pub fn copy_selection(&self) {
        let text = (self.mirror.borrow().as_ref()).and_then(TerminalCore::selected_text);
        if let Some(text) = text {
            self.for_view.borrow_mut().push(TerminalUpdate::Clipboard {
                text,
                destination: ClipboardDestination::Clipboard,
            });
            self.redraw();
        }
    }

    /// A window of the history `height` rows tall, `anchor` rows above the
    /// live bottom, read from the session's terminal.
    pub fn window(&self, anchor: usize, height: usize) -> Option<TerminalScrollWindow> {
        let mirror = self.mirror.borrow();
        Some(mirror.as_ref()?.snapshot_window(anchor, height))
    }

    fn on_the_terminal(&self, act: impl FnOnce(&mut TerminalCore)) {
        if let Some(terminal) = self.mirror.borrow_mut().as_mut() {
            act(terminal);
            self.changed.set(true);
        }
        // What was done here is drawn from here: nothing arrives for it.
        self.redraw();
    }

    fn selected(&self) {
        let text = (self.mirror.borrow().as_ref()).and_then(TerminalCore::selected_text);
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.for_view.borrow_mut().push(TerminalUpdate::Clipboard {
                text,
                destination: ClipboardDestination::Primary,
            });
        }
    }

    fn ask(&self, command: TerminalCommand, platform: Platform) {
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
                for_the_terminal => (self.held.borrow_mut()).push((for_the_terminal, platform)),
            }
            return;
        };
        let to_the_pty = |bytes: Vec<u8>| {
            if !bytes.is_empty() {
                let _ = self.commands.send(TerminalCommand::Input(bytes));
            }
        };
        match command {
            TerminalCommand::KeyInput(input) => {
                let (code, made) = platform.unwrap_or((String::new(), input.text.clone()));
                to_the_pty(terminal.key_stroke(&KeyStroke {
                    code: &code,
                    key: input.key,
                    modifiers: input.modifiers,
                    kind: input.kind,
                    made: made.as_deref(),
                }));
            }
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
                        *mirror = TerminalCore::from_state(&state).map(|mut terminal| {
                            // What the view draws from shapes is marked so
                            // in the rows it is given.
                            terminal.set_drawn_as_shapes(drawn_glyphs::is_geometric);
                            terminal
                        });
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
                TerminalUpdate::HoldReleased => {
                    if let Some(terminal) = mirror.as_mut() {
                        terminal.release_hold();
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
        for (command, platform) in held {
            self.ask(command, platform);
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
    /// from the session's own terminal; nothing until its state has come.
    pub fn frame_after(&self, seen: u64) -> Option<(u64, TerminalFrame)> {
        self.take_in();
        let mut read = self.read.borrow_mut();
        if self.changed.replace(false)
            && let Some(terminal) = self.mirror.borrow().as_ref()
        {
            *read = (read.0 + 1, Some(terminal.snapshot_frame()));
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
                    | TerminalUpdate::HoldReleased
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

    /// What a view acts on, that arrived since the last call.
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

    /// Who to wake when an update arrives.
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

fn wake(waker: &Mutex<Option<Waker>>) {
    if let Some(waker) = waker.lock().unwrap().as_ref() {
        waker.wake_by_ref();
    }
}
