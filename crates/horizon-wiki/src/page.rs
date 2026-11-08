//! Pages: Markdown files opening with a `---` frontmatter block. Every page
//! carries `id: urn:uuid:…`, its identity across renames and the key its
//! comment threads are filed under; writes keep it, and a page written
//! without one gets one.

use std::path::Path;

use serde::Deserialize;
use uuid::Uuid;

use crate::store::{Store, CONCEPTS_FILE};
use crate::{Result, WikiError};

const ID_KEY: &str = "id";
const URN_UUID: &str = "urn:uuid:";

/// One exact-string replacement for [`Store::edit_page`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Edit {
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageInfo {
    pub name: String,
    pub id: Option<Uuid>,
    pub title: Option<String>,
}

/// The frontmatter's `key: value` lines and where the body starts.
struct Frontmatter<'a> {
    entries: Vec<(&'a str, &'a str)>,
    /// Byte offset of the line after the closing `---`.
    body_start: usize,
}

/// Recognizes the same block the Markdown view splits off
/// (`src/markdown/document.rs`): an opening `---` line (LF or CRLF) and the
/// first `---` or `...` line after it, so a page's body starts at the same
/// byte here and in the view.
fn frontmatter(text: &str) -> Option<Frontmatter<'_>> {
    let (rest, mut offset) = match text.strip_prefix("---\n") {
        Some(rest) => (rest, 4),
        None => (text.strip_prefix("---\r\n")?, 5),
    };
    let mut entries = Vec::new();
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            return Some(Frontmatter {
                entries,
                body_start: offset,
            });
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            entries.push((key.trim(), value.trim()));
        }
    }
    None
}

impl<'a> Frontmatter<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, value)| *value)
    }
}

/// A page's `id`, when its frontmatter has a well-formed one.
pub fn page_id(text: &str) -> Option<Uuid> {
    let value = frontmatter(text)?.get(ID_KEY)?;
    Uuid::parse_str(value.strip_prefix(URN_UUID)?).ok()
}

pub fn urn(id: Uuid) -> String {
    format!("{URN_UUID}{id}")
}

/// The body below the frontmatter (the whole text when there is none).
pub fn body(text: &str) -> &str {
    frontmatter(text).map_or(text, |fm| &text[fm.body_start..])
}

/// `text` (which opens with a frontmatter block) with `id` as the block's
/// first entry.
fn with_id(text: &str, id: Uuid) -> String {
    let (fence, rest) = match text.strip_prefix("---\r\n") {
        Some(rest) => ("---\r\n", rest),
        None => ("---\n", &text[4..]),
    };
    format!("{fence}{ID_KEY}: {}\n{rest}", urn(id))
}

fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().expect("store paths have a parent");
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{}.tmp", Uuid::new_v4()));
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })?;
    Ok(())
}

impl Store {
    pub fn read_page(&self, name: &str) -> Result<String> {
        let path = self.page_path(name)?;
        std::fs::read_to_string(&path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => WikiError::NotFound(format!("page `{name}`")),
            _ => error.into(),
        })
    }

    fn existing(&self, name: &str) -> Result<Option<String>> {
        match self.read_page(name) {
            Ok(text) => Ok(Some(text)),
            Err(WikiError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Writes page `name` whole. `text` must open with a frontmatter block;
    /// an `id` it leaves out is the existing page's, or a new one, and an
    /// `id` different from the existing page's is refused.
    pub fn write_page(&self, name: &str, text: &str) -> Result<Uuid> {
        let path = self.page_path(name)?;
        if frontmatter(text).is_none() {
            return Err(WikiError::Invalid(
                "a page opens with a `---` frontmatter block".into(),
            ));
        }
        let existing = self.existing(name)?.as_deref().and_then(page_id);
        let (id, text) = match (page_id(text), existing) {
            (Some(given), Some(kept)) if given != kept => {
                return Err(WikiError::Invalid(format!(
                    "page `{name}` is {}; its id cannot change",
                    urn(kept)
                )))
            }
            (Some(given), _) => (given, text.to_string()),
            (None, kept) => {
                let id = kept.unwrap_or_else(Uuid::new_v4);
                (id, with_id(text, id))
            }
        };
        write_atomically(&path, &text)?;
        Ok(id)
    }

    /// Applies `edits` in order, each an exact string that must occur once in
    /// the page as it stands after the previous one. All or nothing; the
    /// page's `id` must survive.
    pub fn edit_page(&self, name: &str, edits: &[Edit]) -> Result<()> {
        if edits.is_empty() {
            return Err(WikiError::Invalid("no edits given".into()));
        }
        let mut text = self.read_page(name)?;
        let id = page_id(&text);
        for (index, edit) in edits.iter().enumerate() {
            if edit.old.is_empty() {
                return Err(WikiError::Invalid(format!(
                    "edit {}: `old` is empty",
                    index + 1
                )));
            }
            match text.matches(edit.old.as_str()).count() {
                1 => text = text.replacen(edit.old.as_str(), &edit.new, 1),
                0 => {
                    return Err(WikiError::Invalid(format!(
                        "edit {}: `old` is not in the page",
                        index + 1
                    )))
                }
                n => {
                    return Err(WikiError::Invalid(format!(
                        "edit {}: `old` occurs {n} times; include more context",
                        index + 1
                    )))
                }
            }
        }
        if page_id(&text) != id {
            return Err(WikiError::Invalid(
                "the edits would change the page's id".into(),
            ));
        }
        write_atomically(&self.page_path(name)?, &text)
    }

    /// Page `name`'s id, giving the page one first when it has none.
    pub fn ensure_page_id(&self, name: &str) -> Result<Uuid> {
        let text = self.read_page(name)?;
        if let Some(id) = page_id(&text) {
            return Ok(id);
        }
        self.write_page(name, &text)
    }

    /// Every page, by name.
    pub fn list_pages(&self) -> Result<Vec<PageInfo>> {
        let entries = match std::fs::read_dir(self.dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut pages = Vec::new();
        for entry in entries {
            let file_name = entry?.file_name().to_string_lossy().into_owned();
            let Some(name) = file_name.strip_suffix(".md") else {
                continue;
            };
            if file_name == CONCEPTS_FILE || crate::store::validate_page_name(name).is_err() {
                continue;
            }
            let text = self.read_page(name)?;
            let title = frontmatter(&text).and_then(|fm| fm.get("title").map(str::to_string));
            pages.push(PageInfo {
                name: name.to_string(),
                id: page_id(&text),
                title,
            });
        }
        pages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(pages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        (dir, store)
    }

    const PAGE: &str = "---\nconcept: sandbox\ntitle: Sandbox\n---\n\n# Sandbox\n\nBody.\n";

    #[test]
    fn a_new_page_gets_an_id_that_later_writes_keep() {
        let (_dir, store) = store();
        let id = store.write_page("sandbox", PAGE).unwrap();
        let text = store.read_page("sandbox").unwrap();
        assert!(text.starts_with(&format!("---\nid: {}\nconcept: sandbox\n", urn(id))));
        assert_eq!(page_id(&text), Some(id));
        assert_eq!(body(&text), "\n# Sandbox\n\nBody.\n");

        let rewritten = store
            .write_page("sandbox", &PAGE.replace("Body.", "New body."))
            .unwrap();
        assert_eq!(rewritten, id);
        assert!(store.read_page("sandbox").unwrap().contains("New body."));
    }

    /// The body starts where the Markdown view's does
    /// (`src/markdown/document.rs`): CRLF fences and a `...` close count.
    #[test]
    fn the_body_split_matches_the_markdown_view() {
        assert_eq!(body("---\r\ntitle: T\r\n---\r\nbody\r\n"), "body\r\n");
        assert_eq!(body("---\ntitle: T\n...\nbody\n"), "body\n");
        assert_eq!(
            body("---\ntitle: T\nno close\n"),
            "---\ntitle: T\nno close\n"
        );
        let id = Uuid::new_v4();
        let crlf = with_id("---\r\ntitle: T\r\n---\r\nbody\r\n", id);
        assert_eq!(page_id(&crlf), Some(id));
        assert_eq!(body(&crlf), "body\r\n");
    }

    #[test]
    fn a_write_cannot_change_a_page_id_or_drop_the_frontmatter() {
        let (_dir, store) = store();
        store.write_page("sandbox", PAGE).unwrap();
        let other = PAGE.replace(
            "---\nconcept",
            &format!("---\nid: {}\nconcept", urn(Uuid::new_v4())),
        );
        assert!(store.write_page("sandbox", &other).is_err());
        assert!(store.write_page("sandbox", "# no frontmatter\n").is_err());
    }

    #[test]
    fn edits_apply_in_order_all_or_nothing() {
        let (_dir, store) = store();
        let id = store.write_page("sandbox", PAGE).unwrap();
        store
            .edit_page(
                "sandbox",
                &[
                    Edit {
                        old: "Body.".into(),
                        new: "Body one.".into(),
                    },
                    Edit {
                        old: "one".into(),
                        new: "two".into(),
                    },
                ],
            )
            .unwrap();
        assert!(store.read_page("sandbox").unwrap().contains("Body two."));

        let before = store.read_page("sandbox").unwrap();
        let error = store
            .edit_page(
                "sandbox",
                &[
                    Edit {
                        old: "Body".into(),
                        new: "Text".into(),
                    },
                    Edit {
                        old: "missing".into(),
                        new: "x".into(),
                    },
                ],
            )
            .unwrap_err();
        assert!(error.to_string().contains("edit 2"), "{error}");
        assert_eq!(store.read_page("sandbox").unwrap(), before);

        let error = store
            .edit_page(
                "sandbox",
                &[Edit {
                    old: urn(id),
                    new: urn(Uuid::new_v4()),
                }],
            )
            .unwrap_err();
        assert!(error.to_string().contains("id"), "{error}");
    }

    #[test]
    fn an_ambiguous_edit_is_refused() {
        let (_dir, store) = store();
        store
            .write_page("p", "---\ntitle: P\n---\nzq zq\n")
            .unwrap();
        let error = store
            .edit_page(
                "p",
                &[Edit {
                    old: "zq".into(),
                    new: "x".into(),
                }],
            )
            .unwrap_err();
        assert!(error.to_string().contains("2 times"), "{error}");
    }

    #[test]
    fn a_page_without_an_id_gets_one_on_demand_and_listing_skips_the_concept_file() {
        let (dir, store) = store();
        std::fs::write(dir.path().join("old.md"), "---\ntitle: Old\n---\nbody\n").unwrap();
        std::fs::write(dir.path().join("concepts.md"), "# Concepts\n").unwrap();
        let listed = store.list_pages().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, None);
        assert_eq!(listed[0].title.as_deref(), Some("Old"));

        let id = store.ensure_page_id("old").unwrap();
        assert_eq!(store.ensure_page_id("old").unwrap(), id);
        assert_eq!(store.list_pages().unwrap()[0].id, Some(id));
    }
}
