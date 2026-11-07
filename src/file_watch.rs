//! A watch on one file, shared by the views that show a file from disk
//! (the preview pane's plugin artifact, the Markdown view's document).
//!
//! Writers rarely rewrite a file in place: cargo unlinks the old `.wasm`
//! and links a new one, and editors save by renaming a temporary file over
//! the old one. Watching the file itself loses the inode on the first such
//! write, so the watch is on the containing directory and
//! [`event_touches`] filters its traffic down to the one file; the events
//! of a single write arrive in a burst, so a reload waits for
//! [`RELOAD_DEBOUNCE`] of quiet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::{FutureExt as _, StreamExt as _};
use gpui::{AsyncApp, Task};
use notify::event::{EventKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// How long the file must sit still before a reload starts.
pub(crate) const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);

/// The directory a watch on `file` has to be placed on.
pub(crate) fn watch_root(file: &Path) -> Option<&Path> {
    file.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Whether a filesystem event concerns `file`. The watch covers a whole
/// directory, and a cargo build touches many files in it.
pub(crate) fn event_touches(event_paths: &[PathBuf], file: &Path) -> bool {
    event_paths.iter().any(|path| path == file)
}

/// Whether an event kind means the file's content may have changed.
/// inotify also reports opens and closes of a file that is only read, and
/// loading a plugin or a document reads the file, so reacting to access events makes
/// every load schedule the next one.
pub(crate) fn event_changes_content(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Any | EventKind::Other => false,
    }
}

/// A live watch. Dropping it stops the watcher and the debounce task.
pub(crate) struct FileWatch {
    _watcher: RecommendedWatcher,
    _debounce: Task<()>,
}

/// Watches `file` and runs `on_change` once per quiet burst of events.
/// Returns `None` when the watch cannot be placed (no parent directory, or
/// the directory does not exist yet); the caller keeps working without a
/// watch rather than failing the pane.
pub(crate) fn watch(
    file: PathBuf,
    cx: &mut gpui::App,
    mut on_change: impl FnMut(&mut gpui::App) + 'static,
) -> Option<FileWatch> {
    let root = watch_root(&file)?.to_path_buf();
    let (sender, receiver) = mpsc::unbounded::<()>();
    let watched = file.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if event_changes_content(&event.kind) && event_touches(&event.paths, &watched) {
            let _ = sender.unbounded_send(());
        }
    })
    .ok()?;
    watcher.watch(&root, RecursiveMode::NonRecursive).ok()?;
    let debounce = cx.spawn(async move |cx| debounce_loop(receiver, cx, &mut on_change).await);
    Some(FileWatch {
        _watcher: watcher,
        _debounce: debounce,
    })
}

async fn debounce_loop(
    mut receiver: UnboundedReceiver<()>,
    cx: &mut AsyncApp,
    on_change: &mut impl FnMut(&mut gpui::App),
) {
    while receiver.next().await.is_some() {
        loop {
            let timer = cx.background_executor().timer(RELOAD_DEBOUNCE);
            futures::select_biased! {
                next = receiver.next().fuse() => {
                    if next.is_none() {
                        return;
                    }
                }
                () = timer.fuse() => break,
            }
        }
        cx.update(|cx| on_change(cx));
    }
}

#[cfg(test)]
mod tests;
