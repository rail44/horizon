//! Comment threads: one append-only JSONL log per page,
//! `threads/<page uuid>.jsonl`, one [`Annotation`] per line, appended under
//! an exclusive `flock(2)` by whichever process writes (the shell's view, an
//! agent's tool) and read under a shared one. A thread is a range-targeted
//! annotation (its root) and every reply that leads back to it.
//!
//! Snapshots (`snapshots/<uuid>.md`) keep the page as it was when a comment
//! was made; an annotation's `TimeState.cached` names one.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::annotation::{Agent, Annotation, Motivation, Quote, Target, TextualBody};
use crate::logical::LogicalText;
use crate::page::{body, urn};
use crate::{Result, Store, WikiError};

/// A root annotation and its replies, in log order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub root: Annotation,
    pub replies: Vec<Annotation>,
}

impl Thread {
    /// The last annotation in the thread.
    pub fn last(&self) -> &Annotation {
        self.replies.last().unwrap_or(&self.root)
    }
}

impl Store {
    pub fn threads_dir(&self) -> PathBuf {
        self.dir().join("threads")
    }

    pub fn snapshots_dir(&self) -> PathBuf {
        self.dir().join("snapshots")
    }

    pub fn thread_log_path(&self, page: Uuid) -> PathBuf {
        self.threads_dir().join(format!("{page}.jsonl"))
    }

    pub fn snapshot_path(&self, snapshot: Uuid) -> PathBuf {
        self.snapshots_dir().join(format!("{snapshot}.md"))
    }

    /// Every annotation on page `page`, in log order. A line that does not
    /// parse (a torn write, a hand edit) is skipped.
    pub fn annotations(&self, page: Uuid) -> Result<Vec<Annotation>> {
        let file = match File::open(self.thread_log_path(page)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        lock(&file, libc::LOCK_SH)?;
        let mut raw = Vec::new();
        (&file).read_to_end(&mut raw)?;
        Ok(parse_log(&raw))
    }

    /// The threads on page `page`, in the order their roots were made.
    pub fn threads(&self, page: Uuid) -> Result<Vec<Thread>> {
        Ok(assemble(self.annotations(page)?))
    }

    /// Appends `annotation` to page `page`'s log. A reply's parent must
    /// already be in the log, and an id may appear only once.
    pub fn append_annotation(&self, page: Uuid, annotation: &Annotation) -> Result<()> {
        let line = serde_json::to_string(annotation)
            .map_err(|error| WikiError::Invalid(error.to_string()))?;
        std::fs::create_dir_all(self.threads_dir())?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(self.thread_log_path(page))?;
        lock(&file, libc::LOCK_EX)?;
        let mut raw = Vec::new();
        (&file).read_to_end(&mut raw)?;
        let existing = parse_log(&raw);
        if existing.iter().any(|known| known.id == annotation.id) {
            return Err(WikiError::Invalid(format!(
                "{} is already in the log",
                annotation.id
            )));
        }
        if let Target::Annotation(parent) = &annotation.target {
            if !existing.iter().any(|known| &known.id == parent) {
                return Err(WikiError::NotFound(format!("annotation {parent}")));
            }
        }
        // A torn last line must not swallow this one.
        let separator = if raw.last().is_some_and(|byte| *byte != b'\n') {
            "\n"
        } else {
            ""
        };
        file.write_all(format!("{separator}{line}\n").as_bytes())?;
        file.flush()?;
        Ok(())
    }

    /// Keeps `text` (a whole page) as a snapshot and returns its id.
    pub fn write_snapshot(&self, text: &str) -> Result<Uuid> {
        let id = Uuid::new_v4();
        crate::page::write_atomically(&self.snapshot_path(id), text)?;
        Ok(id)
    }

    pub fn read_snapshot(&self, snapshot: Uuid) -> Result<String> {
        std::fs::read_to_string(self.snapshot_path(snapshot)).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                WikiError::NotFound(format!("snapshot {}", urn(snapshot)))
            }
            _ => error.into(),
        })
    }

    /// Opens a thread on the logical characters `chars` of page `name`:
    /// gives the page an id if it has none, snapshots it, and appends the
    /// question. Returns the page's id and the new annotation.
    pub fn ask(
        &self,
        name: &str,
        chars: std::ops::Range<usize>,
        creator: Agent,
        question: TextualBody,
    ) -> Result<(Uuid, Annotation)> {
        let page = self.ensure_page_id(name)?;
        let text = self.read_page(name)?;
        let logical = LogicalText::of_markdown(body(&text));
        let quote = Quote::of(&logical, chars.clone()).ok_or_else(|| {
            WikiError::Invalid(format!(
                "{chars:?} is not a non-empty range of the page's {} characters",
                logical.len()
            ))
        })?;
        let snapshot = self.write_snapshot(&text)?;
        let annotation = Annotation::on_range(
            Motivation::Questioning,
            page,
            &quote,
            snapshot,
            creator,
            question,
        );
        self.append_annotation(page, &annotation)?;
        Ok((page, annotation))
    }

    /// Appends a reply to annotation `parent` on page `page`.
    pub fn reply(
        &self,
        page: Uuid,
        parent: &str,
        creator: Agent,
        text: TextualBody,
    ) -> Result<Annotation> {
        let annotation = Annotation::reply(parent, creator, text);
        self.append_annotation(page, &annotation)?;
        Ok(annotation)
    }

    /// The name of the page whose id is `page`.
    pub fn page_name(&self, page: Uuid) -> Result<String> {
        self.list_pages()?
            .into_iter()
            .find(|info| info.id == Some(page))
            .map(|info| info.name)
            .ok_or_else(|| WikiError::NotFound(format!("page {}", urn(page))))
    }

    /// Every page id that has a comment log.
    pub fn commented_pages(&self) -> Result<Vec<Uuid>> {
        let entries = match std::fs::read_dir(self.threads_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut pages = Vec::new();
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(id) = name
                .strip_suffix(".jsonl")
                .and_then(|stem| Uuid::parse_str(stem).ok())
            {
                pages.push(id);
            }
        }
        pages.sort();
        Ok(pages)
    }
}

/// Groups `annotations` (in log order) into threads: each range-targeted
/// annotation opens one, and a reply joins the thread its parent is in. A
/// reply whose parent is unknown is dropped.
pub fn assemble(annotations: Vec<Annotation>) -> Vec<Thread> {
    let mut threads: Vec<Thread> = Vec::new();
    let mut thread_of: std::collections::HashMap<String, usize> = Default::default();
    for annotation in annotations {
        match &annotation.target {
            Target::Range(_) => {
                thread_of.insert(annotation.id.clone(), threads.len());
                threads.push(Thread {
                    root: annotation,
                    replies: Vec::new(),
                });
            }
            Target::Annotation(parent) => {
                if let Some(&index) = thread_of.get(parent) {
                    thread_of.insert(annotation.id.clone(), index);
                    threads[index].replies.push(annotation);
                }
            }
        }
    }
    threads
}

/// The installation's owner: one id, kept at `<wiki root>/owner` (the
/// directory above every project's store), made on first use.
pub fn owner_id(wiki_root: &Path) -> Result<Uuid> {
    let path = wiki_root.join("owner");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Some(id) = parse_owner(&text) {
            return Ok(id);
        }
    }
    std::fs::create_dir_all(wiki_root)?;
    let id = Uuid::new_v4();
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(format!("{}\n", urn(id)).as_bytes())?;
            Ok(id)
        }
        // Another process made it first; theirs wins.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            parse_owner(&std::fs::read_to_string(&path)?).ok_or_else(|| {
                WikiError::Invalid(format!("{} does not hold an owner id", path.display()))
            })
        }
        Err(error) => Err(error.into()),
    }
}

fn parse_owner(text: &str) -> Option<Uuid> {
    let value = text.trim();
    Uuid::parse_str(value.strip_prefix("urn:uuid:").unwrap_or(value)).ok()
}

fn lock(file: &File, operation: libc::c_int) -> std::io::Result<()> {
    // SAFETY: flock on a descriptor this File owns; released on close.
    let ret = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn parse_log(raw: &[u8]) -> Vec<Annotation> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::markdown_body;

    fn store_with_page() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store
            .write_page(
                "sandbox",
                "---\ntitle: Sandbox\n---\n# Sandbox\n\nThe helper runs **nono** first.\n",
            )
            .unwrap();
        (dir, store)
    }

    #[test]
    fn a_question_quotes_the_logical_text_and_snapshots_the_page() {
        let (_dir, store) = store_with_page();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        let start = "Sandbox\nThe helper runs ".chars().count();
        let (page, question) = store
            .ask(
                "sandbox",
                start..start + 4,
                owner,
                markdown_body("Why first?"),
            )
            .unwrap();
        let quote = question.quote().unwrap();
        assert_eq!(quote.exact, "nono");
        assert_eq!(quote.prefix, "Sandbox\nThe helper runs ");
        assert_eq!(quote.suffix, " first.");
        let Target::Range(resource) = &question.target else {
            panic!("a question targets a range")
        };
        assert_eq!(resource.source, urn(page));
        let snapshot =
            Uuid::parse_str(resource.state.cached.strip_prefix("urn:uuid:").unwrap()).unwrap();
        assert_eq!(
            store.read_snapshot(snapshot).unwrap(),
            store.read_page("sandbox").unwrap()
        );
        assert_eq!(store.annotations(page).unwrap(), vec![question]);
    }

    #[test]
    fn replies_join_their_root_thread_in_log_order() {
        let (_dir, store) = store_with_page();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        let session = Agent::session(Uuid::new_v4());
        let (page, first) = store
            .ask("sandbox", 0..7, owner.clone(), markdown_body("one"))
            .unwrap();
        let (_, second) = store
            .ask("sandbox", 8..18, owner.clone(), markdown_body("two"))
            .unwrap();
        let answer = store
            .reply(page, &first.id, session, markdown_body("answer"))
            .unwrap();
        let follow_up = store
            .reply(page, &answer.id, owner, markdown_body("follow-up"))
            .unwrap();
        let threads = store.threads(page).unwrap();
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].root, first);
        assert_eq!(threads[0].replies, vec![answer, follow_up.clone()]);
        assert_eq!(threads[0].last(), &follow_up);
        assert_eq!(threads[1].root, second);
        assert_eq!(store.commented_pages().unwrap(), vec![page]);
        assert_eq!(store.page_name(page).unwrap(), "sandbox");
    }

    #[test]
    fn a_reply_needs_a_known_parent_and_ids_are_unique() {
        let (_dir, store) = store_with_page();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        let (page, question) = store
            .ask("sandbox", 0..7, owner.clone(), markdown_body("q"))
            .unwrap();
        assert!(matches!(
            store.reply(page, "urn:uuid:nope", owner, markdown_body("r")),
            Err(WikiError::NotFound(_))
        ));
        assert!(matches!(
            store.append_annotation(page, &question),
            Err(WikiError::Invalid(_))
        ));
    }

    #[test]
    fn a_torn_line_is_skipped() {
        let (_dir, store) = store_with_page();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        let (page, question) = store
            .ask("sandbox", 0..7, owner, markdown_body("q"))
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.thread_log_path(page))
            .unwrap();
        file.write_all(b"{\"id\":\"urn:uu").unwrap();
        assert_eq!(store.annotations(page).unwrap(), vec![question.clone()]);
        let reply = store
            .reply(
                page,
                &question.id,
                Agent::session(Uuid::new_v4()),
                markdown_body("r"),
            )
            .unwrap();
        assert_eq!(store.annotations(page).unwrap(), vec![question, reply]);
    }

    #[test]
    fn the_owner_id_is_made_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = owner_id(dir.path()).unwrap();
        assert_eq!(owner_id(dir.path()).unwrap(), first);
    }

    #[test]
    fn an_empty_selection_is_refused() {
        let (_dir, store) = store_with_page();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        assert!(store
            .ask("sandbox", 3..3, owner, markdown_body("q"))
            .is_err());
    }
}
