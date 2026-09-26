//! Where a board view's store comes from, and where its work runs.
//!
//! Both are the same question asked twice: the shell hands a view a
//! project directory and gets a log-backed store with a daemon behind it;
//! a preview hands it a store that is already built and holds its events in
//! memory. Everything else in the views works on whatever [`run_store_job`]
//! hands back, so no call site opens a store or picks an executor itself.

use std::future::Future;
use std::pin::Pin;

use gpui::AsyncApp;
use horizon_board::{Store, StoreError};

#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

/// One unit of store work, boxed so the execution split below does not have
/// to be generic over the future's type.
pub(crate) type StoreJob<T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send>>;

/// Where a view's store comes from.
#[derive(Clone)]
pub(crate) enum BoardStoreSource {
    /// A directory inside the project. `Store::from_dir` collapses a linked
    /// worktree onto its main git root by shelling out to git, so this is
    /// resolved where the job runs rather than on the UI thread.
    #[cfg(not(target_family = "wasm"))]
    Root(PathBuf),
    /// A store the caller already built.
    Ready(Store),
}

impl BoardStoreSource {
    /// The store to read or write through.
    pub(crate) fn open(&self) -> Result<Store, StoreError> {
        match self {
            #[cfg(not(target_family = "wasm"))]
            Self::Root(root) => Store::from_dir(root),
            Self::Ready(store) => Ok(store.clone()),
        }
    }

    /// The project directory this source resolves from, if it has one.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn root(&self) -> Option<&std::path::Path> {
        match self {
            Self::Root(root) => Some(root),
            Self::Ready(_) => None,
        }
    }
}

/// The source a board pane the shell opens reads through: the active
/// session's `workspace_root` wins, and when that is absent (no active
/// session, or a session with no recorded root) the shell process's own cwd
/// stands in. Both are *starting* directories -- `Store::from_dir` does the
/// worktree -> main-root collapse. With neither, the view gets an empty
/// in-memory store: an empty board that answers every write with
/// [`StoreError::ReadOnly`].
#[cfg(not(target_family = "wasm"))]
pub(crate) fn shell_store_source(
    session_root: Option<PathBuf>,
    cwd: Option<PathBuf>,
) -> BoardStoreSource {
    match session_root.or(cwd) {
        Some(root) => BoardStoreSource::Root(root),
        None => BoardStoreSource::Ready(Store::in_memory(Vec::new())),
    }
}

/// Runs one store job to completion and returns its result.
///
/// Native: on the background executor, inside a current-thread tokio
/// runtime. Reads are blocking file folds and writes are remoc rtc calls to
/// `horizon-logd`, so both want a thread other than the UI one and the
/// writes need a reactor.
#[cfg(not(target_family = "wasm"))]
pub(crate) async fn run_store_job<T, F>(
    cx: &AsyncApp,
    source: BoardStoreSource,
    job: F,
) -> Result<T, StoreError>
where
    F: FnOnce(Store) -> StoreJob<T> + Send + 'static,
    T: Send + 'static,
{
    cx.background_executor()
        .spawn(async move {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(StoreError::Io)?;
            let store = source.open()?;
            runtime.block_on(job(store))
        })
        .await
}

/// Runs one store job to completion and returns its result.
///
/// wasm: inline on gpui's executor. A guest has no threads and no tokio, and
/// the only store it can hold is in-memory — reads fold a vector it already
/// has, and writes resolve to [`StoreError::ReadOnly`] without reaching a
/// reactor, so neither ever blocks.
#[cfg(target_family = "wasm")]
pub(crate) async fn run_store_job<T, F>(
    _cx: &AsyncApp,
    source: BoardStoreSource,
    job: F,
) -> Result<T, StoreError>
where
    F: FnOnce(Store) -> StoreJob<T> + Send + 'static,
    T: Send + 'static,
{
    job(source.open()?).await
}

#[cfg(test)]
mod tests {
    use super::BoardStoreSource;
    use horizon_board::{sample_envelopes, BoardEvent, Item, Store, StoreError};
    use std::path::{Path, PathBuf};

    fn ready_source() -> BoardStoreSource {
        let item = Item {
            id: 7,
            title: "handed in".into(),
            rank: "a".into(),
            ..Item::default()
        };
        let events = [BoardEvent::ItemStored { id: 7, item }];
        BoardStoreSource::Ready(Store::in_memory(sample_envelopes(events)))
    }

    /// The hand-off the preview constructor uses: reads answer from the
    /// envelopes the caller built the store with, and writes never leave it.
    #[test]
    fn a_ready_source_answers_from_its_own_events_and_refuses_writes() {
        let source = ready_source();
        let opened = source.open().expect("a ready source opens");
        let listed = opened.list(None, true).expect("list").items;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "handed in");

        // Opening again hands out the same events, not a fresh read.
        let again = source.open().expect("a ready source reopens");
        assert_eq!(again.list(None, true).expect("list").items.len(), 1);

        let refused = futures::executor::block_on(opened.set_status(7, "doing"));
        let refused_as_read_only = matches!(refused, Err(StoreError::ReadOnly));
        assert!(refused_as_read_only, "a ready source must refuse writes");
    }

    /// The shell asks a view for its project directory (to watch the
    /// board); only a root-resolved source has one.
    #[test]
    fn a_root_source_reports_its_directory() {
        let source = BoardStoreSource::Root(PathBuf::from("/tmp/horizon-board-source"));
        assert_eq!(source.root(), Some(Path::new("/tmp/horizon-board-source")));
        assert!(BoardStoreSource::Ready(Store::in_memory(Vec::new()))
            .root()
            .is_none());
    }

    /// The session's root wins over the cwd, the cwd stands in when there
    /// is none, and with neither the view reads an empty board rather than
    /// a directory nobody named.
    #[test]
    fn the_shell_source_prefers_the_session_root_then_the_cwd() {
        let session = PathBuf::from("/tmp/horizon-board-session");
        let cwd = PathBuf::from("/tmp/horizon-board-cwd");
        assert_eq!(
            super::shell_store_source(Some(session.clone()), Some(cwd.clone())).root(),
            Some(session.as_path())
        );
        assert_eq!(
            super::shell_store_source(None, Some(cwd.clone())).root(),
            Some(cwd.as_path())
        );
        let none = super::shell_store_source(None, None);
        assert!(none.root().is_none());
        assert!(none
            .open()
            .expect("an empty in-memory store opens")
            .list(None, true)
            .expect("list")
            .items
            .is_empty());
    }
}
