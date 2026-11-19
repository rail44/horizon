//! The extensions the shell comes with and the ones the configuration
//! names, kept to what their components are: read again when the configuration is, and one whose component is
//! built again is read again by itself. The panes that show an extension
//! read again are made again, on the state they keep.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_ui::reactive_graph::traits::WithUntracked;

use super::{Shell, plugins};
use crate::extension::{Extension, load_all};
use crate::watch::{self, Changed};

/// An extension whose component is watched.
pub(super) struct Watched {
    /// The component's file.
    file: PathBuf,
    changed: Changed,
    _watcher: watch::Watched,
}

impl Shell {
    /// Reads the components at `files` and the shell's own again, off the
    /// UI thread, and takes them in place of the extensions loaded.
    pub(super) fn reload_extensions(&self, files: Vec<String>) {
        let own = self.kept().own_extensions.clone();
        self.in_background(
            move || load_all(&files, &own),
            |shell, loaded| {
                let mut names: Vec<&str> = (plugins::extensions().iter())
                    .map(|extension| extension.name)
                    .collect();
                names.extend(loaded.iter().map(|extension| extension.name));
                plugins::take_in(loaded);
                shell.kept().extensions_read.set(true);
                // (Every command there can be is known from here.)
                shell.say_unknown_bindings();
                shell.watch_extensions();
                shell.remake_panes_of(&names);
            },
        );
    }

    /// Watches the component of each extension loaded.
    pub(super) fn watch_extensions(&self) {
        let kept = self.kept();
        let watched = (plugins::extensions().iter())
            .filter_map(|extension| {
                let changed = Changed::default();
                let watcher =
                    watch::on(extension.file.clone(), changed.clone(), kept.wake.clone())?;
                Some(Watched {
                    file: extension.file.clone(),
                    changed,
                    _watcher: watcher,
                })
            })
            .collect();
        *kept.watched.borrow_mut() = watched;
    }

    /// Reads again the extensions whose components were built again since
    /// this was last asked. One that does not load leaves the extension
    /// as it was.
    pub(super) fn follow_extensions(&self) {
        let built: Vec<PathBuf> = (self.kept().watched.borrow().iter())
            .filter(|watched| watched.changed.settled())
            .map(|watched| watched.file.clone())
            .collect();
        for file in built {
            self.in_background(
                move || Extension::load(&file).map_err(|error| (file, error)),
                |shell, loaded| match loaded {
                    Ok(extension) => {
                        let name = extension.name;
                        plugins::put(Arc::new(extension));
                        shell.remake_panes_of(&[name]);
                    }
                    Err((directory, error)) => {
                        eprintln!("horizon extension {}: {error}", directory.display());
                    }
                },
            );
        }
    }

    /// Makes again the panes that show a plugin among `names`.
    fn remake_panes_of(&self, names: &[&str]) {
        let panes: Vec<_> = self.model.with_untracked(|model| {
            (model.all_pane_ids().into_iter())
                .filter(|pane| {
                    (model.pane_plugin(*pane))
                        .is_some_and(|plugin| names.contains(&plugin.name.as_str()))
                })
                .collect()
        });
        for pane in panes {
            self.remake_pane(pane);
        }
    }
}
