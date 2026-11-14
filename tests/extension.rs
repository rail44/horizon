//! An extension shown in a pane of the shell: the Markdown view built as a
//! wasm component (`plugins/markdown`), loaded from its package, its state
//! kept in the model as it changes and its command carried out from the
//! palette.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use horizon::workspace::{Opening, Placement, Shell, Spawn, css, view};
use horizon_terminal_core::TerminalSize;
use horizon_ui::App;
use horizon_ui::blitz_dom::{BaseDocument, NodeId, local_name};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_ui::testing::click_at;
use horizon_workspace::{PluginPane, Workspace};

mod support;

/// The extension's component, as `scripts/check-extension.sh` builds it.
fn component() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip2/debug/horizon_markdown_plugin.wasm")
}

/// A package of the extension in `directory`: its manifest and its
/// component.
fn package(directory: &Path) -> PathBuf {
    let package = directory.join("package");
    std::fs::create_dir_all(&package).unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/markdown/plugin.toml");
    std::fs::copy(manifest, package.join("plugin.toml")).unwrap();
    std::fs::copy(component(), package.join("markdown.wasm")).unwrap();
    package
}

struct Opened {
    app: App,
    shell: Shell,
}

fn collect(document: &BaseDocument, id: NodeId, class: &str, found: &mut Vec<NodeId>) {
    let node = document.get_node(id).expect("a node");
    let named = (node.attr(local_name!("class")))
        .is_some_and(|classes| horizon_ui::testing::has_class(classes, class));
    if named {
        found.push(id);
    }
    for child in &node.children {
        collect(document, *child, class, found);
    }
}

impl Opened {
    fn model<R>(&self, read: impl FnOnce(&Workspace) -> R) -> R {
        self.shell.model.with_untracked(read)
    }

    fn all(&self, class: &str) -> Vec<NodeId> {
        let document = self.app.document();
        let document = document.borrow();
        let mut found = Vec::new();
        collect(&document, document.root_element().id, class, &mut found);
        found
    }

    fn middle(&self, node: NodeId) -> (f32, f32) {
        let document = self.app.document();
        let document = document.borrow();
        let bounds = document.get_client_bounding_rect(node).expect("a box");
        (
            (bounds.x + bounds.width / 2.) as f32,
            (bounds.y + bounds.height / 2.) as f32,
        )
    }

    fn until(&self, what: &str, met: impl Fn() -> bool) {
        let started = Instant::now();
        while !met() {
            assert!(started.elapsed() < Duration::from_secs(20), "no {what}");
            std::thread::sleep(Duration::from_millis(5));
            self.app.run_effects();
            self.app.resolve();
        }
    }

    fn text(&self) -> String {
        horizon_ui::testing::text(&self.app)
    }
}

#[test]
#[ignore = "needs the extension built for wasm32-wasip2: scripts/check-extension.sh"]
fn an_extension_is_shown_in_a_pane_that_keeps_its_state_and_carries_out_its_command() {
    let directory = std::env::temp_dir().join(format!("hzb-ext-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let (first, second) = (directory.join("first.md"), directory.join("second.md"));
    std::fs::write(&first, "# One\n\n[to the second](second.md)\n").unwrap();
    std::fs::write(&second, "# Two\n").unwrap();
    let package = package(&directory);

    let config = horizon_config::RawConfig {
        extensions: vec![package.to_string_lossy().into_owned()],
        ..horizon_config::RawConfig::default()
    };
    let mut shell = None;
    let app = App::new(1200, 800, css(), || {
        let spawn: Spawn = Box::new(|_| support::shell("sh", TerminalSize::new(80, 24)));
        let (root, built) = view(
            spawn,
            Opening {
                config,
                ..Opening::default()
            },
        );
        shell = Some(built);
        root
    });
    app.resolve();
    let opened = Opened {
        app,
        shell: shell.unwrap(),
    };

    // A pane of the extension on a file: the state is the extension's
    // own type, as text.
    let state = format!("{{path: some({:?})}}", first.to_string_lossy());
    opened.shell.create_plugin(
        PluginPane {
            name: "markdown-extension".to_string(),
            title: "Markdown".to_string(),
            state: serde_json::Value::String(state),
        },
        Placement::NewTab,
    );
    opened.until("the file", || opened.text().contains("One"));
    let pane = opened
        .model(|model| model.cursor_pane_id())
        .expect("the new pane");
    let kept = |opened: &Opened| {
        opened.model(|model| {
            let plugin = model.pane_plugin(pane).expect("the pane's plugin");
            (
                plugin.title.clone(),
                plugin.state.as_str().unwrap_or_default().to_string(),
            )
        })
    };
    // The extension titled the pane for its file.
    opened.until("the title", || kept(&opened).0 == "first.md");

    // A link followed: the state the pane keeps follows the extension's.
    let (x, y) = opened.middle(opened.all("link")[0]);
    click_at(&opened.app, x, y);
    opened.until("the second file", || opened.text().contains("Two"));
    opened.until("the state kept", || kept(&opened).1.contains("second.md"));
    assert_eq!(kept(&opened).0, "second.md");

    // The extension's command, by the name its manifest gives.
    assert!(opened.shell.execute_plugin("back"));
    opened.until("the first file again", || opened.text().contains("One"));
    opened.until("the state kept again", || {
        kept(&opened).1.contains("first.md")
    });

    // What is saved of the pane: the extension's name and its state.
    let saved = opened.model(|model| model.to_persisted_json().unwrap());
    assert!(saved.contains("markdown-extension"), "{saved}");
    assert!(saved.contains("first.md"), "{saved}");
    let _ = std::fs::remove_dir_all(&directory);
}
