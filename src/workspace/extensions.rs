//! The extensions the shell comes with and the ones the configuration
//! names, kept to what their components are: read again when the
//! configuration is, and one whose component is built again is read again
//! by itself. The panes that show an extension read again are made again,
//! on the state they keep.
//!
//! One still being written is tried where the one in use is: `horizon
//! extension use <file>` takes it in place of the one of its name, with
//! all the shell gives an extension, until `horizon extension reset` (or
//! the `Reset Extensions` command) lets go of it or the shell is started
//! again. Reading the configuration again leaves it in.

use std::path::PathBuf;
use std::sync::Arc;

use horizon_control::contract::{EnvelopeBody, ExtensionEntry, Extensions};
use horizon_ui::reactive_graph::traits::WithUntracked;

use super::{Shell, plugins};
use crate::extension::Extension;
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
        self.reload_extensions_then(files, |_| {});
    }

    /// [`reload_extensions`](Self::reload_extensions), and `then` once
    /// they are taken in. Those in use for now come before `files`, and so
    /// in place of what `files` or the shell's own have of their names.
    fn reload_extensions_then(
        &self,
        files: Vec<String>,
        then: impl FnOnce(Shell) + Send + 'static,
    ) {
        let kept = self.kept();
        let own = kept.own_extensions.clone();
        let read = kept.read_extensions.clone();
        let mut tried: Vec<String> = (kept.tried_extensions.borrow().iter())
            .map(|(_, file)| file.to_string_lossy().into_owned())
            .collect();
        tried.extend(files);
        let files = tried;
        self.in_background(
            move || read(&files, &own),
            |shell, loaded| {
                let mut names: Vec<&str> = (plugins::extensions().iter())
                    .map(|extension| extension.name)
                    .collect();
                names.extend(loaded.iter().map(|extension| extension.name));
                plugins::take_in(loaded);
                shell.kept().extensions_read.set(true);
                shell.apply_startup_layout();
                // (Every command there can be is known from here.)
                shell.say_unknown_bindings();
                shell.watch_extensions();
                shell.remake_panes_of(&names);
                then(shell);
            },
        );
    }

    /// Takes the extension whose component is `file` in place of the one
    /// of its name, for now. `then` hears its name once it is in, or what
    /// kept its component from loading.
    pub(super) fn use_extension(
        &self,
        file: PathBuf,
        then: impl FnOnce(Shell, Result<&'static str, String>) + Send + 'static,
    ) {
        self.in_background(
            move || Extension::load(&file).map(|extension| (file, extension)),
            |shell, loaded| match loaded {
                Ok((file, extension)) => {
                    let name = extension.name;
                    {
                        let kept = shell.kept();
                        let mut tried = kept.tried_extensions.borrow_mut();
                        tried.retain(|(other, _)| *other != name);
                        tried.push((name, file));
                    }
                    plugins::put(Arc::new(extension));
                    shell.say_unknown_bindings();
                    shell.watch_extensions();
                    shell.remake_panes_of(&[name]);
                    then(shell, Ok(name));
                }
                Err(error) => then(shell, Err(error)),
            },
        );
    }

    /// Lets go of what [`use_extension`](Self::use_extension) took in --
    /// the one named `name`, or all -- and reads the configuration's and
    /// the shell's own again; `then` once they are in. Refused where
    /// `name` is not one in use for now.
    pub(super) fn reset_extensions(
        &self,
        name: Option<&str>,
        then: impl FnOnce(Shell) + Send + 'static,
    ) -> Result<(), String> {
        let kept = self.kept();
        {
            let mut tried = kept.tried_extensions.borrow_mut();
            match name {
                Some(name) if !tried.iter().any(|(other, _)| *other == name) => {
                    return Err(format!(
                        "`{name}` is not an extension `horizon extension use` put in"
                    ));
                }
                Some(name) => tried.retain(|(other, _)| *other != name),
                None => tried.clear(),
            }
        }
        let files = kept.config.borrow().extensions.clone();
        self.reload_extensions_then(files, then);
        Ok(())
    }

    /// The extensions loaded and where each is from, as the control plane
    /// says them.
    pub(super) fn extensions_body(&self) -> EnvelopeBody {
        let kept = self.kept();
        let tried = kept.tried_extensions.borrow();
        let extensions = (plugins::extensions().iter())
            .map(|extension| {
                let source = if tried.iter().any(|(_, file)| *file == extension.file) {
                    "temporary"
                } else if kept.own_extensions.contains(&extension.file) {
                    "own"
                } else {
                    "configured"
                };
                ExtensionEntry {
                    name: extension.name.to_string(),
                    source: source.to_string(),
                    file: extension.file.to_string_lossy().into_owned(),
                }
            })
            .collect();
        EnvelopeBody::Extensions(Extensions { extensions })
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
