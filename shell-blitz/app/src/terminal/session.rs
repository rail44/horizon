//! One terminal session as a view sees it: commands in, the newest frame
//! and the updates out, and someone to wake when a frame comes. A session
//! outlives a view of it: the frame is kept for the next view to draw.

use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::{Receiver, Sender};
use horizon_shell_runtime::TerminalSessionHandle;
use horizon_terminal_core::{TerminalCommand, TerminalFrame, TerminalUpdate};

/// How many frames have come and the newest of them, and who to wake
/// when one comes: shared with the thread that receives frames.
type Latest = Arc<Mutex<(u64, Option<TerminalFrame>)>>;
type Wake = Arc<Mutex<Option<Waker>>>;

pub struct Session {
    commands: Sender<TerminalCommand>,
    latest: Latest,
    updates: Receiver<TerminalUpdate>,
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
            waker,
            _attachment: None,
        }
    }

    pub fn send(&self, command: TerminalCommand) {
        let _ = self.commands.send(command);
    }

    /// The newest frame and its number, if it is not the one numbered
    /// `seen`: a view that has drawn nothing yet has seen frame 0.
    pub fn frame_after(&self, seen: u64) -> Option<(u64, TerminalFrame)> {
        let latest = self.latest.lock().unwrap();
        let frame = latest.1.as_ref().filter(|_| latest.0 != seen)?;
        Some((latest.0, frame.clone()))
    }

    /// The updates that arrived since the last call.
    pub fn take_updates(&self) -> Vec<TerminalUpdate> {
        self.updates.try_iter().collect()
    }

    /// Who to wake when a frame arrives.
    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
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
