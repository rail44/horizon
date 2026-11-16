//! A watch on the one file a view shows: a change of it is noted, and
//! taken in once the file has sat still for a moment (an editor's save is
//! several events, and a file half written is not worth reading).
//!
//! Every watch of the process is on one watcher of the system's ([`Hub`]).
//! A watcher is an inotify instance, of which a user has a small number
//! (128 by default on Linux, shared with every other program of theirs):
//! one for each file watched ran out with a few panes open, and a watch
//! that could not be made left its view showing the file as it was.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
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
    /// Notes a change now. Whether none was waiting to be taken in: the
    /// one that starts a wait for the file to sit still (one wait serves
    /// every change until it is taken in, however fast they come).
    fn note(&self) -> bool {
        self.0.lock().unwrap().replace(Instant::now()).is_none()
    }

    /// Whether a change waits that has sat still long enough to take in.
    fn still(&self) -> bool {
        (self.0.lock().unwrap()).is_some_and(|at| at.elapsed() >= SETTLE)
    }

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

/// Told that the file it is for changed.
type Told = Arc<dyn Fn() + Send + Sync>;

/// Who is told of what, and which directories are watched for them.
#[derive(Default)]
struct Table {
    next: u64,
    /// Who is told of a change of which file.
    told: HashMap<u64, (PathBuf, Told)>,
    /// How many watch a file in each directory watched.
    directories: HashMap<PathBuf, usize>,
}

/// The one watcher, and the table of who it tells. They are locked apart:
/// asking the watcher to watch a directory waits for the watcher's own
/// thread, which may be handing out an event and waiting for the table.
/// So the table is never held while the watcher is asked anything.
#[derive(Default)]
struct Hub {
    /// Made when the first file is watched; kept from then on. Held for
    /// the whole of a watch's making or ending, which keeps those in turn.
    watcher: Mutex<Option<RecommendedWatcher>>,
    table: Mutex<Table>,
}

fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(Hub::default)
}

/// The watcher's events: whoever watches a file an event is of is told.
fn handle(event: notify::Result<notify::Event>) {
    let Ok(event) = event else {
        return;
    };
    if !changes_content(&event.kind) {
        return;
    }
    // (Cloned out: what is told may watch or stop watching.)
    let told: Vec<Told> = {
        let table = hub().table.lock().unwrap();
        (table.told.values())
            .filter(|(file, _)| event.paths.contains(file))
            .map(|(_, told)| told.clone())
            .collect()
    };
    for told in told {
        told();
    }
}

/// A file watched, until this is dropped.
pub(crate) struct Watched(u64);

impl Drop for Watched {
    fn drop(&mut self) {
        let mut watcher = hub().watcher.lock().unwrap();
        // Whether its directory has nothing left to be watched for.
        let emptied = {
            let mut table = hub().table.lock().unwrap();
            let Some((file, _)) = table.told.remove(&self.0) else {
                return;
            };
            let Some(directory) = file.parent().map(Path::to_path_buf) else {
                return;
            };
            let left = table.directories.get_mut(&directory).map(|watching| {
                *watching -= 1;
                *watching
            });
            (left == Some(0)).then(|| {
                table.directories.remove(&directory);
                directory
            })
        };
        if let (Some(directory), Some(watcher)) = (emptied, watcher.as_mut()) {
            let _ = watcher.unwatch(&directory);
        }
    }
}

/// Has `told` called for each change of `file`'s content. The watch is on
/// the file's directory: an editor that saves by replacing the file leaves
/// a watch on the file itself behind. `None`, with why on stderr, when the
/// file cannot be watched.
fn watch(file: &Path, told: Told) -> Option<Watched> {
    let directory = (file.parent()).filter(|parent| !parent.as_os_str().is_empty())?;
    let mut watcher = hub().watcher.lock().unwrap();
    if watcher.is_none() {
        match notify::recommended_watcher(handle) {
            Ok(made) => *watcher = Some(made),
            Err(error) => {
                eprintln!("horizon: no file can be watched: {error}");
                return None;
            }
        }
    }
    // (The table is let go of before the watcher is asked: see `Hub`.)
    let watched = (hub().table.lock().unwrap())
        .directories
        .contains_key(directory);
    if !watched {
        let watching = (watcher.as_mut())?.watch(directory, RecursiveMode::NonRecursive);
        if let Err(error) = watching {
            eprintln!("horizon: {} is not watched: {error}", file.display());
            return None;
        }
    }
    let mut table = hub().table.lock().unwrap();
    *table
        .directories
        .entry(directory.to_path_buf())
        .or_default() += 1;
    let id = table.next;
    table.next += 1;
    table.told.insert(id, (file.to_path_buf(), told));
    Some(Watched(id))
}

/// Watches `file`, noting its changes in `changed` and waking the page
/// once each has had time to settle. `None` when it cannot be watched;
/// the view then shows the file as it was read.
pub(crate) fn on(file: PathBuf, changed: Changed, wake: Wake) -> Option<Watched> {
    let told = move || {
        if !changed.note() {
            return;
        }
        let (changed, wake) = (changed.clone(), wake.clone());
        std::thread::spawn(move || {
            // Until a little past the moment it has settled.
            while !changed.still() {
                std::thread::sleep(SETTLE + Duration::from_millis(20));
            }
            wake.wake();
        });
    };
    watch(&file, Arc::new(told))
}

/// What a [`Kept`] watch shares with its thread: whether a change has
/// come that nobody was told of yet, and who waits for one.
type Waiting = Arc<Mutex<(bool, Option<std::task::Waker>)>>;

/// A file watched for as long as this is kept, for a plugin that holds a
/// watch on it (`files.watch`): each change that has sat still is told
/// once, at the next wait. It is kept between one wait and the next, so a
/// change that comes while the plugin does not wait -- it is reading the
/// file, or has not asked again yet -- is told then and not lost.
pub(crate) struct Kept {
    waiting: Waiting,
    _watched: Watched,
}

/// Comes back once the file a [`Kept`] watches changed and has sat still.
pub(crate) struct NextChange(Arc<Kept>);

impl std::future::Future for NextChange {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let mut waiting = self.0.waiting.lock().unwrap();
        if std::mem::take(&mut waiting.0) {
            return std::task::Poll::Ready(());
        }
        waiting.1 = Some(context.waker().clone());
        std::task::Poll::Pending
    }
}

impl Kept {
    /// The next change of the file that nobody was told of.
    pub(crate) fn next(self: &Arc<Self>) -> NextChange {
        NextChange(self.clone())
    }
}

/// A watch on `file`, as [`on`] watches it, kept for a plugin's waits.
/// `None` when it cannot be watched.
pub(crate) fn kept(file: PathBuf) -> Option<Arc<Kept>> {
    let changed = Changed::default();
    let waiting: Waiting = Arc::default();
    let waits = waiting.clone();
    let told = move || {
        if !changed.note() {
            return;
        }
        let (changed, waits) = (changed.clone(), waits.clone());
        std::thread::spawn(move || {
            while !changed.settled() {
                std::thread::sleep(SETTLE + Duration::from_millis(20));
            }
            let mut waits = waits.lock().unwrap();
            waits.0 = true;
            if let Some(waker) = waits.1.take() {
                waker.wake();
            }
        });
    };
    let watched = watch(&file, Arc::new(told))?;
    Some(Arc::new(Kept {
        waiting,
        _watched: watched,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Watches are made and let go of while events of other files are
    /// being handed out: neither waits for the other for good.
    #[test]
    fn a_watch_is_made_while_events_are_handed_out() {
        let root = std::env::temp_dir().join(format!("horizon-watch-busy-{}", std::process::id()));
        std::fs::create_dir_all(root.join("busy")).unwrap();
        let root = root.canonicalize().unwrap();
        // One file written over and over, and watched: its events keep the
        // watcher's thread handing out.
        let busy = root.join("busy/file.md");
        std::fs::write(&busy, "0").unwrap();
        let _kept = kept(busy.clone()).expect("a watch");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut turn = 0u64;
            while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                turn += 1;
                let _ = std::fs::write(&busy, turn.to_string());
            }
        });
        // Meanwhile, directories of their own are watched and let go of.
        let (done, finished) = std::sync::mpsc::channel();
        let making = root.clone();
        std::thread::spawn(move || {
            for at in 0..400 {
                let directory = making.join(format!("d{at}"));
                std::fs::create_dir_all(&directory).unwrap();
                let file = directory.join("file.md");
                std::fs::write(&file, "one").unwrap();
                drop(kept(file).expect("a watch"));
            }
            let _ = done.send(());
        });
        let finished = finished.recv_timeout(Duration::from_secs(60));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = writer.join();
        assert!(finished.is_ok(), "making a watch waited for good");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Many files are watched on the one watcher: more than a user may
    /// have watchers of the system's.
    #[test]
    fn many_files_are_watched_on_one_watcher() {
        let directory =
            std::env::temp_dir().join(format!("horizon-watch-many-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let watches: Vec<_> = (0..300)
            .map(|at| {
                let file = directory.join(format!("{at}.md"));
                std::fs::write(&file, "one").unwrap();
                (file.clone(), kept(file).expect("a watch"))
            })
            .collect();
        // A change of the last of them is told to it and to no other.
        let (file, last) = watches.last().unwrap();
        std::fs::write(file, "two").unwrap();
        let started = Instant::now();
        while !last.waiting.lock().unwrap().0 {
            assert!(started.elapsed() < Duration::from_secs(10), "not told");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!watches[0].1.waiting.lock().unwrap().0);
        // Let go of, they leave nothing watched.
        drop(watches);
        let table = hub().table.lock().unwrap();
        assert!(table.told.is_empty() && table.directories.is_empty());
        drop(table);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
