//! The extensions the configuration names, kept to what their packages
//! are: read again when the configuration is, and one whose component is
//! built again is read again by itself. The panes that show an extension
//! read again are made again, on the state they keep.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_ui::reactive_graph::traits::WithUntracked;
use notify::RecommendedWatcher;

use super::{Shell, plugins};
use crate::extension::{Extension, load_all};
use crate::markdown::watch::{self, Changed};

/// An extension whose component is watched.
pub(super) struct Watched {
    /// The package's directory.
    directory: PathBuf,
    changed: Changed,
    _watcher: RecommendedWatcher,
}

impl Shell {
    /// Reads the packages in `directories` again, off the UI thread, and
    /// takes them in place of the extensions loaded.
    pub(super) fn reload_extensions(&self, directories: Vec<String>) {
        self.in_background(
            move || load_all(&directories),
            |shell, loaded| {
                let mut names: Vec<&str> = (plugins::extensions().iter())
                    .map(|extension| extension.name)
                    .collect();
                names.extend(loaded.iter().map(|extension| extension.name));
                plugins::take_in(loaded);
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
                let watcher = watch::on(
                    extension.component.clone(),
                    changed.clone(),
                    kept.wake.clone(),
                )?;
                Some(Watched {
                    directory: extension.directory.clone(),
                    changed,
                    _watcher: watcher,
                })
            })
            .collect();
        *kept.watched.borrow_mut() = watched;
    }

    /// Reads again the packages whose components were built again since
    /// this was last asked. One that does not load leaves the extension
    /// as it was.
    pub(super) fn follow_extensions(&self) {
        let built: Vec<PathBuf> = (self.kept().watched.borrow().iter())
            .filter(|watched| watched.changed.settled())
            .map(|watched| watched.directory.clone())
            .collect();
        for directory in built {
            self.in_background(
                move || Extension::load(&directory).map_err(|error| (directory, error)),
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
