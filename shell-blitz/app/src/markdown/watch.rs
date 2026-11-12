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
