//! The Markdown view on this stack, held to what Horizon's own Markdown
//! view is held to (`src/markdown/view.rs`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use reactive_graph::traits::GetUntracked;
use ui_foundation_spike::blitz_dom::{BaseDocument, NodeId, local_name};
use ui_foundation_spike::blitz_traits::events::{BlitzKeyEvent, KeyState, UiEvent};
use ui_foundation_spike::markdown::{CSS, Markdown, view};
use ui_foundation_spike::theme::{self, Theme};
use ui_foundation_spike::{App, click_at};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ui-foundation-markdown-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Opened {
    app: App,
    state: Markdown,
    /// The URLs the view handed to the desktop.
    opened: Rc<RefCell<Vec<String>>>,
}

fn open(path: &Path) -> Opened {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let log = opened.clone();
    let mut state = None;
    let css = [theme::CSS, CSS].concat();
    let app = App::new(800, 600, &css, || {
        let (root, markdown) = view(
            Some(path.to_path_buf()),
            Rc::new(move |url: &str| log.borrow_mut().push(url.to_owned())),
        );
        state = Some(markdown);
        theme::themed(Theme::built_in, root.focus())
    });
    app.resolve();
    Opened {
        app,
        state: state.unwrap(),
        opened,
    }
}

impl Opened {
    fn text(&self) -> String {
        let document = self.app.document();
        let document = document.borrow();
        document.root_element().text_content()
    }

    /// Clicks the first element of `class`, at its middle.
    fn click(&self, class: &str) {
        fn find(document: &BaseDocument, id: NodeId, class: &str) -> Option<NodeId> {
            let node = document.get_node(id)?;
            if node.attr(local_name!("class")) == Some(class) {
                return Some(id);
            }
            node.children
                .iter()
                .find_map(|child| find(document, *child, class))
        }
        let (x, y) = {
            let document = self.app.document();
            let document = document.borrow();
            let node = find(&document, document.root_element().id, class).expect("the element");
            let bounds = document.get_client_bounding_rect(node).expect("a box");
            (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.)
        };
        click_at(&self.app, x as f32, y as f32);
        self.app.resolve();
    }

    fn press(&self, key: keyboard_types::Key, code: keyboard_types::Code) {
        self.app.dispatch(UiEvent::KeyDown(BlitzKeyEvent {
            key,
            code,
            modifiers: Default::default(),
            location: keyboard_types::Location::Standard,
            is_auto_repeating: false,
            is_composing: false,
            state: KeyState::Pressed,
            text: None,
        }));
        self.app.resolve();
    }
}

#[test]
fn a_link_opens_in_place_and_back_returns() {
    let scratch = Scratch::new("link");
    let index = scratch.write(
        "index.md",
        "---\ntitle: Index\nwritten: 2026-11-08\n---\n\nSee [a](a.md).\n",
    );
    let page = scratch.write("a.md", "---\ntitle: Page A\n---\nbody\n");
    let opened = open(&index);
    assert_eq!(opened.text(), "Indexwritten 2026-11-08See a.");

    // A real click on the link in the text.
    opened.click("link");
    assert_eq!(opened.state.path.get_untracked(), Some(page.clone()));
    assert_eq!(opened.state.history.get_untracked(), vec![index.clone()]);
    assert_eq!(opened.text(), "BackPage Abody");

    // The back key, through the view's own key handling.
    opened.press(
        keyboard_types::Key::Named(keyboard_types::NamedKey::Backspace),
        keyboard_types::Code::Backspace,
    );
    assert_eq!(opened.state.path.get_untracked(), Some(index));
    assert!(opened.state.history.get_untracked().is_empty());
    assert_eq!(opened.text(), "Indexwritten 2026-11-08See a.");
}

#[test]
fn a_link_that_is_not_markdown_stays_put_with_a_notice() {
    let scratch = Scratch::new("notice");
    let index = scratch.write(
        "index.md",
        "# Index\n\n[code](src/lib.rs) and [the web](https://example.com/x)\n",
    );
    let opened = open(&index);
    opened.click("link");
    assert_eq!(opened.state.path.get_untracked(), Some(index));
    assert!(opened.state.history.get_untracked().is_empty());
    assert!(
        opened.text().contains("Not a Markdown file: src/lib.rs"),
        "{}",
        opened.text()
    );
    assert!(opened.opened.borrow().is_empty());
}

#[test]
fn a_missing_file_is_a_pane_state_and_a_rewrite_is_picked_up() {
    let scratch = Scratch::new("later");
    let path = scratch.0.join("later.md");
    let opened = open(&path);
    assert!(opened.text().contains("later.md: "), "{}", opened.text());

    scratch.write("later.md", "---\ntitle: Now\n---\n");
    // What the CLI does to a pane that already shows the file.
    opened.state.open(path);
    opened.app.run_effects();
    opened.app.resolve();
    assert_eq!(opened.text(), "Now");
}
