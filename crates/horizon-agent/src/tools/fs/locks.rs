use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
};

type PathLock = Arc<Mutex<()>>;

fn table() -> &'static Mutex<HashMap<PathBuf, Weak<Mutex<()>>>> {
    static TABLE: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_for(path: &Path) -> PathLock {
    let mut locks = table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

/// Holds every requested path lock in stable lexical order. Sorting makes
/// overlapping multi-file patches deadlock-free even when two sessions ask
/// for the same files in opposite orders. The weak global registry forgets a
/// path as soon as no operation still holds its lock.
pub(super) struct FileLocks {
    locks: Vec<PathLock>,
}

impl FileLocks {
    pub(super) fn acquire(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        Self {
            locks: paths.iter().map(|path| lock_for(path)).collect(),
        }
    }

    pub(super) fn hold(&self) -> Vec<MutexGuard<'_, ()>> {
        self.locks
            .iter()
            .map(|lock| lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
            .collect()
    }
}
