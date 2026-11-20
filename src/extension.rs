//! An extension: a view built as a wasm component. The component's file
//! is the whole of it: its manifest is a custom section of the wasm
//! ([`MANIFEST`]).
//!
//! The component declares what is its own: the state a pane keeps of it
//! (the type `mount` takes and `state` gives), and its commands (the
//! functions of its `commands` interface). Everything every plugin has
//! comes from `horizon-ui`'s SDK. The manifest says what the component's
//! types cannot: the extension's name and title, its commands' titles
//! and descriptions, and what it may read. The two are matched when the
//! component is loaded, and one whose commands differ is refused. An
//! extension written in Rust has both made from one description
//! (`horizon_ui::plugin`), so they cannot differ.
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
use wasmtime::component::{Accessor, ResourceAny, Val};
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

use crate::plugin::Given;
use crate::plugin_host::{Asks, Compiled, Loaded, Plugin, State, compile, instantiate, offered};
use crate::wake::Wake;

/// The custom section of a component that holds its manifest, as JSON.
pub const MANIFEST: &str = "horizon:manifest";

/// The manifest in the component `bytes`, if it has one.
fn manifest_in(bytes: &[u8]) -> Option<String> {
    (wasmparser::Parser::new(0).parse_all(bytes)).find_map(|part| match part.ok()? {
        wasmparser::Payload::CustomSection(section) if section.name() == MANIFEST => {
            String::from_utf8(section.data().to_vec()).ok()
        }
        _ => None,
    })
}

/// What an extension's manifest says.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    plugin: ManifestPlugin,
    /// The extension's own commands, by their names.
    #[serde(default)]
    commands: BTreeMap<String, ManifestCommand>,
    /// Of the view a pane shows, if it has one.
    #[serde(default)]
    pane: ManifestPane,
    #[serde(default)]
    access: Access,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPane {
    /// The view's commands, by their names.
    #[serde(default)]
    commands: BTreeMap<String, ManifestCommand>,
    /// How each parameter of `create` is asked for, by the parameter's name.
    #[serde(default)]
    create: BTreeMap<String, ManifestParameter>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestParameter {
    /// For a file: the media types of the files to choose among
    /// (`crate::media_type`); any file when there are none.
    #[serde(default)]
    types: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPlugin {
    name: String,
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCommand {
    title: String,
    description: String,
    #[serde(default)]
    destructive: bool,
}

/// What the extension is lent of the files.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Access {
    /// Directories, lent to it read-only at the same paths.
    #[serde(default)]
    read: Vec<String>,
    /// Directories it may write in too, made if they are not there. One
    /// may begin with [`DATA`].
    #[serde(default)]
    write: Vec<String>,
}

/// Where a lent directory begins when it is in Horizon's own data:
/// `{data}/wiki` is the `horizon/wiki` of the data home, wherever the
/// environment puts that.
const DATA: &str = "{data}";

/// `$HORIZON_DATA_HOME`, else `$XDG_DATA_HOME`, else `~/.local/share`: as
/// the board, the agent and the wiki resolve theirs.
fn data_home() -> Option<PathBuf> {
    let named = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    match named("HORIZON_DATA_HOME").or_else(|| named("XDG_DATA_HOME")) {
        Some(value) => Some(PathBuf::from(value)),
        None => named("HOME").map(|home| PathBuf::from(home).join(".local").join("share")),
    }
}

/// A directory the manifest names, as it is on this machine, and whether
/// it is in Horizon's data.
fn lent(named: &str) -> Result<(PathBuf, bool), String> {
    match named.strip_prefix(DATA) {
        Some(rest) => {
            let home = data_home().ok_or("no data home to lend from")?;
            let rest = rest.trim_start_matches('/');
            Ok((home.join("horizon").join(rest), true))
        }
        None => Ok((PathBuf::from(named), false)),
    }
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
    /// Whether it has a view a pane shows (`pane`). One without is its
    /// own commands only.
    pub pane: bool,
    /// The commands of a pane that shows its view (`pane-commands`).
    pub pane_commands: Vec<ExtensionCommand>,
    /// Its own commands, which need no pane (`commands`).
    pub commands: Vec<ExtensionCommand>,
    /// What a new pane of it is made from, if it says (`create`): what
    /// `create` takes, in order.
    pub creates: Option<Vec<Parameter>>,
    /// The component's file.
    pub file: PathBuf,
    compiled: Compiled,
    /// The directories lent to it, as the manifest names them: to read,
    /// and to write in too.
    read: Vec<String>,
    write: Vec<String>,
    /// What a pane keeps of it: the type `state` gives. None when its
    /// panes keep nothing (it exports no `state`, and its `mount` takes
    /// nothing).
    state: Option<Type>,
}

/// One thing an extension's `create` takes to make a new pane's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parameter {
    /// A file, chosen among those of one of `types` (any file when
    /// there are none).
    File { name: String, types: Vec<String> },
    /// Words: the next argument of the command line. One that is
    /// `optional` may be left out.
    Text { name: String, optional: bool },
    /// Where the pane was asked for. Nobody is asked for it.
    WorkingDirectory,
}

/// What `create` is called with for one parameter.
enum Handed {
    File(PathBuf),
    Text(String),
    /// A text that may be left out, and whether it was.
    OptionalText(Option<String>),
    WorkingDirectory(PathBuf),
}

/// `text`, kept for as long as the shell runs: a command's name and texts
/// are named by `&'static str` wherever commands are.
fn kept(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// What a component declares of itself, read from its type.
#[derive(Default)]
struct Shape {
    /// Whether it exports `pane` with a `mount` in it.
    pane: bool,
    /// The type `state` gives: what a pane keeps of the extension, if
    /// its panes keep anything.
    state: Option<Type>,
    /// The commands its `pane-commands` interface exports, in the order
    /// they were read.
    pane_commands: Vec<String>,
    /// Those of its `commands` interface.
    commands: Vec<String>,
    /// What `create` takes, in order, if it exports one. (A file's
    /// `types` are the manifest's to say.)
    creates: Option<Vec<Parameter>>,
}

/// Reads what the component exports, and holds it to what an extension
/// must export.
fn shape_of(compiled: &Compiled) -> Result<Shape, String> {
    let engine = &compiled.engine;
    let exported = compiled.component.component_type();
    // The host's resources, as the component names them.
    let resource = |wanted: &str| {
        (exported.imports(engine))
            .filter(|(name, _)| name.starts_with("horizon:ui/files"))
            .find_map(|(_, item)| match item.ty {
                ComponentItem::ComponentInstance(instance) => {
                    (instance.exports(engine)).find_map(|(name, item)| match item.ty {
                        ComponentItem::Resource(resource) if name == wanted => Some(resource),
                        _ => None,
                    })
                }
                _ => None,
            })
    };
    let (file, working_directory) = (resource("file"), resource("working-directory"));
    // What `create` takes, as the host asks for each.
    let created = |function: &wasmtime::component::types::ComponentFunc| {
        let mut taken = Vec::new();
        for (name, ty) in function.params() {
            let name = name.to_string();
            taken.push(match ty {
                Type::Own(resource) if Some(&resource) == file.as_ref() => Parameter::File {
                    name,
                    types: Vec::new(),
                },
                Type::Own(resource) if Some(&resource) == working_directory.as_ref() => {
                    Parameter::WorkingDirectory
                }
                Type::String => Parameter::Text {
                    name,
                    optional: false,
                },
                Type::Option(inner) if inner.ty() == Type::String => Parameter::Text {
                    name,
                    optional: true,
                },
                _ => {
                    return Err(format!(
                        "`create` takes `{name}`, which the host has no way to ask for: a \
                         file, a text or where the pane was asked for"
                    ));
                }
            });
        }
        let files = (taken.iter())
            .filter(|parameter| matches!(parameter, Parameter::File { .. }))
            .count();
        if files > 1 {
            return Err("`create` takes more than one file".to_string());
        }
        Ok(taken)
    };
    // The commands an interface exports.
    let commands_of = |instance: &wasmtime::component::types::ComponentInstance| {
        let mut commands = Vec::new();
        for (command, item) in instance.exports(engine) {
            let ComponentItem::ComponentFunc(function) = item.ty else {
                continue;
            };
            if function.params().next().is_some() {
                return Err(format!(
                    "the command `{command}` takes parameters, which an extension's commands \
                     cannot yet"
                ));
            }
            commands.push(command.to_string());
        }
        Ok(commands)
    };
    let mut shape = Shape::default();
    let mut events = false;
    for (name, item) in exported.exports(engine) {
        match (name, item.ty) {
            ("pane", ComponentItem::ComponentInstance(instance)) => {
                for (name, item) in instance.exports(engine) {
                    match (name, item.ty) {
                        ("mount", ComponentItem::ComponentFunc(_)) => shape.pane = true,
                        ("state", ComponentItem::ComponentFunc(function)) => {
                            shape.state = function.results().next();
                        }
                        ("create", ComponentItem::ComponentFunc(function)) => {
                            shape.creates = Some(created(&function)?);
                        }
                        _ => {}
                    }
                }
                if !shape.pane {
                    return Err("the component's `pane` exports no `mount`".to_string());
                }
            }
            ("pane-commands", ComponentItem::ComponentInstance(instance)) => {
                shape.pane_commands = commands_of(&instance)?;
            }
            ("commands", ComponentItem::ComponentInstance(instance)) => {
                shape.commands = commands_of(&instance)?;
            }
            (name, ComponentItem::ComponentInstance(_))
                if name.starts_with("horizon:ui/events") =>
            {
                events = true;
            }
            _ => {}
        }
    }
    if !shape.pane && shape.commands.is_empty() {
        return Err("the component exports no `pane`, and no command of its own".to_string());
    }
    if !shape.pane && !shape.pane_commands.is_empty() {
        return Err("the component exports `pane-commands` and no `pane`".to_string());
    }
    if !events {
        return Err("the component does not export `horizon:ui/events`".to_string());
    }
    Ok(shape)
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
    /// The extension whose component is at `file`.
    pub fn load(file: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(file).map_err(|error| error.to_string())?;
        let manifest = manifest_in(&bytes)
            .ok_or_else(|| format!("the component has no manifest (`{MANIFEST}`)"))?;
        let mut manifest: Manifest =
            serde_json::from_str(&manifest).map_err(|error| format!("the manifest: {error}"))?;
        let compiled = compile(file)?;
        // (Before a pane is made of it: an extension built for a host
        // that offered something else is not one a pane can show.)
        offered(&compiled).map_err(|error| {
            format!("it asks of the host what this Horizon does not offer: {error}")
        })?;
        let Shape {
            pane,
            state,
            pane_commands,
            commands,
            mut creates,
        } = shape_of(&compiled)?;
        match_commands(&pane_commands, &manifest.pane.commands)?;
        match_commands(&commands, &manifest.commands)?;
        // (What the manifest says of how `create`'s file is asked for.)
        for parameter in creates.iter_mut().flatten() {
            if let Parameter::File { name, types } = parameter {
                *types = (manifest.pane.create.remove(name.as_str()))
                    .unwrap_or_default()
                    .types;
            }
        }
        if let Some(name) = manifest.pane.create.keys().next() {
            return Err(format!("`create` has no file parameter `{name}`"));
        }
        let listed = |commands: BTreeMap<String, ManifestCommand>| {
            (commands.into_iter())
                .map(|(name, command)| ExtensionCommand {
                    name: kept(name),
                    title: kept(command.title),
                    description: kept(command.description),
                    destructive: command.destructive,
                })
                .collect()
        };
        Ok(Self {
            name: kept(manifest.plugin.name),
            title: manifest.plugin.title,
            pane,
            pane_commands: listed(manifest.pane.commands),
            commands: listed(manifest.commands),
            creates,
            file: file.to_path_buf(),
            compiled,
            read: manifest.access.read,
            write: manifest.access.write,
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
        let mut in_data = false;
        let named = (self.read.iter().map(|named| (named, FsPerms::ReadOnly)))
            .chain(self.write.iter().map(|named| (named, FsPerms::ReadWrite)));
        for (named, perms) in named {
            let (directory, data) = lent(named)?;
            in_data |= data;
            let said = |error: &dyn std::fmt::Display| format!("{}: {error}", directory.display());
            if matches!(perms, FsPerms::ReadWrite) {
                std::fs::create_dir_all(&directory).map_err(|error| said(&error))?;
            }
            let path = directory.to_string_lossy();
            wasi.preopened_dir(&directory, &*path, perms)
                .map_err(|error| said(&error))?;
        }
        // What is lent of Horizon's data is where the extension would
        // look for it: it reads the data home as Horizon's own code does.
        if let Some(home) = data_home().filter(|_| in_data) {
            wasi.env("HORIZON_DATA_HOME", home.to_string_lossy());
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
    /// The media types of the files `create`'s file is chosen among, if
    /// it takes a file.
    pub fn file_asked_for(&self) -> Option<&[String]> {
        (self.creates.iter().flatten()).find_map(|parameter| match parameter {
            Parameter::File { types, .. } => Some(types.as_slice()),
            _ => None,
        })
    }

    /// Refuses a pane of an extension that is its own commands only.
    fn shown_in_a_pane(&self) -> Result<(), String> {
        match self.pane {
            true => Ok(()),
            false => Err(format!("`{}` has no view a pane shows", self.name)),
        }
    }

    /// The state of a new pane, as text, made from what was `given` on
    /// the command line: a file is a path from where it was run, a text
    /// the next argument. `None` for an extension whose panes are made
    /// from nothing.
    pub fn created_from(self: &Arc<Self>, given: &mut Given) -> Result<Option<String>, String> {
        self.shown_in_a_pane()?;
        let Some(parameters) = &self.creates else {
            return Ok(None);
        };
        let mut handed = Vec::new();
        for parameter in parameters {
            handed.push(match parameter {
                Parameter::File { name, .. } => Handed::File(given.file(&format!("`{name}`"))?),
                Parameter::Text {
                    name,
                    optional: false,
                } => {
                    let text = given.argument();
                    Handed::Text(text.ok_or_else(|| format!("`{name}` is needed"))?)
                }
                Parameter::Text { optional: true, .. } => Handed::OptionalText(given.argument()),
                Parameter::WorkingDirectory => Handed::WorkingDirectory(given.directory.clone()),
            });
        }
        self.create(handed).map(Some)
    }

    /// The state of a new pane, as text, made where nobody types: on
    /// `file`, if `create` takes one, a person having chosen it, with
    /// `directory` as where the pane was asked for. A text that may be
    /// left out is; one that may not cannot be asked for here.
    pub fn created_on(
        self: &Arc<Self>,
        file: Option<PathBuf>,
        directory: &Path,
    ) -> Result<Option<String>, String> {
        self.shown_in_a_pane()?;
        let Some(parameters) = &self.creates else {
            return Ok(None);
        };
        let mut handed = Vec::new();
        for parameter in parameters {
            handed.push(match parameter {
                Parameter::File { name, .. } => {
                    Handed::File(file.clone().ok_or_else(|| format!("`{name}` is needed"))?)
                }
                Parameter::Text {
                    name,
                    optional: false,
                } => {
                    return Err(format!(
                        "`{name}` is asked for on the command line: `horizon new {} <{name}>`",
                        self.name
                    ));
                }
                Parameter::Text { optional: true, .. } => Handed::OptionalText(None),
                Parameter::WorkingDirectory => Handed::WorkingDirectory(directory.to_path_buf()),
            });
        }
        self.create(handed).map(Some)
    }

    /// What the extension's `create` makes of what it is `handed`, asked
    /// of an instance made for the question.
    fn create(self: &Arc<Self>, handed: Vec<Handed>) -> Result<String, String> {
        let failed = RwSignal::new(None::<String>);
        let state =
            (self.instantiate(Asks::default(), Wake::default(), failed)).and_then(|running| {
                let state = running.loaded.call(|plugin, accessor| {
                    Box::pin(async move {
                        let create = function(plugin, accessor, Some("pane"), "create")?;
                        let mut values = Vec::new();
                        for handed in handed {
                            values.push(match handed {
                                Handed::Text(text) => Val::String(text),
                                Handed::OptionalText(text) => {
                                    Val::Option(text.map(|text| Box::new(Val::String(text))))
                                }
                                Handed::File(path) => accessor.with(|mut access| {
                                    let file = access.data_mut().file(path)?;
                                    ResourceAny::try_from_resource(file, &mut access)
                                        .map(Val::Resource)
                                })?,
                                Handed::WorkingDirectory(path) => accessor.with(|mut access| {
                                    let directory = access.data_mut().working_directory(path)?;
                                    ResourceAny::try_from_resource(directory, &mut access)
                                        .map(Val::Resource)
                                })?,
                            });
                        }
                        let mut answer = [Val::Bool(false)];
                        create
                            .call_concurrent(accessor, &values, &mut answer)
                            .await?;
                        Ok(answer[0].clone())
                    })
                });
                // (A `create` that may refuse says why in words.)
                let state = match state {
                    Some(Val::Result(Ok(made))) => made.map(|made| *made),
                    Some(Val::Result(Err(refused))) => {
                        return Err(match refused.as_deref() {
                            Some(Val::String(why)) => why.clone(),
                            _ => "`create` refused".to_string(),
                        });
                    }
                    other => other,
                };
                let said = failed.get_untracked();
                state
                    .and_then(|state| state.to_wave().ok())
                    .ok_or_else(|| said.unwrap_or_else(|| "`create` gave no state".to_string()))
            });
        failed.dispose();
        state
    }
}

impl Extension {
    /// Carries out the extension's own command `name` in an instance made
    /// for it, which is what comes back: it is the command's, and what
    /// the command put over the workspace goes with it.
    pub fn run_command(
        self: &Arc<Self>,
        name: &str,
        asks: Asks,
        wake: Wake,
        failed: RwSignal<Option<String>>,
    ) -> Result<Running, String> {
        if !self.commands.iter().any(|command| command.name == name) {
            return Err(format!("`{}` has no command `{name}`", self.name));
        }
        let running = self.instantiate(asks, wake, failed)?;
        let named = name.to_string();
        let ran = running.loaded.call(|plugin, accessor| {
            Box::pin(async move {
                let command = function(plugin, accessor, Some("commands"), &named)?;
                command.call_concurrent(accessor, &[], &mut []).await
            })
        });
        match ran {
            Some(()) => Ok(running),
            None => {
                Err((failed.get_untracked())
                    .unwrap_or_else(|| format!("`{name}` was not carried out")))
            }
        }
    }
}

/// An extension running: for a pane, or for a command of its own.
pub struct Running {
    extension: Arc<Extension>,
    loaded: Rc<Loaded>,
}

impl Running {
    /// Builds the view on `state`, the text a pane kept (none for a new
    /// pane): its root node.
    pub fn mount(&self, state: Option<&str>) -> Result<u64, String> {
        let given = match (&self.extension.state, state) {
            // (Its panes keep nothing: `mount` takes nothing.)
            (None, _) => Vec::new(),
            (Some(kept), Some(text)) => {
                let value = Val::from_wave(kept, text)
                    .map_err(|error| format!("the state kept, {text:?}: {error}"))?;
                vec![Val::Option(Some(Box::new(value)))]
            }
            (Some(_), None) => vec![Val::Option(None)],
        };
        let root = self.loaded.call(|plugin, accessor| {
            Box::pin(async move {
                let mount = function(plugin, accessor, Some("pane"), "mount")?;
                let mut answer = [Val::U64(0)];
                (mount.call_concurrent(accessor, &given, &mut answer)).await?;
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
        self.extension.state.as_ref()?;
        let value = self.loaded.call(|plugin, accessor| {
            Box::pin(async move {
                let state = function(plugin, accessor, Some("pane"), "state")?;
                let mut answer = [Val::Bool(false)];
                state.call_concurrent(accessor, &[], &mut answer).await?;
                Ok(answer[0].clone())
            })
        })?;
        value.to_wave().ok()
    }

    /// Carries out the pane's command named `name`; `false` when there is
    /// none.
    pub fn run(&self, name: &str) -> bool {
        let commands = &self.extension.pane_commands;
        if !commands.iter().any(|command| command.name == name) {
            return false;
        }
        let name = name.to_string();
        let ran = self.loaded.call(|plugin, accessor| {
            Box::pin(async move {
                let command = function(plugin, accessor, Some("pane-commands"), &name)?;
                command.call_concurrent(accessor, &[], &mut []).await
            })
        });
        ran.is_some()
    }

    /// Has the extension follow the page: its work after each layout, its
    /// effects when the page's run.
    pub fn follow(&self) {
        self.loaded.follow();
    }

    /// Whether it has stopped: trapped, or ended.
    pub fn stopped(&self) -> bool {
        self.loaded.gone()
    }
}

/// The function `name` the plugin exports, inside the interface `within`
/// if one is given.
fn function(
    plugin: &Plugin,
    accessor: &Accessor<State>,
    within: Option<&str>,
    name: &str,
) -> wasmtime::Result<wasmtime::component::Func> {
    let instance = &plugin.instance;
    let none = |name: &str| wasmtime::Error::msg(format!("no `{name}`"));
    accessor.with(|mut access| {
        let within = match within {
            Some(within) => Some(
                (instance.get_export_index(&mut access, None, within))
                    .ok_or_else(|| none(within))?,
            ),
            None => None,
        };
        let index = (instance.get_export_index(&mut access, within.as_ref(), name))
            .ok_or_else(|| none(name))?;
        instance
            .get_func(&mut access, index)
            .ok_or_else(|| wasmtime::Error::msg(format!("`{name}` is not a function")))
    })
}

/// The components of the extensions that come with the shell, by their
/// files' names and the packages they are built from.
const OWN: [(&str, &str); 4] = [
    ("horizon_markdown_plugin.wasm", "horizon-markdown-plugin"),
    (
        "horizon_theme_settings_plugin.wasm",
        "horizon-theme-settings-plugin",
    ),
    ("horizon_wiki_plugin.wasm", "horizon-wiki-plugin"),
    ("horizon_sessions_plugin.wasm", "horizon-sessions-plugin"),
];

/// The files of the extensions that come with the shell: each beside the
/// executable, or where cargo builds it for the executable's profile
/// (`target/wasm32-wasip2/<profile>` beside `target/<profile>`). One that
/// is in neither place is said on stderr, with how it is built, and left
/// out: the views it has are then not there.
pub fn own() -> Vec<PathBuf> {
    let Some(beside) = (std::env::current_exe().ok())
        .and_then(|executable| executable.parent().map(Path::to_path_buf))
    else {
        return Vec::new();
    };
    own_in(&beside)
}

/// [`own`], for an executable in `beside`.
pub fn own_in(beside: &Path) -> Vec<PathBuf> {
    let built = (beside.parent().zip(beside.file_name()))
        .map(|(target, profile)| target.join("wasm32-wasip2").join(profile));
    (OWN.iter())
        .filter_map(|(file, package)| {
            let found = [Some(beside.to_path_buf()), built.clone()]
                .into_iter()
                .flatten()
                .map(|directory| directory.join(file))
                .find(|file| file.is_file());
            if found.is_none() {
                eprintln!(
                    "horizon: no {file} beside the executable; build it with \
                     `cargo build -p {package} --target wasm32-wasip2`"
                );
            }
            found
        })
        .collect()
}

/// The extensions whose components are at `files` (the configuration's)
/// and at `own` (the shell's), those that load; what kept one from
/// loading is said on stderr. One the configuration names takes the
/// place of the shell's own of the same name.
pub fn load_all(files: &[String], own: &[PathBuf]) -> Vec<Arc<Extension>> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let configured = files
        .iter()
        .map(|file| match (file.strip_prefix("~/"), &home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(file),
        });
    let mut loaded: Vec<Arc<Extension>> = Vec::new();
    for file in configured.chain(own.iter().cloned()) {
        match Extension::load(&file) {
            Ok(extension) => {
                if loaded.iter().all(|other| other.name != extension.name) {
                    loaded.push(Arc::new(extension));
                }
            }
            Err(error) => eprintln!("horizon extension {}: {error}", file.display()),
        }
    }
    loaded
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
