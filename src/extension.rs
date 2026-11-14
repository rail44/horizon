//! An extension: a view built as a wasm component, in a package of its
//! own with a manifest (`plugin.toml`) beside it.
//!
//! The component declares what is its own: the state a pane keeps of it
//! (the type `mount` takes and `state` gives), and its commands (the
//! functions of its `commands` interface). Everything every plugin has
//! comes from `horizon-ui`'s SDK. The manifest says what the component's
//! types cannot: the extension's name and title, its commands' titles
//! and descriptions, and what it may read. The two are matched when the
//! package is loaded, and a package whose commands differ is refused.
//!
//! A pane's state is kept as the value's text in WAVE, the WIT values'
//! text format, and given back to `mount` when the pane is made again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use horizon_ui::reactive_graph::signal::RwSignal;
use serde::Deserialize;
use wasmtime::component::types::{ComponentItem, Type};
use wasmtime::component::{Instance, Val};
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

use crate::plugin_host::{Asks, Compiled, Loaded, compile, instantiate};

/// What a package's manifest says.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    plugin: ManifestPlugin,
    #[serde(default)]
    commands: BTreeMap<String, ManifestCommand>,
    #[serde(default)]
    access: Access,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPlugin {
    name: String,
    title: String,
    /// The component's file, in the package.
    component: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCommand {
    title: String,
    description: String,
    #[serde(default)]
    destructive: bool,
}

/// What the extension may read.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Access {
    /// Directories, lent to it read-only at the same paths.
    #[serde(default)]
    read: Vec<String>,
}

/// A command of an extension, as the palette lists it.
pub struct ExtensionCommand {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub destructive: bool,
}

/// A package loaded: its component compiled, matched with its manifest.
pub struct Extension {
    pub name: &'static str,
    pub title: String,
    pub commands: Vec<ExtensionCommand>,
    compiled: Compiled,
    read: Vec<PathBuf>,
    /// What a pane keeps of it: the type `state` gives.
    state: Type,
}

/// `text`, kept for as long as the shell runs: a command's name and texts
/// are named by `&'static str` wherever commands are.
fn kept(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

impl Extension {
    /// The package in `directory`.
    pub fn load(directory: &Path) -> Result<Self, String> {
        let at = directory.join("plugin.toml");
        let manifest =
            std::fs::read_to_string(&at).map_err(|error| format!("{}: {error}", at.display()))?;
        let manifest: Manifest =
            toml::from_str(&manifest).map_err(|error| format!("{}: {error}", at.display()))?;
        let compiled = compile(&directory.join(&manifest.plugin.component))?;
        let engine = &compiled.engine;
        let shape = compiled.component.component_type();
        let mut state = None;
        let mut functions = Vec::new();
        let mut mount = false;
        let mut events = false;
        for (name, item) in shape.exports(engine) {
            match (name, item.ty) {
                ("mount", ComponentItem::ComponentFunc(_)) => mount = true,
                ("state", ComponentItem::ComponentFunc(function)) => {
                    state = function.results().next();
                }
                ("commands", ComponentItem::ComponentInstance(instance)) => {
                    for (command, item) in instance.exports(engine) {
                        let ComponentItem::ComponentFunc(function) = item.ty else {
                            continue;
                        };
                        if function.params().next().is_some() {
                            return Err(format!(
                                "the command `{command}` takes parameters, which an extension's \
                                 commands cannot yet"
                            ));
                        }
                        functions.push(command.to_string());
                    }
                }
                (name, ComponentItem::ComponentInstance(_))
                    if name.starts_with("horizon:ui/events") =>
                {
                    events = true;
                }
                _ => {}
            }
        }
        let state = state.ok_or("the component exports no `state`")?;
        if !mount {
            return Err("the component exports no `mount`".to_string());
        }
        if !events {
            return Err("the component does not export `horizon:ui/events`".to_string());
        }
        // The commands the component has are the ones the manifest names.
        let named: Vec<&String> = manifest.commands.keys().collect();
        for function in &functions {
            if !manifest.commands.contains_key(function) {
                return Err(format!(
                    "the manifest says nothing of the command `{function}`"
                ));
            }
        }
        for name in named {
            if !functions.contains(name) {
                return Err(format!("the component has no command `{name}`"));
            }
        }
        let commands = (manifest.commands.into_iter())
            .map(|(name, command)| ExtensionCommand {
                name: kept(name),
                title: kept(command.title),
                description: kept(command.description),
                destructive: command.destructive,
            })
            .collect();
        Ok(Self {
            name: kept(manifest.plugin.name),
            title: manifest.plugin.title,
            commands,
            compiled,
            read: manifest
                .access
                .read
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            state,
        })
    }

    /// The extension in a store of its own, for one pane: given what it may
    /// read and `asks`; `failed` is told what it said if it traps.
    pub fn instantiate(
        self: &Rc<Self>,
        asks: Asks,
        failed: RwSignal<Option<String>>,
    ) -> Result<Running, String> {
        let mut wasi = WasiCtxBuilder::new();
        for directory in &self.read {
            let path = directory.to_string_lossy();
            wasi.preopened_dir(directory, &*path, FsPerms::ReadOnly)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
        }
        let loaded = instantiate(&self.compiled, wasi.build(), asks, failed)
            .map_err(|error| format!("{error:#}"))?;
        Ok(Running {
            extension: self.clone(),
            loaded,
        })
    }
}

/// An extension running for a pane.
pub struct Running {
    extension: Rc<Extension>,
    loaded: Rc<Loaded>,
}

impl Running {
    /// Builds the view on `state`, the text a pane kept (none for a new
    /// pane): its root node.
    pub fn mount(&self, state: Option<&str>) -> Result<u64, String> {
        let given = match state {
            Some(text) => {
                let value = Val::from_wave(&self.extension.state, text)
                    .map_err(|error| format!("the state kept, {text:?}: {error}"))?;
                Val::Option(Some(Box::new(value)))
            }
            None => Val::Option(None),
        };
        let root = self.loaded.call(|instance, store| {
            let mount = function(instance, store, None, "mount")?;
            let mut answer = [Val::U64(0)];
            mount.call(&mut *store, &[given], &mut answer)?;
            Ok(answer[0].clone())
        });
        match root {
            Some(Val::U64(root)) => Ok(root),
            _ => Err("the view was not built".to_string()),
        }
    }

    /// What the pane keeps of it now, as text.
    pub fn state(&self) -> Option<String> {
        let value = self.loaded.call(|instance, store| {
            let state = function(instance, store, None, "state")?;
            let mut answer = [Val::Bool(false)];
            state.call(&mut *store, &[], &mut answer)?;
            Ok(answer[0].clone())
        })?;
        value.to_wave().ok()
    }

    /// Carries out the command named `name`; `false` when there is none.
    pub fn run(&self, name: &str) -> bool {
        if !self
            .extension
            .commands
            .iter()
            .any(|command| command.name == name)
        {
            return false;
        }
        let ran = self.loaded.call(|instance, store| {
            let commands = instance
                .get_export_index(&mut *store, None, "commands")
                .ok_or_else(|| wasmtime::Error::msg("no commands"))?;
            let command = function(instance, store, Some(&commands), name)?;
            command.call(&mut *store, &[], &mut [])
        });
        ran.is_some()
    }

    /// Has the extension follow the page: its work after each layout, its
    /// effects when the page's run.
    pub fn follow(&self) {
        self.loaded.follow();
    }
}

/// The function `name` the instance exports, inside `within` if given.
fn function(
    instance: &Instance,
    store: &mut wasmtime::Store<crate::plugin_host::State>,
    within: Option<&wasmtime::component::ComponentExportIndex>,
    name: &str,
) -> wasmtime::Result<wasmtime::component::Func> {
    let index = instance
        .get_export_index(&mut *store, within, name)
        .ok_or_else(|| wasmtime::Error::msg(format!("no `{name}`")))?;
    instance
        .get_func(&mut *store, index)
        .ok_or_else(|| wasmtime::Error::msg(format!("`{name}` is not a function")))
}

/// The packages in `directories`, those that load; what kept one from
/// loading is said on stderr.
pub fn load_all(directories: &[String]) -> Vec<Rc<Extension>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    (directories.iter())
        .filter_map(|directory| {
            let directory = match (directory.strip_prefix("~/"), &home) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(directory),
            };
            match Extension::load(&directory) {
                Ok(extension) => Some(Rc::new(extension)),
                Err(error) => {
                    eprintln!("horizon extension {}: {error}", directory.display());
                    None
                }
            }
        })
        .collect()
}
