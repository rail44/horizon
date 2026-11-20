//! A preview: an extension still being written, shown from its
//! component's file in a pane of this shell
//! (`docs/preview-pane-design.md`).
//!
//! The pane shows the extension's view, made on what the pane was given
//! for it, and reads the component again when it is built again or when
//! asked to. The component is compiled off the UI thread; what the pane
//! showed stays until the new one is ready. The extension is this pane's
//! alone: one of its name the shell has loaded stays as it is, and the
//! extension's commands are not carried out here.
//!
//! An extension that traps is let go of, and the pane says what it said.
//! It traps when it takes more memory than [`MEMORY_LIMIT`], or a call
//! into it goes on for longer than [`CALL_LIMIT`].

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set, Update, With, WithUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{El, each_layout, el, relayout};

use crate::background::Background;
use crate::extension::Extension;
use crate::plugin::{Command, Mounted, Plugin as PanePlugin};
use crate::plugin_host::{Asks, Limits};
#[cfg(doc)]
use crate::plugin_host::{CALL_LIMIT, MEMORY_LIMIT};
use crate::wake::Wake;
use crate::watch;

const NO_PREVIEW: &str = "No preview. `horizon new preview <extension.wasm>` shows one here.";

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

/// What a preview pane shows: an extension's component, and what its
/// pane is made from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
pub struct Target {
    pub path: PathBuf,
    /// What the extension's pane is made from, as it was typed.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Where a file among the arguments is a path from.
    #[serde(default)]
    pub directory: PathBuf,
}

/// What a target's path was read as.
type Built = Arc<Extension>;

/// Reads the extension whose component is at `path`.
fn build(path: &std::path::Path) -> Result<Built, String> {
    Extension::load(path).map(Arc::new)
}

/// A target as last read, or why it could not be.
type Read = (Target, u32, Result<Built, String>);

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

/// Where what was read is shown, and what it said if it could not be.
fn stage() -> (El, RwSignal<Option<String>>) {
    let failed = RwSignal::new(None::<String>);
    let root = el("div").class(styles::SHOWN).when(
        move || failed.get().is_some(),
        move || {
            el("div")
                .class(styles::FAILED)
                .text_dyn(move || failed.get().unwrap_or_default())
        },
    );
    (root, failed)
}

/// The view of the extension read for `target`, made on what the target
/// gives for it, or what was said in place of it.
fn shown(built: Result<Built, String>, target: &Target, limits: Limits, wake: Wake) -> El {
    let (root, failed) = stage();
    let mounted = built.and_then(|extension| {
        let name = extension.name;
        let mut given = crate::plugin::Given {
            arguments: target.arguments.clone(),
            named: Default::default(),
            directory: target.directory.clone(),
        };
        let state = extension.created_from(&mut given)?;
        given.done().map_err(|error| format!("`{name}`: {error}"))?;
        let running = extension.instantiate_within(Asks::default(), limits, wake, failed)?;
        let node = running.mount(state.as_deref())?;
        running.follow();
        Ok(node)
    });
    match mounted {
        Ok(node) => _ = root.child(El(NodeId::from_u64(node))),
        Err(error) => failed.update(|failed| _ = failed.get_or_insert(error)),
    }
    root
}

/// A preview pane, empty until it is given a target. `background` is
/// where the file is compiled, `limits` how far the extension may go
/// before it is ended, and `wake` what a change of the file wakes.
pub fn view(
    // What to show: the pane follows it.
    target: Signal<Option<Target>>,
    background: Background,
    limits: Limits,
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
            move || build(&path),
            move |built| {
                // (The pane may be gone by now.)
                state.read.try_set(Some((target, reads, built)));
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
    // What was last put in the pane.
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
                let built = (state.read)
                    .with_untracked(|read| read.as_ref().map(|(_, _, built)| built.clone()));
                let shown = shown(
                    built.unwrap_or_else(|| Err("nothing was read".to_string())),
                    target,
                    limits,
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
        // The pane given the keys has them on itself: they go on to what
        // the plugin shows, whenever the pane was given them. (Not only
        // in the layout after the plugin is placed: the pane given the
        // keys in the frame its plugin is placed in has them only once
        // that frame is painted, after the layout.)
        if let Some(shown) = placed.get()
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
            description: "Read the active preview pane's extension from disk again.",
            destructive: false,
            command: PreviewCommand::Reload,
        }]
    }

    /// An extension's component, and what the extension's pane is made
    /// from.
    fn create(mut given: crate::plugin::Given) -> Result<Option<Target>, String> {
        let path = given.file("an extension's component")?;
        let arguments = std::mem::take(&mut given.arguments);
        let directory = given.directory.clone();
        given.done()?;
        Ok(Some(Target {
            path,
            arguments,
            directory,
        }))
    }

    fn mount(
        self,
        target: Option<Target>,
        _: horizon_ui::Callback<std::convert::Infallible>,
    ) -> Mounted<Option<Target>, PreviewCommand> {
        let (view, preview) = view(
            Signal::stored(target.clone()),
            self.background,
            Limits::default(),
            self.wake,
        );
        Mounted {
            view,
            run: horizon_ui::Callback::new(move |PreviewCommand::Reload| preview.reload()),
            enabled: None,
            state: Signal::stored_local(target),
        }
    }
}
