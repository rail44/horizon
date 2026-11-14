//! A view a pane shows as a plugin: it names the commands it carries out
//! and keeps a state of its own, which the workspace saves under the
//! plugin's name and hands back when the pane is made again.
//!
//! What a built-in view implements here is what an extension's world
//! exports (`docs/extension-boundary-design.md`): `mount` and the state,
//! and a function per command.

use horizon_ui::El;
use horizon_ui::reactive_graph::owner::LocalStorage;
use horizon_ui::reactive_graph::wrappers::read::Signal;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use horizon_ui::Callback;

pub trait Plugin {
    /// What the workspace saves a pane of it under.
    const NAME: &'static str;
    /// A pane's title in the tab strip, unless [`Plugin::title`] says
    /// otherwise.
    const TITLE: &'static str;
    /// What a pane of it keeps across runs.
    type State: Serialize + DeserializeOwned + Clone + PartialEq + 'static;
    /// The commands it carries out.
    type Command: Copy + 'static;
    /// What it asks of whoever shows it.
    type Asked: 'static;

    /// Its commands, in the order the palette lists them.
    fn commands() -> Vec<Command<Self::Command>>;

    /// A pane's title in the tab strip, on `state`.
    fn title(state: &Self::State) -> String {
        let _ = state;
        Self::TITLE.to_string()
    }

    /// A pane's view, on `state`. `ask` hears what the view asks for.
    fn mount(
        self,
        state: Self::State,
        ask: Callback<Self::Asked>,
    ) -> Mounted<Self::State, Self::Command>;
}

/// A command as the palette lists it and a key binding names it.
#[derive(Clone, Debug)]
pub struct Command<C> {
    /// The command's stable kebab-case id.
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub destructive: bool,
    pub command: C,
}

/// A pane's view, the commands it carries out, and the state it keeps.
pub struct Mounted<S: 'static, C: 'static> {
    pub view: El,
    pub run: Callback<C>,
    /// Whether a command can be carried out now; every one can when
    /// `None`.
    pub enabled: Option<Enabled<C>>,
    pub state: Signal<S, LocalStorage>,
}

/// Says whether a command can be carried out now.
pub type Enabled<C> = horizon_ui::reactive_graph::callback::UnsyncCallback<C, bool>;

/// A plugin whose panes show sessions of one kind, which run in a runtime
/// of their own: what the host asks of it to list the sessions the
/// runtime runs and to end one.
pub trait SessionPlugin: Plugin {
    const KIND: horizon_workspace::SessionKind;
    /// A session as the shell holds it.
    type Session;
    /// The runtime the sessions run in.
    type Runtime: ?Sized;

    /// The sessions the runtime runs.
    fn running(runtime: &Self::Runtime) -> Result<Vec<Running>, String>;

    /// Ends a session the shell holds.
    fn end(session: &Self::Session);
}

/// A session a runtime runs, and what the runtime says of it.
#[derive(Clone, Debug)]
pub struct Running {
    pub session: horizon_workspace::SessionId,
    /// Where it works.
    pub directory: Option<std::path::PathBuf>,
    /// The session it was derived from.
    pub parent: Option<horizon_workspace::SessionId>,
}
