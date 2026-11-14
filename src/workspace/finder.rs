//! The file a new pane of an extension is made on: found by typing among
//! the files under the directory the cursor's pane works in (the one the
//! shell was started in, when its pane has no session or there is no
//! pane), and handed to the extension, whose answer is the pane's state.

use std::path::{Path, PathBuf};

use horizon_ui::reactive_graph::traits::{Set, WithUntracked};
use horizon_ui::{Callback, Component, El, el};
use horizon_workspace::PluginPane;

use super::{Placement, Shell, plugins, styles};
use crate::background::Background;
use crate::extension::Creates;
use crate::finder::{Finder, list};

impl Shell {
    /// Where the files to find are listed from.
    fn finder_root(&self) -> PathBuf {
        let working = self.model.with_untracked(|model| {
            let session = model.pane_session_id(model.cursor_pane_id()?)?;
            model.session_directory(session).map(Path::to_path_buf)
        });
        working
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }
}

pub(super) fn view(shell: Shell, plugin: &'static str, placement: Placement) -> El {
    let close = move || {
        shell.modal.set(None);
        shell.focus_active();
    };
    let scrim = el("div")
        .class(styles::SCRIM)
        .on("pointerdown", move |_| close());
    let Some(extension) = plugins::extension(plugin) else {
        return scrim;
    };
    let Some(Creates::File { extensions }) = extension.creates.clone() else {
        return scrim;
    };
    let root = shell.finder_root();
    let background = Background::new(shell.kept().wake.clone());
    let listed = list(root.clone(), extensions, background.clone());
    horizon_ui::each_effects(move || {
        background.take_in();
    });
    let finder = Finder {
        files: listed.files.into(),
        listing: listed.listing.into(),
        picked: Callback::new(move |path: String| {
            shell.modal.set(None);
            match extension.create(&root.join(path)) {
                Ok(state) => shell.create_plugin(
                    PluginPane {
                        name: extension.name.to_string(),
                        title: extension.title.clone(),
                        state: serde_json::Value::String(state),
                    },
                    placement,
                ),
                Err(error) => {
                    eprintln!("horizon: {}: {error}", extension.name);
                    shell.focus_active();
                }
            }
        }),
        left: Callback::new(move |()| close()),
    };
    scrim.child(finder.view())
}
