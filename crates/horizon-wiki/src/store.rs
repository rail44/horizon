//! Where a project's wiki lives: `$XDG_DATA_HOME/horizon/wiki/<sanitized-root>/`
//! (`~/.local/share/...` without the variable), `<sanitized-root>` being the
//! project's main repository toplevel with every `/` replaced by `-`, as the
//! knowledge layer and the board name theirs. Nothing here is ever placed in
//! the repository.
//!
//! Inside: one `<name>.md` per page (`index.md` is the overview),
//! `concepts.md` (the concept file), `threads/<page uuid>.jsonl` (each page's
//! comment log) and `snapshots/<uuid>.md` (a page as it was when a comment
//! was made).

use std::path::{Path, PathBuf};

use crate::{Result, WikiError};

/// The concept file's name; it is not a page.
pub const CONCEPTS_FILE: &str = "concepts.md";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// A store at an explicit directory (tests, tools given a path).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store of the project whose main repository toplevel is
    /// `project_root`. `None` when no data home can be found.
    pub fn for_project(project_root: &Path) -> Option<Self> {
        Some(Self::at(Self::wiki_root()?.join(sanitize(project_root))))
    }

    /// The directory above every project's store, which also holds the
    /// installation's owner id ([`crate::thread::owner_id`]).
    pub fn wiki_root() -> Option<PathBuf> {
        Some(data_home()?.join("horizon").join("wiki"))
    }

    /// The store of the project `dir` is in (any worktree of it resolves to
    /// the main repository). `None` outside a repository.
    pub fn for_dir(dir: &Path) -> Option<Self> {
        Self::for_project(&project_root(dir)?)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file of page `name` (a slug: lowercase letters, digits, hyphens).
    pub fn page_path(&self, name: &str) -> Result<PathBuf> {
        validate_page_name(name)?;
        Ok(self.dir.join(format!("{name}.md")))
    }

    pub fn concepts_path(&self) -> PathBuf {
        self.dir.join(CONCEPTS_FILE)
    }
}

/// A page name is a slug, and `concepts` is the concept file's.
pub fn validate_page_name(name: &str) -> Result<()> {
    let slug = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    if !slug {
        return Err(WikiError::InvalidName(format!(
            "`{name}`: a page name uses lowercase letters, digits and hyphens"
        )));
    }
    if name == "concepts" {
        return Err(WikiError::InvalidName(
            "`concepts` is the concept file, not a page".into(),
        ));
    }
    Ok(())
}

/// The main repository toplevel of `dir`, read from the repository's own
/// files: the parent of the directory git keeps what its worktrees share
/// in. For where git cannot be run (a wasm guest); it reads what
/// `git rev-parse --git-common-dir` answers from, for a repository laid
/// out as git lays one out.
pub fn project_root_from_files(dir: &Path) -> Option<PathBuf> {
    let (at, git) = dir.ancestors().find_map(|at| {
        let git = at.join(".git");
        std::fs::symlink_metadata(&git).ok().map(|_| (at, git))
    })?;
    // The main checkout: `.git` is the directory itself.
    if git.is_dir() {
        return Some(at.to_path_buf());
    }
    // A linked worktree: `.git` names its own directory, which names the
    // shared one.
    let text = std::fs::read_to_string(&git).ok()?;
    let own = at.join(text.trim().strip_prefix("gitdir:")?.trim());
    let common = match std::fs::read_to_string(own.join("commondir")) {
        Ok(shared) => own.join(shared.trim()),
        // (A submodule's: its own directory is the shared one.)
        Err(_) => own,
    };
    plain(&common).parent().map(Path::to_path_buf)
}

/// `path` with its `.` and `..` taken out, without asking the file
/// system.
fn plain(path: &Path) -> PathBuf {
    let mut plain = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => _ = plain.pop(),
            other => plain.push(other),
        }
    }
    plain
}

/// The main repository toplevel of `dir`, canonicalized: the parent of
/// `git rev-parse --git-common-dir`, as the knowledge layer and the board
/// resolve it, so a linked worktree names its main checkout.
#[cfg(target_family = "wasm")]
pub fn project_root(dir: &Path) -> Option<PathBuf> {
    project_root_from_files(dir)
}

/// The main repository toplevel of `dir`, canonicalized: the parent of
/// `git rev-parse --git-common-dir`, as the knowledge layer and the board
/// resolve it, so a linked worktree names its main checkout.
#[cfg(not(target_family = "wasm"))]
pub fn project_root(dir: &Path) -> Option<PathBuf> {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    std::fs::canonicalize(common.parent()?).ok()
}

/// `$HORIZON_DATA_HOME`, else `$XDG_DATA_HOME`, else `~/.local/share`: as
/// the board and the agent resolve theirs.
fn data_home() -> Option<PathBuf> {
    let named = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    match named("HORIZON_DATA_HOME").or_else(|| named("XDG_DATA_HOME")) {
        Some(value) => Some(PathBuf::from(value)),
        None => std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(|home| PathBuf::from(home).join(".local").join("share")),
    }
}

fn sanitize(project_root: &Path) -> String {
    project_root.to_string_lossy().replace('/', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_found_from_its_files_as_git_finds_it() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        // A main checkout, and a worktree of it laid out as git lays one
        // out.
        let main = root.join("project");
        std::fs::create_dir_all(main.join(".git/worktrees/side")).unwrap();
        std::fs::create_dir_all(main.join("src/deep")).unwrap();
        let side = root.join("elsewhere/side");
        std::fs::create_dir_all(side.join("src")).unwrap();
        let own = main.join(".git/worktrees/side");
        std::fs::write(side.join(".git"), format!("gitdir: {}\n", own.display())).unwrap();
        std::fs::write(own.join("commondir"), "../..\n").unwrap();

        assert_eq!(project_root_from_files(&main), Some(main.clone()));
        assert_eq!(
            project_root_from_files(&main.join("src/deep")),
            Some(main.clone())
        );
        assert_eq!(
            project_root_from_files(&side.join("src")),
            Some(main.clone())
        );
        assert_eq!(project_root_from_files(&root.join("elsewhere")), None);
    }

    /// The same answer as git's, for this repository's own checkout
    /// (which may be a worktree).
    #[test]
    fn a_project_found_from_its_files_is_the_one_git_names() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(named) = project_root(here) else {
            return;
        };
        let read = project_root_from_files(here).and_then(|root| root.canonicalize().ok());
        assert_eq!(read, Some(named));
    }

    #[test]
    fn the_store_is_named_after_the_sanitized_project_root() {
        assert_eq!(
            sanitize(Path::new("/home/me/src/horizon")),
            "-home-me-src-horizon"
        );
        let store = Store::at("/data/wiki/x");
        assert_eq!(
            store.page_path("sandbox").unwrap(),
            PathBuf::from("/data/wiki/x/sandbox.md")
        );
        assert_eq!(
            store.concepts_path(),
            PathBuf::from("/data/wiki/x/concepts.md")
        );
    }

    #[test]
    fn page_names_are_slugs_and_concepts_is_reserved() {
        for good in ["index", "agent-tools", "a1"] {
            assert!(validate_page_name(good).is_ok(), "{good}");
        }
        for bad in ["", "../x", "Index", "a b", "-x", "x.md", "concepts"] {
            assert!(validate_page_name(bad).is_err(), "{bad}");
        }
    }
}
