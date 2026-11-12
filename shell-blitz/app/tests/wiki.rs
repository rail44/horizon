//! The wiki view over a store on disk: a page drawn, a passage selected
//! and commented on, the thread it starts read and replied in, and what
//! the answering side writes arriving by the files.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use horizon_blitz::wake::Wake;
use horizon_blitz::wiki::{CSS, Target, Wiki, view};
use horizon_theme_css::{Theme, themed};
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::blitz_traits::events::{BlitzImeEvent, BlitzKeyEvent, KeyState, UiEvent};
use horizon_ui::reactive_graph::traits::GetUntracked;
use horizon_ui::testing::{click_at, select};
use horizon_ui::{App, El};
use horizon_wiki::Store;
use keyboard_types::{Key, Modifiers, NamedKey};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Opened {
    _scratch: Scratch,
    store: Store,
    app: App,
    wiki: Wiki,
}

const PAGE: &str = "---\ntitle: Sandbox\nstatus: draft\n---\n\
The **helper** runs `nono` first.\n\n\
Then the agent, see [the other page](other.md).\n\n\
- one item\n- another item\n";

fn open(name: &str) -> Opened {
    // `<wiki root>/<project>/`: the owner's id is kept beside the stores.
    let scratch = Scratch(
        std::env::temp_dir().join(format!("horizon-blitz-wiki-{name}-{}", std::process::id())),
    );
    let store = Store::at(scratch.0.join("project"));
    store.write_page("sandbox", PAGE).unwrap();
    store
        .write_page("other", "---\ntitle: Other\n---\nAnother page.\n")
        .unwrap();
    let path = store.page_path("sandbox").unwrap();
    let mut wiki = None;
    let css = [horizon_theme_css::CSS, CSS].concat();
    let app = App::new(700, 700, &css, || {
        let (root, state) = view(Some(path), Rc::new(|_| {}), Wake::default());
        wiki = Some(state);
        themed(Theme::built_in, root.focus())
    });
    let opened = Opened {
        _scratch: scratch,
        store,
        app,
        wiki: wiki.unwrap(),
    };
    // The page is read off the UI thread.
    opened.until("the page", || opened.text().contains("runs"));
    opened
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    let node = document.get_node(id).expect("a node");
    let named = (node.attr(local_name!("class")))
        .is_some_and(|classes| classes.split(' ').any(|name| name == class));
    if named {
        found.push(id);
    }
    for child in &node.children {
        collect(document, *child, class, found);
    }
}

impl Opened {
    /// Lays out (which takes in what was read, and a change of the files)
    /// until `met`.
    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        loop {
            self.app.resolve();
            if met() {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no {what} in:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn text(&self) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.root_element().text_content()
    }

    fn all(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        collect(&document, document.root_element().id, class, &mut found);
        found
    }

    fn text_of(&self, node: NodeId) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.get_node(node).unwrap().text_content()
    }

    /// The element a block's own text is in: its first child.
    fn text_root(&self, block: usize) -> El {
        let block = self.all("block")[block];
        let document = self.app.document();
        let document = document.borrow();
        El(document.get_node(block).unwrap().children[0])
    }

    fn click(&self, node: NodeId) {
        let (x, y) = {
            let document = self.app.document();
            let document = document.borrow();
            let bounds = document.get_client_bounding_rect(node).expect("a box");
            (
                (bounds.x + bounds.width / 2.) as f32,
                (bounds.y + bounds.height / 2.) as f32,
            )
        };
        click_at(&self.app, x, y);
        self.app.resolve();
    }

    fn key(&self, key: Key) {
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key,
            code: keyboard_types::Code::Unidentified,
            modifiers: Modifiers::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
        self.app.resolve();
    }

    /// Text as an input method commits it into the focused field.
    fn commit(&self, text: &str) {
        self.app
            .dispatch(UiEvent::Ime(BlitzImeEvent::Commit(text.to_string())));
        self.app.resolve();
    }
}

#[test]
fn a_page_is_its_header_and_its_blocks() {
    let opened = open("page");
    assert_eq!(opened.text_of(opened.all("heading")[0]), "Sandbox");
    // The front matter's other fields, as chips.
    assert_eq!(opened.text_of(opened.all("fields")[0]), "status draft");
    // A paragraph, a paragraph, and each item of the list on its own.
    let blocks: Vec<String> = (opened.all("block").into_iter())
        .map(|block| opened.text_of(block))
        .collect();
    assert_eq!(
        blocks,
        [
            "The helper runs nono first.",
            "Then the agent, see the other page.",
            "one item",
            "another item"
        ]
    );

    // A link to another page of the wiki is followed in place, and gone
    // back from.
    opened.click(opened.all("link")[0]);
    opened.until("the other page", || opened.text().contains("Another page."));
    assert_eq!(opened.wiki.history.get_untracked().len(), 1);
    opened.key(Key::Character("h".into()));
    opened.until("the first page", || opened.text().contains("runs"));
}

#[test]
fn a_selected_passage_is_commented_on_and_its_thread_replied_in() {
    let opened = open("thread");
    // "helper", selected in the first block's text.
    let root = opened.text_root(0);
    select(&opened.app, (root, 4), (root, 10));
    opened.until("the selection", || {
        opened.wiki.selection.get_untracked().is_some()
    });
    // The offer to comment is set after the block; `c` takes it.
    assert_eq!(opened.all("offer").len(), 1);
    opened.key(Key::Character("c".into()));
    let target = opened.wiki.target.get_untracked();
    assert!(matches!(target, Some(Target::Selection(_))), "{target:?}");
    let card = opened.all("new");
    assert_eq!(card.len(), 1);
    assert!(opened.text_of(card[0]).contains("“helper”"));

    // What is typed and sent starts a thread on the passage: marked in
    // the page, and open after its block.
    opened.commit("Which helper?");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the thread", || !opened.all("thread").is_empty());
    assert!(opened.all("new").is_empty());
    assert_eq!(opened.text_of(opened.all("mark")[0]), "helper");
    let thread = opened.all("thread")[0];
    assert!(opened.text_of(thread).contains("1 comment"));
    assert!(opened.text_of(thread).contains("Which helper?"));
    // The page still reads as it did around the mark.
    assert_eq!(
        opened.text_of(opened.all("block")[0]),
        "The helper runs nono first."
    );

    // The head shuts the thread and opens it; opened, it has the field,
    // and what is sent from it is a reply.
    opened.click(opened.all("head")[0]);
    assert!(opened.all("post").is_empty());
    opened.click(opened.all("head")[0]);
    assert_eq!(opened.all("composer").len(), 1);
    opened.commit("The supervised one.");
    opened.key(Key::Named(NamedKey::Enter));
    opened.until("the reply", || opened.all("post").len() == 2);
    assert!(
        opened
            .text_of(opened.all("thread")[0])
            .contains("2 comments")
    );

    // The answering side lists the comment it is answering beside the
    // log, which the view sees by the file.
    let id = opened
        .store
        .threads(horizon_wiki::page::page_id(&opened.store.read_page("sandbox").unwrap()).unwrap())
        .unwrap()[0]
        .root
        .id
        .clone();
    opened.store.write_replying(&[id].into()).unwrap();
    opened.until("the answer on its way", || {
        !opened.all("replying").is_empty()
    });
    // Escape leaves the field.
    opened.key(Key::Named(NamedKey::Escape));
    assert_eq!(opened.wiki.target.get_untracked(), None);
}
