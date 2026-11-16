//! The file a new pane of an extension is made on: found by typing among
//! the files under the directory the cursor's pane works in (the one the
//! shell was started in, when its pane has no session or there is no
//! pane), and handed to the extension, whose answer is the pane's state.

use std::path::{Path, PathBuf};

use horizon_ui::reactive_graph::traits::{Set, WithUntracked};
use horizon_ui::{Callback, Component, El, el};
use horizon_workspace::PluginPane;

use super::{Placement, Shell, plugins, styles};
use crate::finder::{Finder, Source};

impl Shell {
    /// Where a pane asked for here is asked for: the directory the session
    /// under the cursor works in, or else the shell's own. The files to
    /// find are listed from it.
    pub(super) fn working_directory(&self) -> PathBuf {
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
    let Some(extensions) = extension.file_asked_for().map(<[String]>::to_vec) else {
        return scrim;
    };
    let root = shell.working_directory();
    let finder = Finder {
        source: Source::under(root.clone(), extensions, shell.kept().wake.clone()),
        picked: Callback::new(move |path: String| {
            shell.modal.set(None);
            match extension.created_on(Some(root.join(path)), &root) {
                Ok(state) => shell.create_plugin(
                    PluginPane {
                        name: extension.name.to_string(),
                        title: extension.title.clone(),
                        state: state.map_or(serde_json::Value::Null, serde_json::Value::String),
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
