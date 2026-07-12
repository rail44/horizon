use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use uuid::Uuid;

const STATE_FILE_NAME: &str = "workspace.json";
const STATE_DIRECTORY_NAME: &str = "horizon";

#[derive(Debug)]
pub(crate) struct WorkspaceStateStore {
    path: PathBuf,
    last_saved_bytes: Option<Vec<u8>>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum LoadResult {
    Missing,
    Valid(String),
    Invalid(InvalidState),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InvalidState {
    UnsupportedVersion { found: u64, supported: u64 },
    Corrupt(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveResult {
    Saved,
    Unchanged,
}

impl WorkspaceStateStore {
    pub(crate) fn from_environment() -> Self {
        Self::new(resolve_state_path(
            std::env::var_os("HORIZON_WORKSPACE_STATE").as_deref(),
            std::env::var_os("XDG_STATE_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            &std::env::temp_dir(),
        ))
    }

    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            last_saved_bytes: None,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn load(&mut self, supported_version: u64) -> io::Result<LoadResult> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.last_saved_bytes = None;
                return Ok(LoadResult::Missing);
            }
            Err(error) => return Err(error),
        };

        let value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value,
            Err(error) => {
                self.last_saved_bytes = None;
                return Ok(LoadResult::Invalid(InvalidState::Corrupt(
                    error.to_string(),
                )));
            }
        };
        let Some(version) = value.get("version").and_then(serde_json::Value::as_u64) else {
            self.last_saved_bytes = None;
            return Ok(LoadResult::Invalid(InvalidState::Corrupt(
                "top-level `version` must be an unsigned integer".to_owned(),
            )));
        };
        if version != supported_version {
            self.last_saved_bytes = None;
            return Ok(LoadResult::Invalid(InvalidState::UnsupportedVersion {
                found: version,
                supported: supported_version,
            }));
        }

        let json = String::from_utf8(bytes.clone()).map_err(io::Error::other)?;
        self.last_saved_bytes = Some(bytes);
        Ok(LoadResult::Valid(json))
    }

    pub(crate) fn save(&mut self, json: &str) -> io::Result<SaveResult> {
        let bytes = json.as_bytes();
        if self.last_saved_bytes.as_deref() == Some(bytes) {
            return Ok(SaveResult::Unchanged);
        }

        atomic_replace(&self.path, bytes)?;
        self.last_saved_bytes = Some(bytes.to_vec());
        Ok(SaveResult::Saved)
    }
}

fn resolve_state_path(
    explicit: Option<&OsStr>,
    xdg_state_home: Option<&OsStr>,
    home: Option<&OsStr>,
    temp_dir: &Path,
) -> PathBuf {
    if let Some(path) = explicit.filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path) = xdg_state_home.filter(|value| !value.is_empty()) {
        return PathBuf::from(path)
            .join(STATE_DIRECTORY_NAME)
            .join(STATE_FILE_NAME);
    }
    if let Some(path) = home.filter(|value| !value.is_empty()) {
        return PathBuf::from(path)
            .join(".local/state")
            .join(STATE_DIRECTORY_NAME)
            .join(STATE_FILE_NAME);
    }
    temp_dir.join(STATE_DIRECTORY_NAME).join(STATE_FILE_NAME)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new(STATE_FILE_NAME))
        .to_string_lossy();
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-workspace-state-{label}-{}",
            Uuid::new_v4()
        ))
    }

    #[test]
    fn path_resolution_follows_environment_precedence() {
        let temp = Path::new("/tmp/fallback");
        assert_eq!(
            resolve_state_path(
                Some(OsStr::new("/override/state.json")),
                Some(OsStr::new("/xdg")),
                Some(OsStr::new("/home/user")),
                temp,
            ),
            PathBuf::from("/override/state.json")
        );
        assert_eq!(
            resolve_state_path(
                None,
                Some(OsStr::new("/xdg")),
                Some(OsStr::new("/home/user")),
                temp,
            ),
            PathBuf::from("/xdg/horizon/workspace.json")
        );
        assert_eq!(
            resolve_state_path(None, None, Some(OsStr::new("/home/user")), temp),
            PathBuf::from("/home/user/.local/state/horizon/workspace.json")
        );
        assert_eq!(
            resolve_state_path(None, None, None, temp),
            PathBuf::from("/tmp/fallback/horizon/workspace.json")
        );
    }

    #[test]
    fn missing_corrupt_and_newer_files_are_distinct() {
        let root = test_dir("load-errors");
        let path = root.join("workspace.json");
        let mut store = WorkspaceStateStore::new(path.clone());
        assert_eq!(store.load(1).unwrap(), LoadResult::Missing);

        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"not json").unwrap();
        assert!(matches!(
            store.load(1).unwrap(),
            LoadResult::Invalid(InvalidState::Corrupt(_))
        ));

        fs::write(&path, br#"{"version":2}"#).unwrap();
        assert_eq!(
            store.load(1).unwrap(),
            LoadResult::Invalid(InvalidState::UnsupportedVersion {
                found: 2,
                supported: 1,
            })
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_creates_parents_and_skips_unchanged_state() {
        let root = test_dir("save");
        let path = root.join("nested/workspace.json");
        let json = "{\"tabs\":[],\"version\":1}";
        let mut store = WorkspaceStateStore::new(path.clone());

        assert_eq!(store.save(json).unwrap(), SaveResult::Saved);
        assert_eq!(store.save(json).unwrap(), SaveResult::Unchanged);
        assert_eq!(fs::read_to_string(&path).unwrap(), json);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn loading_valid_state_seeds_unchanged_detection() {
        let root = test_dir("load-valid");
        let path = root.join("workspace.json");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"{\n  \"version\": 1, \"tabs\": []\n}\n").unwrap();
        let mut store = WorkspaceStateStore::new(path);

        let LoadResult::Valid(json) = store.load(1).unwrap() else {
            panic!("expected valid state");
        };
        assert_eq!(store.save(&json).unwrap(), SaveResult::Unchanged);
        fs::remove_dir_all(root).unwrap();
    }
}
