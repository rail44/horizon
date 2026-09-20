//! The preview pane's artifact watcher.
//!
//! cargo does not rewrite a `.wasm` in place — it unlinks the old file and
//! links a new one — so watching the file itself loses the inode on the
//! first rebuild. The watch is on the containing directory and
//! [`event_touches`] filters its traffic down to the one artifact; the
//! events of a single rebuild arrive in a burst, so a reload waits for
//! [`RELOAD_DEBOUNCE`] of quiet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::{FutureExt as _, StreamExt as _};
use gpui::{AsyncApp, Task};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

/// How long the artifact must sit still before a reload starts.
pub(crate) const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);

/// The directory a watch on `artifact` has to be placed on.
pub(crate) fn watch_root(artifact: &Path) -> Option<&Path> {
    artifact
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Whether a filesystem event concerns `artifact`. The watch covers a whole
/// directory, and a cargo build touches many files in it.
pub(crate) fn event_touches(event_paths: &[PathBuf], artifact: &Path) -> bool {
    event_paths.iter().any(|path| path == artifact)
}

/// A live watch. Dropping it stops the watcher and the debounce task.
pub(crate) struct ArtifactWatch {
    _watcher: RecommendedWatcher,
    _debounce: Task<()>,
}

/// Watches `artifact` and runs `on_change` once per quiet burst of events.
/// Returns `None` when the watch cannot be placed (no parent directory, or
/// the directory does not exist yet); the caller keeps working without a
/// watch rather than failing the pane.
pub(crate) fn watch(
    artifact: PathBuf,
    cx: &mut gpui::App,
    mut on_change: impl FnMut(&mut gpui::App) + 'static,
) -> Option<ArtifactWatch> {
    let root = watch_root(&artifact)?.to_path_buf();
    let (sender, receiver) = mpsc::unbounded::<()>();
    let watched = artifact.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else {
            return;
        };
        if event_touches(&event.paths, &watched) {
            let _ = sender.unbounded_send(());
        }
    })
    .ok()?;
    watcher.watch(&root, RecursiveMode::NonRecursive).ok()?;
    let debounce = cx.spawn(async move |cx| debounce_loop(receiver, cx, &mut on_change).await);
    Some(ArtifactWatch {
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
