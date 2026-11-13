//! The board: the task list and one task's thread, as two views over
//! `horizon-board`'s store. Neither holds the other: a pane split puts
//! them side by side.
//!
//! What the views are built on is `horizon-board-view`'s: the tree order,
//! folding, the selection, where a dragged row lands, the key maps and
//! the fixed labels. A view reads and writes the store off the UI thread
//! and takes the answers in at the page's next poll, and reads again when
//! the store's log says something was written.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use horizon_board::{Item, Store, StoreError};
use horizon_board_view::activity::{BoardSessionActivity, task_session_state};
use horizon_board_view::chrome;
use horizon_ui::reactive_graph::signal::{RwSignal, WriteSignal};
use horizon_ui::reactive_graph::traits::With;
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Component, El};
use horizon_workspace::SessionId;

use crate::background::Background;
use crate::parts::{Chip, Tone};

pub mod list;
#[cfg(not(target_family = "wasm"))]
pub mod thread;

pub use list::BoardList;
#[cfg(not(target_family = "wasm"))]
pub use thread::BoardThread;

/// Where a view's store is: a project's, by a directory in it, or one
/// handed in whole (a test's, which refuses writes).
#[derive(Clone)]
pub enum Source {
    Root(PathBuf),
    Ready(Store),
}

impl Source {
    /// The directory the store is a project's by, if it is one's.
    pub fn root(&self) -> Option<PathBuf> {
        match self {
            Self::Root(root) => Some(root.clone()),
            Self::Ready(_) => None,
        }
    }

    fn open(&self) -> Result<Store, StoreError> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Root(root) => Store::from_dir(root),
            // (A plugin reaches no project: its stores are handed in.)
            #[cfg(target_family = "wasm")]
            Self::Root(_) => Err(StoreError::ReadOnly),
            Self::Ready(store) => Ok(store.clone()),
        }
    }
}

/// A read or a write of the store.
type Job<T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send>>;

/// Runs `job` over `source`'s store on a thread of its own, and `then` on
/// the UI thread with its answer.
fn run<T: Send + 'static>(
    background: &Background,
    source: Source,
    job: impl FnOnce(Store) -> Job<T> + Send + 'static,
    then: impl FnOnce(Result<T, StoreError>) + Send + 'static,
) {
    background.run(
        move || {
            #[cfg(not(target_family = "wasm"))]
            {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(StoreError::Io)?;
                runtime.block_on(job(source.open()?))
            }
            // A store handed in whole answers without waiting on anything.
            #[cfg(target_family = "wasm")]
            {
                let mut job = job(source.open()?);
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                loop {
                    if let std::task::Poll::Ready(answer) = job.as_mut().poll(&mut context) {
                        break answer;
                    }
                }
            }
        },
        then,
    );
}

/// Calls `written` on the UI thread whenever the log of the store at
/// `root` takes a write, from this process or another, until what is
/// returned is dropped.
#[cfg(not(target_family = "wasm"))]
fn follow(
    root: PathBuf,
    background: Background,
    written: impl Fn() + Send + Sync + 'static,
) -> tokio::sync::oneshot::Sender<()> {
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        let written = std::sync::Arc::new(written);
        runtime.block_on(async move {
            let Ok(store) = Store::from_dir(&root) else {
                return;
            };
            let mut lines = tokio::select! {
                biased;
                _ = &mut stopped => return,
                lines = store.subscribe(None) => match lines {
                    Ok(lines) => lines,
                    Err(_) => return,
                },
            };
            // The first line is the log as it stands, which the view has
            // read already.
            let _ = lines.next_line().await;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => return,
                    line = lines.next_line() => match line {
                        Ok(Some(_)) => {
                            let written = written.clone();
                            background.tell(move || written());
                        }
                        _ => return,
                    },
                }
            }
        });
    });
    stop
}

/// The tone a task's status is drawn in, as a chip or a dot.
fn tone(item: &Item) -> Tone {
    if item.is_closed {
        return Tone::Muted;
    }
    match item.status.trim() {
        "done" => Tone::Success,
        "review" => Tone::Info,
        "blocked" | "見送り" => Tone::Warning,
        "進行中" | "doing" | "設計中" => Tone::Accent,
        _ => Tone::Muted,
    }
}

/// What the sessions a board's tasks are bound to are doing, as whoever
/// placed the view says: a view draws it and does not find it out.
pub type Activity = Signal<std::collections::HashMap<SessionId, BoardSessionActivity>>;

/// What a board view and whoever placed it say to each other of the
/// sessions its tasks are bound to.
#[derive(Clone, Copy)]
pub struct Sessions {
    pub activity: Activity,
    /// The sessions the view's tasks are bound to, each once: the view
    /// says, for whoever placed it to find out what they are doing.
    pub bound: WriteSignal<Vec<SessionId>>,
}

impl Sessions {
    /// For a view nobody tells anything: no session is doing anything
    /// that is known.
    pub fn untold() -> Self {
        Self {
            activity: RwSignal::new(std::collections::HashMap::new()).into(),
            bound: RwSignal::new(Vec::new()).write_only(),
        }
    }
}

/// A chip in `into` for what the session `item` is bound to is doing,
/// there while the task is bound to one.
fn activity_chip(into: El, item: &Item, states: Activity) {
    let item = item.clone();
    into.each(
        move || {
            let state = states.with(|states| task_session_state(&item, states));
            state.into_iter().collect::<Vec<_>>()
        },
        |state| *state,
        |state| {
            let tone = match state {
                BoardSessionActivity::Failed => Tone::Danger,
                BoardSessionActivity::Running
                | BoardSessionActivity::ToolRunning
                | BoardSessionActivity::WaitingForApproval => Tone::Accent,
                _ => Tone::Muted,
            };
            // (Named for whoever looks for it; no rule is about it.)
            Chip {
                tone,
                text: state.label(),
            }
            .class("activity")
        },
    );
}

/// A refused write, as the notice line says it.
fn write_refusal(error: &StoreError) -> String {
    match error {
        StoreError::ReadOnly => chrome::READ_ONLY_REFUSAL.to_string(),
        other => other.to_string(),
    }
}
