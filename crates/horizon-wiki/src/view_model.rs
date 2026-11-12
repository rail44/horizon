//! What the wiki view draws, read without gpui: one page of the project
//! wiki, its comment threads, and where each thread sits in the page now.
//!
//! Positions cross three spaces. A thread stores code points of the page's
//! logical text (`crate::logical`); the view's `TextView` reports a
//! selection, and takes highlights, through byte ranges of the Markdown
//! body it was given. The logical text's source map converts between the
//! first two, and `RenderedText::range_for_source` takes a body range on to
//! the rendered string.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::anchor::{self, Anchor};
use crate::logical::LogicalText;
use crate::{Store, Thread};
use uuid::Uuid;

use horizon_workspace::markdown::{self as document, Document};

/// One thread and where it is in the page as it reads now.
#[derive(Debug, Clone)]
pub struct PlacedThread {
    pub thread: Thread,
    pub anchor: Anchor,
    /// The anchor's bytes in the page body, when it is still in the page.
    pub source: Option<Range<usize>>,
    /// The top-level block the anchor starts in, which the thread is drawn
    /// after; `None` for an orphan.
    pub block: Option<usize>,
}

impl PlacedThread {
    /// The text the thread's root quoted.
    pub fn quote(&self) -> String {
        self.thread
            .root
            .quote()
            .map(|quote| quote.exact)
            .unwrap_or_default()
    }
}

/// A page as loaded for the view.
#[derive(Debug)]
pub struct WikiPage {
    /// The page's id; `None` until it has one (a page nobody commented on
    /// may lack it).
    pub id: Option<Uuid>,
    pub document: Document,
    logical: LogicalText,
    /// The body's blocks (top-level blocks, and each item of a top-level
    /// list), as byte ranges; the view draws each on its own and places a
    /// block's threads against it.
    pub blocks: Vec<Range<usize>>,
    /// Per block, the top-level list it is an item of, if any.
    pub lists: Vec<Option<usize>>,
    /// Anchored threads first, in page order, then orphans in the order
    /// they were opened.
    pub threads: Vec<PlacedThread>,
    /// The comments a session is answering now.
    pub replying: BTreeSet<String>,
}

impl WikiPage {
    pub fn load(store: &Store, name: &str) -> Result<Self, String> {
        let text = store.read_page(name).map_err(|error| error.to_string())?;
        let id = crate::page::page_id(&text);
        let document = document::parse(&text);
        let logical = LogicalText::of_markdown(&document.body);
        let threads = match id {
            Some(id) => store.threads(id).map_err(|error| error.to_string())?,
            None => Vec::new(),
        };
        let (blocks, lists): (Vec<_>, Vec<_>) = crate::logical::page_blocks(&document.body)
            .into_iter()
            .map(|block| (block.range, block.list))
            .unzip();
        let mut threads: Vec<PlacedThread> = threads
            .into_iter()
            .map(|thread| place(&logical, &blocks, thread))
            .collect();
        // Stable: orphans keep their log order after every anchored one.
        threads.sort_by_key(|placed| {
            placed
                .source
                .as_ref()
                .map_or(usize::MAX, |range| range.start)
        });
        Ok(Self {
            id,
            document,
            logical,
            blocks,
            lists,
            threads,
            replying: store.replying(),
        })
    }

    /// The logical characters a selection of body bytes `source` covers;
    /// `None` when it covers no text (only markup or separators).
    pub fn selection(&self, source: Range<usize>) -> Option<Range<usize>> {
        self.logical
            .logical_range(source)
            .filter(|chars| !chars.is_empty())
    }

    /// The logical characters whose source is within body bytes `source`:
    /// a view that draws the body's text itself places each run of it in
    /// the logical text by this.
    pub fn logical_range(&self, source: Range<usize>) -> Option<Range<usize>> {
        self.logical.logical_range(source)
    }

    /// The top-level block logical characters `chars` start in, where a new
    /// comment on them is drawn.
    pub fn block_of(&self, chars: Range<usize>) -> Option<usize> {
        let source = self.logical.source_range(chars)?;
        self.blocks
            .iter()
            .rposition(|block| block.start <= source.start)
    }

    /// The logical text of `chars`, for showing what a new comment quotes.
    pub fn quote_of(&self, chars: Range<usize>) -> String {
        self.logical.slice(chars)
    }

    pub fn orphans(&self) -> impl Iterator<Item = (usize, &PlacedThread)> {
        self.threads
            .iter()
            .enumerate()
            .filter(|(_, placed)| placed.source.is_none())
    }
}

impl PlacedThread {
    /// Whether a session is writing an answer in this thread now.
    pub fn replying(&self, replying: &BTreeSet<String>) -> bool {
        std::iter::once(&self.thread.root)
            .chain(&self.thread.replies)
            .any(|annotation| replying.contains(&annotation.id))
    }
}

fn place(logical: &LogicalText, blocks: &[Range<usize>], thread: Thread) -> PlacedThread {
    let anchor = thread
        .root
        .quote()
        .map_or(Anchor::Orphaned, |quote| anchor::anchor(logical, &quote));
    let source = anchor.range().and_then(|chars| logical.source_range(chars));
    // The block holding the anchor's start, or failing that (the start
    // sits in the gap between blocks) the last block before it.
    let block = source
        .as_ref()
        .and_then(|source| blocks.iter().rposition(|block| block.start <= source.start));
    PlacedThread {
        thread,
        anchor,
        source,
        block,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{markdown_body, Agent};

    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store() -> (Scratch, Store) {
        let dir = Scratch(std::env::temp_dir().join(format!("horizon-wiki-{}", Uuid::new_v4())));
        let store = Store::at(&dir.0);
        store
            .write_page(
                "sandbox",
                "---\ntitle: Sandbox\n---\nThe **helper** runs `nono` first.\n\nThen the agent.\n",
            )
            .unwrap();
        (dir, store)
    }

    #[test]
    fn a_selection_round_trips_through_logical_positions() {
        let (_dir, store) = store();
        let page = WikiPage::load(&store, "sandbox").unwrap();
        let body = &page.document.body;
        // A selection that spans the `**` delimiters covers "helper".
        let start = body.find("**helper").unwrap();
        let chars = page.selection(start..start + "**helper**".len()).unwrap();
        assert_eq!(page.quote_of(chars.clone()), "helper");

        store
            .ask(
                "sandbox",
                chars,
                Agent::person(Uuid::new_v4(), "owner"),
                markdown_body("Which helper?"),
            )
            .unwrap();
        let page = WikiPage::load(&store, "sandbox").unwrap();
        let placed = &page.threads[0];
        let source = placed.source.clone().unwrap();
        assert_eq!(&page.document.body[source.clone()], "helper");
        assert_eq!(placed.block, Some(0));
        assert_eq!(page.blocks.len(), 2);
        assert!(!placed.replying(&page.replying));

        // The answering side lists the root as being answered.
        store
            .write_replying(&[placed.thread.root.id.clone()].into())
            .unwrap();
        let page = WikiPage::load(&store, "sandbox").unwrap();
        assert!(page.threads[0].replying(&page.replying));
        assert_eq!(page.orphans().count(), 0);
    }

    #[test]
    fn a_comment_on_a_list_item_belongs_to_that_item() {
        let (_dir, store) = store();
        store
            .write_page(
                "list",
                "---\ntitle: L\n---\nIntro.\n\n- first item\n- second item\n",
            )
            .unwrap();
        let page = WikiPage::load(&store, "list").unwrap();
        assert_eq!(page.blocks.len(), 3);
        assert_eq!(page.lists, vec![None, Some(0), Some(0)]);
        let start = "Intro.\nfirst item\n".chars().count();
        assert_eq!(page.quote_of(start..start + 6), "second");
        assert_eq!(page.block_of(start..start + 6), Some(2));
        store
            .ask(
                "list",
                start..start + 6,
                Agent::person(Uuid::new_v4(), "owner"),
                markdown_body("Why second?"),
            )
            .unwrap();
        let page = WikiPage::load(&store, "list").unwrap();
        assert_eq!(page.threads[0].block, Some(2));
    }

    #[test]
    fn a_removed_passage_leaves_its_thread_an_orphan_after_the_anchored_ones() {
        let (_dir, store) = store();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        let agent = "The helper runs nono first.\nThen the ".chars().count();
        store
            .ask(
                "sandbox",
                agent..agent + 5,
                owner.clone(),
                markdown_body("first"),
            )
            .unwrap();
        store
            .ask("sandbox", 4..10, owner, markdown_body("second"))
            .unwrap();
        let text = store.read_page("sandbox").unwrap();
        store
            .write_page("sandbox", &text.replace("\nThen the agent.\n", "\n"))
            .unwrap();
        let page = WikiPage::load(&store, "sandbox").unwrap();
        assert_eq!(page.threads.len(), 2);
        assert_eq!(page.threads[0].thread.root.body.value, "second");
        assert!(page.threads[0].source.is_some());
        let orphans: Vec<_> = page.orphans().collect();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].1.thread.root.body.value, "first");
    }
}
