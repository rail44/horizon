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

/// The main repository toplevel of `dir`, canonicalized: the parent of
/// `git rev-parse --git-common-dir`, as the knowledge layer and the board
/// resolve it, so a linked worktree names its main checkout.
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
