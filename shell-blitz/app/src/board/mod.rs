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
use horizon_board_view::chrome;

use crate::background::Background;

pub mod list;
pub mod thread;

pub use list::BoardList;
pub use thread::BoardThread;

/// The style sheet of both views.
pub fn css() -> String {
    [list::CSS, thread::CSS].concat()
}

/// Where a view's store is: a project's, by a directory in it, or one
/// handed in whole (a test's, which refuses writes).
#[derive(Clone)]
pub enum Source {
    Root(PathBuf),
    Ready(Store),
}

impl Source {
    fn open(&self) -> Result<Store, StoreError> {
        match self {
            Self::Root(root) => Store::from_dir(root),
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
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(StoreError::Io)?;
            runtime.block_on(job(source.open()?))
        },
        then,
    );
}

/// Calls `written` on the UI thread whenever the log of the store at
/// `root` takes a write, from this process or another, until what is
/// returned is dropped.
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

/// The class of a task's status: the tone its chip or dot is drawn in.
fn tone(item: &Item) -> &'static str {
    if item.is_closed {
        return "muted";
    }
    match item.status.trim() {
        "done" => "success",
        "review" => "info",
        "blocked" | "見送り" => "warning",
        "進行中" | "doing" | "設計中" => "accent",
        _ => "muted",
    }
}

/// A refused write, as the notice line says it.
fn write_refusal(error: &StoreError) -> String {
    match error {
        StoreError::ReadOnly => chrome::READ_ONLY_REFUSAL.to_string(),
        other => other.to_string(),
    }
}
