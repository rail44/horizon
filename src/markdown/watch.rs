//! A watch on the one file a view shows: a change of it is noted, and
//! taken in once the file has sat still for a moment (an editor's save is
//! several events, and a file half written is not worth reading).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::wake::Wake;

/// How long the file must sit still before it is read again.
const SETTLE: Duration = Duration::from_millis(300);

/// When the file last changed, if that has not been taken in yet.
#[derive(Clone, Default)]
pub(crate) struct Changed(Arc<Mutex<Option<Instant>>>);

impl Changed {
    /// Whether a change has sat still long enough to take in; it is then
    /// taken.
    pub(crate) fn settled(&self) -> bool {
        let mut changed = self.0.lock().unwrap();
        let settled = changed.is_some_and(|at| at.elapsed() >= SETTLE);
        if settled {
            *changed = None;
        }
        settled
    }
}

/// Whether an event's kind means the file's content may have changed.
/// inotify also reports opens and closes of a file that is only read, and
/// reading the file is what a change leads to.
fn changes_content(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Any | EventKind::Other => false,
    }
}

/// Watches `file`, noting its changes in `changed` and waking the page
/// once each has had time to settle. The watch is on the file's
/// directory: an editor that saves by replacing the file leaves a watch on
/// the file itself behind. `None` when there is no such directory to
/// watch; the view then shows the file as it was read.
pub(crate) fn on(file: PathBuf, changed: Changed, wake: Wake) -> Option<RecommendedWatcher> {
    let directory = (file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty()))?
    .to_path_buf();
    let handler = move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if !changes_content(&event.kind) || !event.paths.contains(&file) {
            return;
        }
        *changed.0.lock().unwrap() = Some(Instant::now());
        let wake = wake.clone();
        std::thread::spawn(move || {
            // A little past the moment it has settled.
            std::thread::sleep(SETTLE + Duration::from_millis(20));
            wake.wake();
        });
    };
    let mut watcher = notify::recommended_watcher(handler).ok()?;
    watcher
        .watch(&directory, RecursiveMode::NonRecursive)
        .ok()?;
    Some(watcher)
}

/// What [`next_change`] shares with the watch's thread: whether the change
/// has come, and who waits for it.
type Waiting = Arc<Mutex<(bool, Option<std::task::Waker>)>>;

/// Comes back once the file it watches changed and has sat still: for a
/// plugin waiting on a file (`files.wait-for-change`).
pub(crate) struct NextChange {
    waiting: Waiting,
    _watcher: RecommendedWatcher,
}

impl std::future::Future for NextChange {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let mut waiting = self.waiting.lock().unwrap();
        if waiting.0 {
            return std::task::Poll::Ready(());
        }
        waiting.1 = Some(context.waker().clone());
        std::task::Poll::Pending
    }
}

/// The next change of `file`, as [`on`] watches it. `None` when there is
/// no directory to watch it in.
pub(crate) fn next_change(file: PathBuf) -> Option<NextChange> {
    let directory = (file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty()))?
    .to_path_buf();
    let changed = Changed::default();
    let waiting: Waiting = Arc::default();
    let told = waiting.clone();
    let handler = move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if !changes_content(&event.kind) || !event.paths.contains(&file) {
            return;
        }
        *changed.0.lock().unwrap() = Some(Instant::now());
        let (changed, told) = (changed.clone(), told.clone());
        std::thread::spawn(move || {
            std::thread::sleep(SETTLE + Duration::from_millis(20));
            if changed.settled() {
                let mut told = told.lock().unwrap();
                told.0 = true;
                if let Some(waker) = told.1.take() {
                    waker.wake();
                }
            }
        });
    };
    let mut watcher = notify::recommended_watcher(handler).ok()?;
    watcher
        .watch(&directory, RecursiveMode::NonRecursive)
        .ok()?;
    Some(NextChange {
        waiting,
        _watcher: watcher,
    })
}
