//! The Markdown extension, built for `wasm32-wasip2` and run in a pane's
//! view: the same behavior the built-in Markdown view's tests hold it to.

// Not `gpui::*`: its `test` shadows the built-in attribute `#[gpui::test]`
// expands to.
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use gpui::{AnyWindowHandle, Context, Entity, IntoElement, Render, TestAppContext, Window};
use serde_json::json;

use super::ExtensionView;
use crate::markdown::DocumentChanged;

/// Builds `extensions/markdown` and returns its component.
fn extension() -> PathBuf {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/markdown");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .current_dir(&directory)
        .status()
        .expect("cargo");
    assert!(status.success(), "building the Markdown extension");
    directory.join("target/wasm32-wasip2/release/horizon_markdown_extension.wasm")
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("horizon-extension-{}", uuid::Uuid::new_v4()));
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

/// A window's root, drawing the extension view made inside it.
struct Holder(Entity<ExtensionView>);

impl Render for Holder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.0.clone()
    }
}

struct Opened {
    view: Entity<ExtensionView>,
    window: AnyWindowHandle,
    /// Every document the view reported moving to, after the first.
    moved: Rc<RefCell<Vec<PathBuf>>>,
}

fn open(cx: &mut TestAppContext, path: &Path) -> Opened {
    cx.update(gpui_component::init);
    let wasm = extension();
    let path = path.to_path_buf();
    let moved = Rc::new(RefCell::new(Vec::new()));
    let log = moved.clone();
    let window = cx.add_window(|window, cx| {
        let view = ExtensionView::build(&wasm, Some(path), false, window, cx).unwrap();
        cx.subscribe(&view, move |_, _, event: &DocumentChanged, _| {
            log.borrow_mut().push(event.0.clone());
        })
        .detach();
        Holder(view)
    });
    let view = window.read_with(cx, |holder, _| holder.0.clone()).unwrap();
    cx.run_until_parked();
    Opened {
        view,
        window: window.into(),
        moved,
    }
}

impl Opened {
    /// What the plugin has described: texts, and components as
    /// `<name props>`.
    fn drawn(&self, cx: &mut TestAppContext) -> Vec<String> {
        let plugin = self.view.read_with(cx, |view, _| view.plugin.clone());
        plugin.read_with(cx, |plugin, _| {
            (plugin.outline().into_iter())
                .map(|(_, line)| line)
                .collect()
        })
    }

    fn has(&self, part: &str, cx: &mut TestAppContext) -> bool {
        self.drawn(cx).iter().any(|line| line.contains(part))
    }

    /// Clicks the link to `target` in the drawn Markdown, as the Markdown
    /// component reports it.
    fn follow(&self, target: &str, cx: &mut TestAppContext) {
        let plugin = self.view.read_with(cx, |view, _| view.plugin.clone());
        plugin.update(cx, |plugin, cx| {
            let (node, _) = (plugin.outline().into_iter())
                .find(|(_, line)| line.starts_with("<markdown "))
                .expect("the Markdown body");
            plugin.dispatch(node, "link", &json!(target), cx);
        });
        cx.run_until_parked();
    }
}

#[gpui::test]
fn a_link_opens_in_place_and_back_returns(cx: &mut TestAppContext) {
    let scratch = Scratch::new();
    let index = scratch.write(
        "index.md",
        "---\ntitle: Index\nwritten: 2026-11-08\n---\n\nSee [a](a.md).\n",
    );
    let page = scratch.write("a.md", "---\ntitle: Page A\n---\nbody\n");
    let opened = open(cx, &index);
    assert_eq!(
        opened.drawn(cx)[..2],
        ["Index", r#"<chip {"label":"written 2026-11-08"}>"#]
    );
    assert!(opened.has("See [a](a.md).", cx));
    assert!(!opened.has("<button", cx));

    opened.follow("a.md", cx);
    assert_eq!(
        opened.drawn(cx)[..2],
        [r#"<button {"label":"Back"}>"#, "Page A"]
    );
    assert_eq!(*opened.moved.borrow(), std::slice::from_ref(&page));

    // The back key, through the plugin's own key handling.
    cx.simulate_keystrokes(opened.window, "backspace");
    cx.run_until_parked();
    assert_eq!(opened.drawn(cx)[0], "Index");
    assert!(!opened.has("<button", cx));
    assert_eq!(*opened.moved.borrow(), [page, index]);
}

#[gpui::test]
fn a_link_that_is_not_markdown_stays_put_with_a_notice(cx: &mut TestAppContext) {
    let scratch = Scratch::new();
    let index = scratch.write("index.md", "# Index\n");
    let opened = open(cx, &index);
    opened.follow("src/lib.rs", cx);
    assert!(opened.has("Not a Markdown file: src/lib.rs", cx));
    assert!(opened.has("# Index", cx));
    assert!(opened.moved.borrow().is_empty());
}

#[gpui::test]
fn a_missing_file_is_a_pane_state_and_a_rewrite_is_picked_up(cx: &mut TestAppContext) {
    let scratch = Scratch::new();
    let path = scratch.0.join("later.md");
    let opened = open(cx, &path);
    assert!(opened.has("later.md: ", cx));
    assert!(!opened.has("<markdown", cx));

    scratch.write("later.md", "---\ntitle: Now\n---\n");
    // What the CLI does to a pane that already shows the file.
    (opened.view).update(cx, |view, cx| view.open(path.clone(), cx));
    cx.run_until_parked();
    assert_eq!(opened.drawn(cx)[0], "Now");
}
