//! A preview: a view of this shell that is still being written, built
//! into a wasm plugin (`../preview-plugin`) and shown from it.
//!
//! The plugin has the view's signals, effects and handlers; the document
//! is this shell's. What the plugin's `El` and `Page` do arrives here as
//! calls (`horizon-ui`'s `plugin.wit`) and is done in this document, on
//! nodes the plugin made and no others. An event that reaches one of
//! those nodes is told to the plugin, and the plugin's effects run when
//! this shell's do.
//!
//! A pane shows one preview of one plugin file, and reads the file again
//! when it changes or when asked to. The file is compiled off the UI
//! thread; what the pane showed stays until the new one is ready.
//!
//! A plugin that traps is let go of, and the pane says what it said. It
//! traps when it takes more memory than [`MEMORY_LIMIT`], or a call into
//! it goes on for longer than [`CALL_LIMIT`].

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set, Update, With, WithUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{El, each_layout, el, relayout};

/// The bindings of a preview plugin's own exports.
mod own {
    wasmtime::component::bindgen!({
        path: "crates/horizon-ui/wit",
        world: "preview",
        imports: { default: trappable },
        exports: { default: async },
        with: {
            "horizon:ui/document": crate::plugin_host::horizon::ui::document,
            "horizon:ui/desktop": crate::plugin_host::horizon::ui::desktop,
            "horizon:ui/pane": crate::plugin_host::horizon::ui::pane,
            "horizon:ui/files": crate::plugin_host::horizon::ui::files,
        },
    });
}

use crate::background::Background;
use crate::markdown::watch;
use crate::plugin::{Command, Mounted, Plugin as PanePlugin};
use crate::plugin_host::{Asks, Compiled, compile, instantiate, no_grants};
#[cfg(doc)]
use crate::plugin_host::{CALL_LIMIT, MEMORY_LIMIT};
use crate::wake::Wake;

const NO_PREVIEW: &str = "No preview. `horizon preview <plugin.wasm>` shows one here.";

mod styles {
    horizon_ui::css_module! {"
    .preview { height: 100%; min-height: 0; display: flex; flex-direction: column;
               background: var(--background); color: var(--text); }
    .preview > .shown { flex: 1; min-height: 0; display: flex; flex-direction: column; }
    .preview .failed { padding: var(--u16); color: var(--danger);
                       font-size: var(--meta-size); line-height: var(--meta-line);
                       white-space: pre-wrap; }
    .preview > .empty, .preview > .loading {
        padding: var(--u16); color: var(--text-muted);
        font-size: var(--meta-size); line-height: var(--meta-line); }
"}
}

/// What a preview pane shows: a plugin's file, and which of its previews
/// (its first, with no name).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
pub struct Target {
    pub path: PathBuf,
    pub name: Option<String>,
}

/// The file of a target as last compiled, or why it could not be.
type Read = (Target, u32, Result<Compiled, String>);

/// A preview pane's state, for whoever placed it to drive.
#[derive(Clone, Copy)]
pub struct Preview {
    /// What the pane is given to show: it follows it.
    target: Signal<Option<Target>>,
    /// Moves when the file is to be read again.
    reads: RwSignal<u32>,
    read: RwSignal<Option<Read>>,
}

impl Preview {
    /// Reads the file again.
    pub fn reload(&self) {
        self.reads.update(|reads| *reads += 1);
    }
}

/// One preview of a compiled plugin, or what was said in place of it.
fn shown(compiled: Result<Compiled, String>, name: Option<&str>, wake: Wake) -> El {
    let failed = RwSignal::new(None::<String>);
    let root = el("div").class(styles::SHOWN).when(
        move || failed.get().is_some(),
        move || {
            el("div")
                .class(styles::FAILED)
                .text_dyn(move || failed.get().unwrap_or_default())
        },
    );
    let loaded = compiled.and_then(|compiled| {
        let loaded = instantiate(&compiled, no_grants(), Asks::default(), wake, failed);
        let loaded = loaded.map_err(|error| format!("{error:#}"))?;
        // (The plugin's own exports, beside those every plugin has.)
        let previews = loaded
            .call(|instance, store| Box::pin(async move { own::Preview::new(store, instance) }));
        let previews = previews.ok_or_else(|| "the plugin is not a preview plugin".to_string())?;
        Ok((loaded, Rc::new(previews)))
    });
    let (loaded, previews) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            failed.set(Some(error));
            return root;
        }
    };
    let name = match name {
        Some(name) => Some(name.to_string()),
        None => {
            let previews = previews.clone();
            (loaded.call(|_, store| Box::pin(async move { previews.call_names(store).await })))
                .and_then(|names| names.into_iter().next())
        }
    };
    let Some(name) = name else {
        failed.update(|failed| {
            failed.get_or_insert_with(|| "the plugin has no previews".to_string());
        });
        return root;
    };
    let mounted = {
        let name = name.clone();
        loaded.call(|_, store| Box::pin(async move { previews.call_mount(store, &name).await }))
    };
    match mounted {
        Some(Some(mounted)) => {
            root.child(El(NodeId::from_u64(mounted)));
        }
        Some(None) => failed.set(Some(format!("the plugin has no preview named `{name}`"))),
        None => {}
    }
    loaded.follow();
    root
}

/// A preview pane, empty until it is given a target. `background` is
/// where the file is compiled, and `wake` what a change of the file wakes.
pub fn view(
    // What to show: the pane follows it.
    target: Signal<Option<Target>>,
    background: Background,
    wake: Wake,
) -> (El, Preview) {
    let state = Preview {
        target,
        reads: RwSignal::new(0),
        read: RwSignal::new(None),
    };
    let compiling = background.clone();
    let shown_wake = wake.clone();
    Effect::new(move |_| {
        let (Some(target), reads) = (state.target.get(), state.reads.get()) else {
            return;
        };
        let path = target.path.clone();
        compiling.run(
            move || compile(&path),
            move |compiled| {
                // (The pane may be gone by now.)
                state.read.try_set(Some((target, reads, compiled)));
            },
        );
    });
    // Whether what is shown is not yet what was asked for.
    let reading = move || {
        let asked = (state.target.get()).map(|target| (target, state.reads.get()));
        let read = (state.read).with(|read| {
            read.as_ref()
                .map(|(target, reads, _)| (target.clone(), *reads))
        });
        asked.is_some() && asked != read
    };
    // What was last put in the pane, until the layout after it.
    let arrived = Rc::new(Cell::new(None::<El>));
    let placed = arrived.clone();
    let root = el("div")
        .class(styles::PREVIEW)
        .attr("tabindex", "0")
        .when(
            move || state.target.with(Option::is_none),
            || el("div").class(styles::EMPTY).text(NO_PREVIEW),
        )
        .when(reading, move || {
            el("div").class(styles::LOADING).text_dyn(move || {
                let file = (state.target.get()).and_then(|target| {
                    Some(target.path.file_name()?.to_string_lossy().into_owned())
                });
                format!("Loading {}…", file.unwrap_or_default())
            })
        })
        // What was read last stays until the next is ready.
        .each(
            move || {
                (state.read)
                    .with(|read| {
                        read.as_ref()
                            .map(|(target, reads, _)| (target.clone(), *reads))
                    })
                    .into_iter()
                    .collect::<Vec<_>>()
            },
            Clone::clone,
            move |(target, _)| {
                let compiled = (state.read)
                    .with_untracked(|read| read.as_ref().map(|(_, _, compiled)| compiled.clone()));
                let shown = shown(
                    compiled.unwrap_or_else(|| Err("nothing was read".to_string())),
                    target.name.as_deref(),
                    shown_wake.clone(),
                );
                arrived.set(Some(shown));
                shown
            },
        );
    // The file is watched: a build that replaces it is shown.
    let changed = watch::Changed::default();
    let noted = changed.clone();
    Effect::new(move |_| {
        let path = state.target.get().map(|target| target.path);
        path.and_then(|path| watch::on(path, noted.clone(), wake.clone()))
    });
    each_layout(move |page| {
        // A pane given the keys before its plugin was read has them on
        // itself: they go on to what the plugin shows.
        if let Some(shown) = placed.take()
            && page.has_focus(root)
        {
            shown.focus_within();
        }
        let mut again = background.take_in();
        if changed.settled() {
            state.reload();
            again = true;
        }
        if again {
            relayout();
        }
    });
    (root, state)
}

/// The preview as a plugin: a pane keeps what it shows, and comes back
/// on it.
pub struct PreviewPlugin {
    pub background: Background,
    pub wake: Wake,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewCommand {
    Reload,
}

impl PanePlugin for PreviewPlugin {
    const NAME: &'static str = "preview";
    const TITLE: &'static str = "Preview";
    type State = Option<Target>;
    type Command = PreviewCommand;
    type Asked = std::convert::Infallible;

    fn commands() -> Vec<Command<PreviewCommand>> {
        vec![Command {
            name: "reload-preview",
            title: "Reload Preview",
            description: "Reload the active preview pane's plugin from disk.",
            destructive: false,
            command: PreviewCommand::Reload,
        }]
    }

    fn mount(
        self,
        target: Option<Target>,
        _: horizon_ui::Callback<std::convert::Infallible>,
    ) -> Mounted<Option<Target>, PreviewCommand> {
        let (view, preview) = view(Signal::stored(target.clone()), self.background, self.wake);
        Mounted {
            view,
            run: horizon_ui::Callback::new(move |PreviewCommand::Reload| preview.reload()),
            enabled: None,
            state: Signal::stored_local(target),
        }
    }
}
