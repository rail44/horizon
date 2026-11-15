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
use std::sync::Arc;

use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Dispose, GetUntracked};
use serde::Deserialize;
use wasmtime::component::types::{ComponentItem, Type};
use wasmtime::component::{Instance, ResourceAny, Val};
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

use crate::plugin_host::{Asks, Compiled, Loaded, compile, instantiate};
use crate::wake::Wake;

/// What a package's manifest says.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    plugin: ManifestPlugin,
    #[serde(default)]
    commands: BTreeMap<String, ManifestCommand>,
    /// How each parameter of `create` is asked for, by the parameter's name.
    #[serde(default)]
    create: BTreeMap<String, ManifestParameter>,
    #[serde(default)]
    access: Access,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestParameter {
    /// For a file: the extensions of the files to choose among; any file
    /// when there are none.
    #[serde(default)]
    extensions: Vec<String>,
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
    /// What a new pane of it is made from, if it says (`create`).
    pub creates: Option<Creates>,
    /// The package's directory, and its component's file in it.
    pub directory: PathBuf,
    pub component: PathBuf,
    compiled: Compiled,
    read: Vec<PathBuf>,
    /// What a pane keeps of it: the type `state` gives.
    state: Type,
}

/// What an extension's `create` takes to make a new pane's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Creates {
    /// A file, chosen among those with one of `extensions` (any file when
    /// there are none).
    File { extensions: Vec<String> },
}

/// `text`, kept for as long as the shell runs: a command's name and texts
/// are named by `&'static str` wherever commands are.
fn kept(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The file of the component the package in `directory` names, if its
/// manifest can be read: what a build of the extension replaces.
pub fn component_file(directory: &Path) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(directory.join("plugin.toml")).ok()?;
    let manifest: Manifest = toml::from_str(&manifest).ok()?;
    Some(directory.join(manifest.plugin.component))
}

/// What a component declares of itself, read from its type.
struct Shape {
    /// The type `state` gives: what a pane keeps of the extension.
    state: Type,
    /// The commands its `commands` interface exports, in the order they
    /// were read.
    commands: Vec<String>,
    /// The name of the file parameter `create` takes, if it exports one.
    created_on: Option<String>,
}

/// Reads what the component exports, and holds it to what an extension
/// must export.
fn shape_of(compiled: &Compiled) -> Result<Shape, String> {
    let engine = &compiled.engine;
    let exported = compiled.component.component_type();
    // The host's file resource, as the component names it.
    let file = (exported.imports(engine))
        .filter(|(name, _)| name.starts_with("horizon:ui/files"))
        .find_map(|(_, item)| match item.ty {
            ComponentItem::ComponentInstance(instance) => {
                (instance.exports(engine)).find_map(|(name, item)| match item.ty {
                    ComponentItem::Resource(file) if name == "file" => Some(file),
                    _ => None,
                })
            }
            _ => None,
        });
    let mut state = None;
    let mut created_on = None;
    let mut commands = Vec::new();
    let mut mount = false;
    let mut events = false;
    for (name, item) in exported.exports(engine) {
        match (name, item.ty) {
            ("mount", ComponentItem::ComponentFunc(_)) => mount = true,
            ("state", ComponentItem::ComponentFunc(function)) => {
                state = function.results().next();
            }
            ("create", ComponentItem::ComponentFunc(function)) => {
                let parameters: Vec<_> = function.params().collect();
                let [(name, Type::Own(resource))] = parameters.as_slice() else {
                    return Err(
                        "`create` takes what the host cannot ask for yet: it can ask for one file"
                            .to_string(),
                    );
                };
                if Some(resource) != file.as_ref() {
                    return Err(format!(
                        "`create` takes `{name}`, which is not the host's `file`"
                    ));
                }
                created_on = Some(name.to_string());
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
                    commands.push(command.to_string());
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
    Ok(Shape {
        state,
        commands,
        created_on,
    })
}

/// Holds the commands the component exports and the ones the manifest
/// names to each other: each must name the other.
fn match_commands(
    exported: &[String],
    named: &BTreeMap<String, ManifestCommand>,
) -> Result<(), String> {
    for function in exported {
        if !named.contains_key(function) {
            return Err(format!(
                "the manifest says nothing of the command `{function}`"
            ));
        }
    }
    for name in named.keys() {
        if !exported.contains(name) {
            return Err(format!("the component has no command `{name}`"));
        }
    }
    Ok(())
}

impl Extension {
    /// The package in `directory`.
    pub fn load(directory: &Path) -> Result<Self, String> {
        let at = directory.join("plugin.toml");
        let manifest =
            std::fs::read_to_string(&at).map_err(|error| format!("{}: {error}", at.display()))?;
        let mut manifest: Manifest =
            toml::from_str(&manifest).map_err(|error| format!("{}: {error}", at.display()))?;
        let compiled = compile(&directory.join(&manifest.plugin.component))?;
        let Shape {
            state,
            commands,
            created_on,
        } = shape_of(&compiled)?;
        match_commands(&commands, &manifest.commands)?;
        // (What the manifest says of how `create`'s file is asked for.)
        let creates = created_on.map(|parameter| Creates::File {
            extensions: (manifest.create.remove(&parameter))
                .unwrap_or_default()
                .extensions,
        });
        if let Some(name) = manifest.create.keys().next() {
            return Err(format!("`create` has no parameter `{name}`"));
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
            creates,
            directory: directory.to_path_buf(),
            component: directory.join(&manifest.plugin.component),
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
        self: &Arc<Self>,
        asks: Asks,
        wake: Wake,
        failed: RwSignal<Option<String>>,
    ) -> Result<Running, String> {
        let mut wasi = WasiCtxBuilder::new();
        for directory in &self.read {
            let path = directory.to_string_lossy();
            wasi.preopened_dir(directory, &*path, FsPerms::ReadOnly)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
        }
        let loaded = instantiate(&self.compiled, wasi.build(), asks, wake, failed)
            .map_err(|error| format!("{error:#}"))?;
        Ok(Running {
            extension: self.clone(),
            loaded,
        })
    }
}

impl Extension {
    /// The state of a new pane on the file at `path`, as text: what the
    /// extension's `create` makes of it, asked of an instance made for the
    /// question.
    pub fn create(self: &Arc<Self>, path: &Path) -> Result<String, String> {
        let failed = RwSignal::new(None::<String>);
        let state =
            (self.instantiate(Asks::default(), Wake::default(), failed)).and_then(|running| {
                let path = path.to_path_buf();
                let state = running.loaded.call(|instance, store| {
                    Box::pin(async move {
                        let create = function(instance, store, None, "create")?;
                        let file = store.data_mut().file(path)?;
                        let file = ResourceAny::try_from_resource(file, &mut *store)?;
                        let mut answer = [Val::Bool(false)];
                        create
                            .call_async(&mut *store, &[Val::Resource(file)], &mut answer)
                            .await?;
                        Ok(answer[0].clone())
                    })
                });
                let said = failed.get_untracked();
                state
                    .and_then(|state| state.to_wave().ok())
                    .ok_or_else(|| said.unwrap_or_else(|| "`create` gave no state".to_string()))
            });
        failed.dispose();
        state
    }
}

/// An extension running for a pane.
pub struct Running {
    extension: Arc<Extension>,
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
            Box::pin(async move {
                let mount = function(instance, store, None, "mount")?;
                let mut answer = [Val::U64(0)];
                mount.call_async(&mut *store, &[given], &mut answer).await?;
                Ok(answer[0].clone())
            })
        });
        match root {
            Some(Val::U64(root)) => Ok(root),
            _ => Err("the view was not built".to_string()),
        }
    }

    /// What the pane keeps of it now, as text.
    pub fn state(&self) -> Option<String> {
        let value = self.loaded.call(|instance, store| {
            Box::pin(async move {
                let state = function(instance, store, None, "state")?;
                let mut answer = [Val::Bool(false)];
                state.call_async(&mut *store, &[], &mut answer).await?;
                Ok(answer[0].clone())
            })
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
        let name = name.to_string();
        let ran = self.loaded.call(|instance, store| {
            Box::pin(async move {
                let commands = instance
                    .get_export_index(&mut *store, None, "commands")
                    .ok_or_else(|| wasmtime::Error::msg("no commands"))?;
                let command = function(instance, store, Some(&commands), &name)?;
                command.call_async(&mut *store, &[], &mut []).await
            })
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
pub fn load_all(directories: &[String]) -> Vec<Arc<Extension>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    (directories.iter())
        .filter_map(|directory| {
            let directory = match (directory.strip_prefix("~/"), &home) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(directory),
            };
            match Extension::load(&directory) {
                Ok(extension) => Some(Arc::new(extension)),
                Err(error) => {
                    eprintln!("horizon extension {}: {error}", directory.display());
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(commands: &[&str]) -> BTreeMap<String, ManifestCommand> {
        (commands.iter())
            .map(|name| {
                (
                    name.to_string(),
                    ManifestCommand {
                        title: String::new(),
                        description: String::new(),
                        destructive: false,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn matching_command_lists_agree() {
        let exported = vec!["back".to_string()];
        assert!(match_commands(&exported, &named(&["back"])).is_ok());
    }

    #[test]
    fn an_exported_command_the_manifest_omits_is_refused() {
        let exported = vec!["back".to_string(), "forward".to_string()];
        let error = match_commands(&exported, &named(&["back"])).unwrap_err();
        assert!(error.contains("forward"), "{error}");
    }

    #[test]
    fn a_named_command_the_component_lacks_is_refused() {
        let exported = vec!["back".to_string()];
        let error = match_commands(&exported, &named(&["back", "forward"])).unwrap_err();
        assert!(error.contains("forward"), "{error}");
    }

    #[test]
    fn a_component_without_commands_matches_an_empty_manifest() {
        assert!(match_commands(&[], &BTreeMap::new()).is_ok());
    }
}
