//! The shell around the views: the tab strip, the active tab's panes in
//! their splits, workspace mode, and the palette of commands.
//!
//! The model is `horizon-workspace`'s:
//! every operation here is one of its operations, and what is on screen
//! follows it. A pane's element is made once, when the model first has the
//! pane, and lives until the model lets the pane go. Splitting, closing,
//! resizing and changing tab change only the boxes around it.
//!
//! A pane shows a terminal, an agent, a Markdown file, a page of the
//! project wiki, the theme settings, or a plugin's view (the board's task
//! list, or one task's thread). Creating one asks which,
//! in a shell with an agent runtime; a Markdown file is opened by naming
//! it (`horizon markdown <path>`), and a thread from the list.
//!
//! Closing a pane lets go of its view and keeps its session, which the
//! session manager lists and puts back in a pane; a session ends when it
//! is terminated, or when the shell in it goes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::Waker;

use horizon_agent::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
use horizon_agent::roles::RoleId;
use horizon_config::RawConfig;
use horizon_shell_runtime::AgentCommand;
use horizon_terminal_core::{TerminalCommand, TerminalNotification, TerminalUpdate};
use horizon_theme_css::{Theme, themed};
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::{LocalStorage, Owner, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{
    Get, GetUntracked, Set, Track, Update, With, WithUntracked, WithValue,
};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{El, each_layout, el, relayout, stop_propagation};
use horizon_workspace::title::derive_session_title;
use horizon_workspace::types::TabSummary;
use horizon_workspace::{PaneId, PaneKind, SessionId, SessionKind, SplitAxis, Workspace};

use crate::agent::{self, Agent, AgentSession};
use crate::board;
use crate::markdown;
use crate::terminal::{self, Session, Terminal, TerminalView};
use crate::wake::Wake;

mod arrangement;
mod board_sessions;
mod chooser;
mod commands;
mod control;
mod keymap;
mod keys;
mod models;
mod palette;
mod picker;
mod plugins;
mod reload;
mod sessions;

pub(crate) mod styles {
    horizon_ui::css_module! {"
    .workspace { position: relative; display: flex; flex-direction: column; height: 100%;
                 outline: none; background: var(--background); color: var(--text);
                 font-family: var(--font-family); font-size: var(--body-size);
                 line-height: var(--body-line);
        &.resizing { user-select: none; }
        .tabs { flex: none; display: flex; gap: var(--u4); padding: var(--u4) var(--u28) var(--u4) var(--u8);
                background: var(--tab-track); border-top: var(--line) solid var(--border);
                border-bottom: var(--line) solid var(--border); }
        .tab-label { flex: 1; min-width: 0; display: flex; padding: 0 var(--u4) 0 var(--u8);
                     border-radius: var(--radius-large); line-height: var(--u24); color: var(--tab-text);
                     cursor: pointer; }
        .tab-label.active { background: var(--background); color: var(--text); }
        .tab-label .title { flex: 1; min-width: 0; text-align: center; overflow: hidden;
                            white-space: nowrap; text-overflow: ellipsis; }
        .tab-label .close { flex: none; width: 16px; text-align: center; border-radius: var(--radius);
                            opacity: 0; }
        .tab-label:hover .close { opacity: 1; }
        .tab-label .close:hover { background: var(--surface-selected); }
        .tab { flex: 1; min-height: 0; display: flex; }
        .split { flex: 1; min-width: 0; min-height: 0; display: flex; }
        .split.down { flex-direction: column; }
        .cell { flex-shrink: 1; flex-basis: 0; min-width: 0; min-height: 0; display: flex; }
        .divider { flex: none; background: var(--surface-chrome); }
        .split.across > .divider { width: 5px; cursor: col-resize; }
        .split.down > .divider { height: 5px; cursor: row-resize; }
        .pane { position: relative; flex: 1; min-width: 0; min-height: 0;
                box-sizing: border-box; display: flex; flex-direction: column;
                overflow: hidden; border: var(--line-strong) solid var(--background);
                background: var(--background); }
        .pane.focused { border-color: var(--border); }
        .pane.cursor { border-color: var(--accent); }
        .pane > * { flex: 1; min-height: 0; }
        .pane > .veil { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 50;
                        background: var(--background); opacity: 0.5; pointer-events: none; }
        .empty { flex: 1; display: flex; align-items: center; justify-content: center;
                 color: var(--text-muted); outline: none; }
        .scrim { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 100;
                 display: flex; justify-content: center; align-items: flex-start;
                 padding-top: 97px; }
        .picker { width: 560px; max-height: 400px; display: flex; flex-direction: column;
                  overflow: hidden; background: var(--background);
                  border: var(--line) solid var(--border); border-radius: 8px;
                  box-shadow: var(--shadow-modal); }
        .search { flex: none; position: relative; display: flex; align-items: center;
                  height: 32px; padding: 0 var(--u12); border-bottom: var(--line) solid var(--border); }
        .search > * { flex: 1; font-size: var(--body-size); line-height: calc(var(--u4) * 5); }
        .items { flex: 1; min-height: 0; overflow-y: auto; }
        .item { display: flex; padding: 7px var(--u12); cursor: pointer; }
        .item.command { flex-direction: column; }
        .item.session { align-items: baseline; gap: var(--u8); }
        .item .name { line-height: calc(var(--u4) * 5); }
        .item .about, .item .attached, .item .detached {
            font-size: var(--meta-size); line-height: var(--meta-line);
            color: var(--text-muted); }
        .item .attached { color: var(--success); }
        .item.destructive .name { color: var(--danger); }
        .item.disabled { cursor: default; }
        .item.disabled .name { color: var(--text-muted); }
        .item.chosen { background: var(--surface-selected); }
    }
"}
}

/// The page's style sheet: the theme's alone. A view's own comes with
/// the view (`horizon_ui::css_module!`).
pub fn css() -> &'static str {
    horizon_theme_css::CSS
}

pub use keymap::Keymap;

/// A session the model has just made, to start. A terminal's shell
/// begins where the terminal it was made from is, or in a directory named
/// for it; an agent works in the directory named, in the role named.
pub struct Starting {
    pub session: SessionId,
    pub from: Option<SessionId>,
    pub directory: Option<PathBuf>,
    pub role: Option<RoleId>,
}

use horizon_theme::spec::{FONT_SIZE_STEP, MAX_FONT_SIZE, MIN_FONT_SIZE};

/// What summarizes a first message into a title under `config`.
fn summarizer(config: &RawConfig) -> Option<Arc<AuxiliaryClient>> {
    let small = config.resolved_small_model().ok()?;
    Some(Arc::new(AuxiliaryClient::new(AuxiliaryConfig {
        base_url: small.provider.base_url,
        api_key_env: small.provider.api_key_env,
        model: small.model,
    })))
}

/// Hands a notification a program in a terminal session asked for to the
/// desktop.
pub type Notify = Rc<dyn Fn(SessionId, TerminalNotification)>;

/// Starts a terminal session.
pub type Spawn = Box<dyn Fn(Starting) -> Session>;

/// Starts an agent session.
pub type SpawnAgent = Box<dyn Fn(Starting) -> AgentSession>;

/// The providers and models an agent session can be switched to, as the
/// agent runtime has them. Each call waits for the runtime's answer, and
/// is made off the UI thread.
pub trait Models: Send + Sync {
    fn providers(&self) -> Result<Vec<horizon_acp::ProviderSummary>, String>;
    fn models(&self, provider: &str) -> Result<Vec<String>, String>;
    fn switch(&self, session: SessionId, provider: &str, model: &str) -> Result<(), String>;
}

/// What is done on the UI thread with what a thread came back with.
type Arrived = Box<dyn FnOnce(Shell) + Send>;

/// An agent session a runtime has, with what the runtime knows of it.
pub struct RunningAgent {
    pub session: SessionId,
    pub directory: Option<PathBuf>,
    /// The session it was derived from.
    pub parent: Option<SessionId>,
}

/// What the runtime is asked off the UI thread.
type Asks<Of, Answer> = Arc<dyn Fn(Of) -> Result<Answer, String> + Send + Sync>;

/// The agent runtime beyond starting a session in it: what the reload
/// command and the board need of it.
pub struct AgentRuntime {
    /// Ends the runtime, starts it again and lists its sessions. Called
    /// off the UI thread.
    pub restart: Asks<(), Vec<RunningAgent>>,
    /// Attaches to a session of the runtime as it is now.
    pub attach: Box<dyn Fn(SessionId) -> AgentSession>,
    /// The organizer of the board of the project in a directory, started
    /// if it is not running. Called off the UI thread.
    pub organizer: Asks<PathBuf, RunningAgent>,
    /// A session of the runtime, if it has it. Called off the UI thread.
    pub find: Asks<SessionId, Option<RunningAgent>>,
}

/// What `Reload Terminal Runtime` does with the runtime: ends it, with
/// every terminal in it, and starts it again. Called off the UI thread.
pub type ReloadTerminals = Arc<dyn Fn() + Send + Sync>;

/// The runtime being ended and started again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reloading {
    Agents,
    Terminals,
}

/// Writes a theme's seed where the configuration is kept, and says where.
pub type SaveTheme = Rc<dyn Fn(&horizon_theme::seed::Seed) -> Result<PathBuf, String>>;

/// Keeps the model for the next run.
pub type Save = Box<dyn FnMut(&Workspace)>;

/// Reads the configuration again.
pub type Reload = Box<dyn Fn() -> Result<RawConfig, String>>;

/// What the shell opens with.
pub struct Opening {
    /// The configuration: the theme, the font and its size, the chords.
    pub config: RawConfig,
    /// What `Reload Config` reads the configuration with.
    pub reload: Option<Reload>,
    pub model: Workspace,
    /// The sessions of `model` that are running already.
    pub sessions: Vec<(SessionId, Session)>,
    pub agent_sessions: Vec<(SessionId, AgentSession)>,
    /// What starts an agent session: without it this shell has terminals
    /// only.
    pub agents: Option<SpawnAgent>,
    /// Hands a URL to the desktop: a link in a document that is not a
    /// file.
    pub open_url: Option<markdown::OpenUrl>,
    /// Posts a desktop notification: without it none is posted.
    pub notify: Option<Notify>,
    /// What the model picker lists and switches with.
    pub models: Option<Arc<dyn Models>>,
    /// What the reload commands do with their runtimes, and what the
    /// board asks of the agents'.
    pub agent_runtime: Option<AgentRuntime>,
    pub reload_terminals: Option<ReloadTerminals>,
    /// The board's store, in place of the one of the project a board pane
    /// is opened in: a test's.
    pub board: Option<board::Source>,
    /// Writes a theme's seed to the config file, in place of
    /// `horizon-theme`'s own writing of it: a test's.
    pub save_theme: Option<SaveTheme>,
    /// Told the model whenever it has changed, to keep it for the next
    /// run.
    pub save: Option<Save>,
}

impl Default for Opening {
    /// One terminal, kept nowhere, with no configuration.
    fn default() -> Self {
        Self {
            config: RawConfig::default(),
            reload: None,
            model: Workspace::mvp(),
            sessions: Vec::new(),
            agent_sessions: Vec::new(),
            agents: None,
            open_url: None,
            notify: None,
            models: None,
            agent_runtime: None,
            reload_terminals: None,
            board: None,
            save_theme: None,
            save: None,
        }
    }
}

/// How a session not yet started is to begin.
#[derive(Default)]
struct Start {
    /// The session it was made from.
    from: Option<SessionId>,
    directory: Option<PathBuf>,
    role: Option<RoleId>,
}

impl Start {
    fn within(directory: PathBuf) -> Self {
        Self {
            directory: Some(directory),
            ..Self::default()
        }
    }
}

/// Where a new pane goes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Placement {
    NewTab,
    SplitRight,
    SplitDown,
}

/// The list over everything, when one is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Modal {
    Palette,
    Sessions,
    /// Which view the new pane at this place shows.
    Chooser(Placement),
    /// Which model an agent session goes on with.
    Models(SessionId),
}

/// A pane on the page.
struct PaneView {
    element: El,
    /// What the pane's view made: effects that end with the pane.
    owner: Owner,
    terminal: Option<(SessionId, Terminal)>,
    agent: Option<Agent>,
    /// The plugin a plugin view shows.
    plugin: Option<plugins::Shown>,
    /// The sessions a board view says its tasks are bound to.
    bound: Option<RwSignal<Vec<SessionId>>>,
}

/// What the shell keeps beside the model.
struct Kept {
    panes: RefCell<HashMap<PaneId, PaneView>>,
    /// The sessions of the model, whether a pane shows them or not:
    /// closing a pane lets go of its view and keeps its session.
    sessions: RefCell<HashMap<SessionId, Rc<Session>>>,
    agents: RefCell<HashMap<SessionId, Rc<AgentSession>>>,
    spawn_agent: Option<SpawnAgent>,
    /// What summarizes an agent session's first message into its title:
    /// the configuration's `small_model`, if it names a usable one.
    summarizer: RefCell<Option<Arc<AuxiliaryClient>>>,
    /// How the sessions made and not yet started are to begin.
    starting: RefCell<HashMap<SessionId, Start>>,
    keymap: RefCell<Keymap>,
    spawn: Spawn,
    reload: Option<Reload>,
    /// The font size the configuration gives: what `Reset Font Size`
    /// goes back to.
    configured_size: Cell<f32>,
    waker: RefCell<Option<Waker>>,
    /// The same, for what wakes the page from another thread.
    wake: Wake,
    open_url: markdown::OpenUrl,
    notify: Option<Notify>,
    models: Option<Arc<dyn Models>>,
    agent_runtime: Option<AgentRuntime>,
    reload_terminals: Option<ReloadTerminals>,
    reloading: Cell<Option<Reloading>>,
    /// The sessions a board named that this workspace did not have, and
    /// whether the agent runtime has answered about each.
    looked_for: RefCell<HashMap<SessionId, bool>>,
    /// What the sessions the board views name are doing: every board view
    /// is given it.
    board_activity:
        RwSignal<HashMap<SessionId, horizon_board_view::activity::BoardSessionActivity>>,
    /// The next arranging gives the active pane the keys.
    refocus: Cell<bool>,
    board: Option<board::Source>,
    /// The configuration in use: what a theme's seed is read from, and
    /// what one being edited replaces the theme of.
    config: RefCell<RawConfig>,
    save_theme: Option<SaveTheme>,
    /// What threads came back with, to take in on the UI thread.
    arrived: Arc<Mutex<Vec<Arrived>>>,
    owner: Owner,
}

#[derive(Clone, Copy)]
pub struct Shell {
    pub model: RwSignal<Workspace>,
    /// The colors, the font and its size, for every view.
    pub theme: RwSignal<Theme>,
    /// Workspace mode: keys are the shell's, not the focused view's.
    mode: Memo<bool>,
    /// The pane commands act on: the mode's cursor, and outside the mode
    /// the pane with the focus.
    cursor: Memo<Option<PaneId>>,
    modal: RwSignal<Option<Modal>>,
    /// What is typed into the open list, and which of its rows is chosen.
    query: RwSignal<String>,
    chosen: RwSignal<usize>,
    /// The boundary being dragged.
    resizing: RwSignal<Option<arrangement::Resize>>,
    /// Changed to have the panes arranged again though the tab's shape is
    /// the same: a pane's view was let go of, to be made again.
    rearranged: RwSignal<u32>,
    kept: StoredValue<Rc<Kept>, LocalStorage>,
    root: El,
}

impl Shell {
    fn kept(&self) -> Rc<Kept> {
        self.kept.with_value(Rc::clone)
    }

    /// Changes the model, then brings the panes, the sessions and the
    /// focus to it.
    fn change(&self, change: impl FnOnce(&mut Workspace)) {
        self.model.update(change);
        self.reconcile();
        self.focus_active();
    }

    /// [`change`](Self::change) that leaves the focus where it is: under
    /// an open list.
    fn model_changed(&self, change: impl FnOnce(&mut Workspace)) {
        self.model.update(change);
        self.reconcile();
    }

    /// Opens a list over everything, empty of what was typed before.
    fn open(&self, modal: Modal) {
        self.query.set(String::new());
        self.chosen.set(0);
        self.modal.set(Some(modal));
    }

    /// Makes a view for each pane the model has and the page does not,
    /// lets go of the views of panes the model no longer has, and ends the
    /// sessions it no longer has. Doing it again changes nothing.
    fn reconcile(&self) {
        let kept = self.kept();
        let (panes, sessions) = self.model.with_untracked(|model| {
            let panes: Vec<_> = (model.all_pane_ids().into_iter())
                .filter_map(|id| {
                    let session =
                        (model.terminal_session_id(id)).or_else(|| model.agent_session_id(id));
                    Some((id, model.pane_kind(id)?, session))
                })
                .collect();
            let sessions: HashSet<SessionId> = (model.session_summaries().iter())
                .map(|session| session.id)
                .collect();
            (panes, sessions)
        });
        kept.sessions.borrow_mut().retain(|id, session| {
            let known = sessions.contains(id);
            if !known {
                session.send(TerminalCommand::Shutdown);
            }
            known
        });
        kept.agents.borrow_mut().retain(|id, session| {
            let known = sessions.contains(id);
            if !known {
                session.send(AgentCommand::Close);
            }
            known
        });
        let gone: Vec<PaneId> = (kept.panes.borrow().keys())
            .filter(|id| !panes.iter().any(|(pane, ..)| pane == *id))
            .copied()
            .collect();
        for id in gone {
            if let Some(view) = kept.panes.borrow_mut().remove(&id) {
                view.owner.cleanup();
                view.element.remove();
            }
        }
        for (id, kind, session) in panes {
            if kept.panes.borrow().contains_key(&id) {
                continue;
            }
            let view = self.pane(id, kind, session);
            kept.panes.borrow_mut().insert(id, view);
        }
    }

    fn pane(&self, id: PaneId, kind: PaneKind, session: Option<SessionId>) -> PaneView {
        let (shell, kept) = (*self, self.kept());
        let owner = kept.owner.child();
        let (mut terminal, mut agent) = (None, None);
        let mut shown_plugin = None;
        let mut bound = None;
        let start = |session| {
            kept.starting
                .borrow_mut()
                .remove(&session)
                .unwrap_or_default()
        };
        let element = owner.with(|| {
            let content = match (kind, session, &kept.spawn_agent) {
                (PaneKind::Terminal, Some(session_id), _) => {
                    let running = kept.sessions.borrow().get(&session_id).cloned();
                    let session = running.unwrap_or_else(|| {
                        let start = start(session_id);
                        // The terminal runtime knows its own sessions only.
                        let from =
                            (start.from).filter(|from| kept.sessions.borrow().contains_key(from));
                        let session = Rc::new((kept.spawn)(Starting {
                            session: session_id,
                            from,
                            directory: start.directory,
                            role: None,
                        }));
                        (kept.sessions.borrow_mut()).insert(session_id, session.clone());
                        session
                    });
                    if let Some(waker) = kept.waker.borrow().clone() {
                        session.set_waker(waker);
                    }
                    let (root, view) = terminal::build(TerminalView {
                        session,
                        look: Signal::derive(move || shell.theme.with(Theme::terminal)),
                        open_url: Some(kept.open_url.clone()),
                    });
                    terminal = Some((session_id, view));
                    root
                }
                // (Its session is attached to again when the runtime is
                // back, and the pane made again.)
                (PaneKind::Agent, Some(_), _)
                    if kept.reloading.get() == Some(Reloading::Agents) =>
                {
                    el("div")
                        .class(styles::EMPTY)
                        .attr("tabindex", "0")
                        .text("Reloading the agent runtime")
                }
                (PaneKind::Agent, Some(session_id), Some(spawn)) => {
                    let running = kept.agents.borrow().get(&session_id).cloned();
                    let session = running.unwrap_or_else(|| {
                        let start = start(session_id);
                        // It works where the session it was made from is.
                        let directory = start.directory.or_else(|| {
                            let from = start.from?;
                            (self.model).with_untracked(|model| {
                                model.session_directory(from).map(PathBuf::from)
                            })
                        });
                        let directory = directory.or_else(|| std::env::current_dir().ok());
                        let session = Rc::new(spawn(Starting {
                            session: session_id,
                            from: start.from,
                            directory: directory.clone(),
                            role: start.role,
                        }));
                        (kept.agents.borrow_mut()).insert(session_id, session.clone());
                        if let Some(directory) = directory {
                            self.model.update(|model| {
                                model.set_session_directory(session_id, directory);
                            });
                        }
                        session
                    });
                    if let Some(waker) = kept.waker.borrow().clone() {
                        session.set_waker(waker);
                    }
                    session.set_summarizer(kept.summarizer.borrow().clone());
                    let (root, view) = agent::live(session, move || {
                        shell.switch_model(session_id);
                    });
                    agent = Some(view);
                    root
                }
                (PaneKind::View, ..) => {
                    let shown = (self.model).with_untracked(|model| model.pane_plugin(id).cloned());
                    let named = RwSignal::new(Vec::new());
                    let mounted = shown.and_then(|shown| self.mount_plugin(id, &shown, named));
                    match mounted {
                        Some((root, plugin)) => {
                            bound = Some(named);
                            shown_plugin = Some(plugin);
                            root
                        }
                        None => el("div")
                            .class(styles::EMPTY)
                            .attr("tabindex", "0")
                            .text("This view's plugin is not here."),
                    }
                }
                _ => el("div")
                    .class(styles::EMPTY)
                    .attr("tabindex", "0")
                    .text("This view is not shown yet."),
            };
            el("div")
                // The focused pane's border is the theme's quiet line; in
                // workspace mode the cursor's is the accent, and every
                // pane is veiled.
                .class(styles::PANE)
                .class_dyn(
                    move || match (shell.cursor.get() == Some(id), shell.mode.get()) {
                        (true, true) => Some(styles::CURSOR),
                        (true, false) => Some(styles::FOCUSED),
                        (false, _) => None,
                    },
                )
                .on("pointerdown", move |_| shell.pane_pressed(id))
                .child(content)
                .when(move || shell.mode.get(), || el("div").class(styles::VEIL))
        });
        PaneView {
            element,
            owner,
            terminal,
            agent,
            plugin: shown_plugin,
            bound,
        }
    }

    /// A press in a pane takes the focus there, out of workspace mode if
    /// it is on.
    fn pane_pressed(&self, id: PaneId) {
        let (mode, active) = self
            .model
            .with_untracked(|model| (model.is_workspace_mode_active(), model.is_active_pane(id)));
        if mode {
            self.model.update(|model| {
                model.commit_workspace_mode_to(id);
                model.activate_pane(id);
            });
        } else if !active {
            self.model.update(|model| {
                model.activate_pane(id);
            });
        }
    }

    /// Gives the keys to the view of the pane the model has active: the
    /// first thing in it that takes focus. In workspace mode, and with no
    /// pane, they go to the shell itself.
    fn focus_active(&self) {
        let pane = self.model.with_untracked(|model| {
            (!model.is_workspace_mode_active())
                .then(|| model.cursor_pane_id())
                .flatten()
        });
        let element =
            pane.and_then(|id| self.kept().panes.borrow().get(&id).map(|view| view.element));
        match element {
            Some(element) => element.focus_within(),
            None => self.root.focus(),
        };
    }

    /// Takes in what the terminals reported since the last layout: a
    /// title, a working directory, and that the shell in one has gone,
    /// which ends its session.
    fn take_in_sessions(&self) {
        let arrived = std::mem::take(&mut *self.kept().arrived.lock().unwrap());
        if !arrived.is_empty() {
            for arrived in arrived {
                arrived(*self);
            }
            relayout();
        }
        self.tell_boards();
        let (mut titles, mut directories, mut exited) = (Vec::new(), Vec::new(), Vec::new());
        let mut notifications = Vec::new();
        // What a session says of itself is read from the session, whether a
        // pane shows it or not.
        let kept = self.kept();
        let shown: HashSet<SessionId> = (kept.panes.borrow().values())
            .filter_map(|view| view.terminal.as_ref().map(|(session, _)| *session))
            .collect();
        for (id, session) in kept.sessions.borrow().iter() {
            for report in session.take_reports() {
                match report {
                    TerminalUpdate::Exited => exited.push(*id),
                    TerminalUpdate::Notification(notification) => {
                        notifications.push((*id, notification));
                    }
                    // A program's title is shaped as any title is; one
                    // that shapes to nothing is no title.
                    TerminalUpdate::Title(title) => {
                        titles.push((*id, title.as_deref().and_then(derive_session_title)));
                    }
                    TerminalUpdate::WorkingDirectory(directory) => {
                        directories.push((*id, directory));
                    }
                    _ => {}
                }
            }
            // What is for a view has nowhere to go without one.
            if !shown.contains(id) {
                session.take_updates();
            }
        }
        for (id, session) in kept.agents.borrow().iter() {
            if let Some(title) = session.take_title() {
                titles.push((*id, Some(title)));
            }
        }
        // A notification is for a session the owner is not looking at:
        // not the active pane's, or any while the window is away.
        if let Some(notify) = kept.notify.as_ref() {
            let looked_at = (self.model)
                .with_untracked(Workspace::active_terminal_session_id)
                .filter(|_| crate::window::active());
            for (session, notification) in notifications {
                if looked_at != Some(session) {
                    notify(session, notification);
                }
            }
        }
        if titles.is_empty() && directories.is_empty() && exited.is_empty() {
            return;
        }
        let ended = !exited.is_empty();
        self.model.update(|model| {
            for (session, title) in titles {
                model.set_session_derived_title(session, title.as_deref());
            }
            for (session, directory) in directories {
                model.set_session_directory(session, directory);
            }
            for session in exited {
                model.terminate_session(session);
            }
        });
        if ended && self.modal.get_untracked().is_none() {
            self.focus_active();
        }
        // What follows the model is on the page before it is painted.
        relayout();
    }

    /// Brings the pane a session is in to the front: the answer to a
    /// click on its notification. A session no pane shows is left where
    /// it is.
    pub fn reveal_session(&self, session: SessionId) {
        let shown =
            (self.model).with_untracked(|model| model.pane_location_for_session(session).is_some());
        if shown {
            self.go_to_session(session);
        }
    }

    /// Who the sessions wake when they have something to draw.
    pub fn set_waker(&self, waker: Waker) {
        let kept = self.kept();
        for session in kept.sessions.borrow().values() {
            session.set_waker(waker.clone());
        }
        for session in kept.agents.borrow().values() {
            session.set_waker(waker.clone());
        }
        kept.wake.set(waker.clone());
        *kept.waker.borrow_mut() = Some(waker);
    }

    /// A new pane at `placement` over a new session of `kind`, with the
    /// focus. A split is of the active session's pane.
    fn create(&self, kind: SessionKind, role: Option<RoleId>, placement: Placement) {
        let kept = self.kept();
        self.change(|model| {
            let from = model.active_session_id();
            let split = |model: &mut Workspace, axis| {
                model.split_session_with_new_session(from?, kind, axis, true)
            };
            let made = match placement {
                Placement::NewTab => Some(model.open_tab_with_new_session_activated(kind, true)),
                Placement::SplitRight => split(model, SplitAxis::Horizontal),
                Placement::SplitDown => split(model, SplitAxis::Vertical),
            };
            model.exit_workspace_mode();
            if let Some(made) = made {
                let start = Start {
                    from,
                    directory: None,
                    role,
                };
                kept.starting.borrow_mut().insert(made, start);
            }
        });
    }

    /// A new pane at `placement`: of the view chosen, when there is more
    /// than terminals to choose from.
    fn place(&self, placement: Placement) {
        match self.kept().spawn_agent {
            Some(_) => self.open(Modal::Chooser(placement)),
            None => self.create(SessionKind::Terminal, None, placement),
        }
    }

    /// Runs `work` on a thread of its own, and `then` on the UI thread
    /// with what it came back with.
    fn in_background<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(Shell, T) + Send + 'static,
    ) {
        let kept = self.kept();
        let (arrived, wake) = (kept.arrived.clone(), kept.wake.clone());
        std::thread::spawn(move || {
            let came = work();
            (arrived.lock().unwrap()).push(Box::new(move |shell| then(shell, came)));
            wake.wake();
        });
    }

    /// Opens the model picker for an agent session, if there is a runtime
    /// to ask.
    fn switch_model(&self, session: SessionId) {
        if self.kept().models.is_some() {
            self.open(Modal::Models(session));
        }
    }

    /// The agent session the commands act on: the cursor's.
    fn active_agent(&self) -> Option<Rc<AgentSession>> {
        let session = self.model.with_untracked(Workspace::active_session_id)?;
        self.kept().agents.borrow().get(&session).cloned()
    }

    /// The store a board pane made now reads: the one handed in, else
    /// that of the project the active session works in, else the one
    /// this shell was started in.
    fn board_source(&self) -> board::Source {
        if let Some(source) = self.kept().board.clone() {
            return source;
        }
        let directory = self.model.with_untracked(|model| {
            let session = model.active_session_id()?;
            model.session_directory(session).map(PathBuf::from)
        });
        match directory.or_else(|| std::env::current_dir().ok()) {
            Some(directory) => board::Source::Root(directory),
            None => board::Source::Ready(horizon_board::Store::in_memory(Vec::new())),
        }
    }

    /// The name of the plugin a pane shows.
    pub fn plugin(&self, pane: PaneId) -> Option<&'static str> {
        Some(
            self.kept()
                .panes
                .borrow()
                .get(&pane)?
                .plugin
                .as_ref()?
                .plugin,
        )
    }

    /// The commands of the plugin the cursor's pane shows, by name.
    fn cursor_plugin_commands(&self) -> Vec<&'static str> {
        let pane = self.model.with_untracked(Workspace::cursor_pane_id);
        let Some(plugin) = pane.and_then(|pane| self.plugin(pane)) else {
            return Vec::new();
        };
        (plugins::commands().into_iter())
            .filter(|listed| listed.plugin == plugin)
            .map(|listed| listed.name)
            .collect()
    }

    /// Carries out the command `name` of the plugin the cursor's pane
    /// shows; `false` when it has none of that name.
    pub fn execute_plugin(&self, name: &str) -> bool {
        let Some(pane) = self.model.with_untracked(Workspace::cursor_pane_id) else {
            return false;
        };
        let kept = self.kept();
        let panes = kept.panes.borrow();
        let Some(shown) = panes.get(&pane).and_then(|view| view.plugin.as_ref()) else {
            return false;
        };
        // (What the command does may change the panes.)
        let run = shown.clone();
        drop(panes);
        run.run(name)
    }

    /// The pane of `pane`'s tab that shows the plugin `name`.
    fn tab_plugin_pane(&self, pane: PaneId, name: &str) -> Option<PaneId> {
        self.model.with_untracked(|model| {
            (model.tab_pane_kinds(pane).into_iter())
                .map(|(pane, _)| pane)
                .find(|pane| {
                    model
                        .pane_plugin(*pane)
                        .is_some_and(|plugin| plugin.name == name)
                })
        })
    }

    /// Shows `task`'s thread beside the list in `list`'s tab: in the
    /// tab's thread pane if it has one, made again on the task, else in a
    /// split of the list's. The thread takes the focus.
    fn open_thread(&self, list: PaneId, task: u64) {
        let thread = self.tab_plugin_pane(list, plugins::THREAD);
        self.change(|model| {
            model.exit_workspace_mode();
            model.activate_pane(list);
            match thread {
                Some(thread) => {
                    model.set_pane_plugin(thread, plugins::thread_on(task));
                    model.activate_pane(thread);
                }
                None => {
                    model.split_active_tab_with_view(
                        plugins::thread_on(task),
                        SplitAxis::Horizontal,
                    );
                }
            }
        });
        if let Some(thread) = thread {
            self.remake_pane(thread);
        }
    }

    /// From the thread in `thread`, back to the list in its tab.
    fn back_to_board_list(&self, thread: PaneId) {
        if let Some(list) = self.tab_plugin_pane(thread, plugins::LIST) {
            self.change(|model| {
                model.exit_workspace_mode();
                model.activate_pane(list);
            });
        }
    }

    /// Goes to a session's pane, or puts a session no pane shows in one
    /// beside the active pane. A session this workspace does not have is
    /// left alone.
    fn go_to_session(&self, session: SessionId) {
        self.change(|model| {
            match model.pane_location_for_session(session) {
                Some((tab, pane)) => {
                    model.activate_pane_index(tab, pane);
                }
                None => {
                    model.attach_existing_session_to_split_activated(session, true);
                }
            }
            model.exit_workspace_mode();
        });
    }

    /// A new pane at `placement` showing the plugin `plugin`.
    pub fn create_plugin(&self, plugin: horizon_workspace::PluginPane, placement: Placement) {
        self.change(|model| {
            match placement {
                Placement::NewTab => model.open_tab_with_view_activated(plugin, true),
                Placement::SplitRight => {
                    model.split_active_tab_with_view(plugin, SplitAxis::Horizontal)
                }
                Placement::SplitDown => {
                    model.split_active_tab_with_view(plugin, SplitAxis::Vertical)
                }
            };
            model.exit_workspace_mode();
        });
    }

    /// A new pane at `placement` on the overview of the wiki of the
    /// project this shell was started in.
    fn create_wiki(&self, placement: Placement) {
        let page = (std::env::current_dir().ok())
            .and_then(|directory| horizon_wiki::Store::for_dir(&directory))
            .and_then(|store| store.page_path("index").ok());
        if let Some(wiki) = plugins::document(plugins::WIKI, page) {
            self.create_plugin(wiki, placement);
        }
    }

    /// The agent view a pane shows.
    pub fn agent(&self, pane: PaneId) -> Option<Agent> {
        self.kept().panes.borrow().get(&pane)?.agent
    }

    /// The terminal a pane shows.
    pub fn terminal(&self, pane: PaneId) -> Option<Terminal> {
        let kept = self.kept();
        let panes = kept.panes.borrow();
        let (_, terminal) = panes.get(&pane)?.terminal.as_ref()?;
        Some(terminal.clone())
    }

    /// Takes a configuration in: its theme, its font and size, its
    /// chords, in place of the ones in use.
    pub fn apply_config(&self, config: &RawConfig) {
        let theme = Theme::from_config(config);
        let kept = self.kept();
        *kept.config.borrow_mut() = config.clone();
        kept.configured_size.set(theme.font_size);
        *kept.keymap.borrow_mut() = Keymap::from_config(config);
        let summarizer = summarizer(config);
        for session in kept.agents.borrow().values() {
            session.set_summarizer(summarizer.clone());
        }
        *kept.summarizer.borrow_mut() = summarizer;
        self.theme.set(theme);
    }

    /// The font size, `steps` away from what it is: every view's text and
    /// spacing scale with it.
    fn step_font_size(&self, steps: f32) {
        self.theme.update(|theme| {
            theme.font_size =
                (theme.font_size + steps * FONT_SIZE_STEP).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        });
    }

    /// The list open over everything.
    pub fn modal(&self) -> Option<Modal> {
        self.modal.get_untracked()
    }
}

fn strip(shell: Shell) -> El {
    let tabs = Memo::new(move |_| shell.model.with(Workspace::tab_summaries));
    let tab = move |index: usize| tabs.with(|tabs: &Vec<TabSummary>| tabs.get(index).cloned());
    el("div").class(styles::TABS).each(
        move || (0..tabs.with(Vec::len)).collect::<Vec<_>>(),
        |index| *index,
        move |index| {
            let index = *index;
            el("div")
                .class(styles::TAB_LABEL)
                .class_dyn(move || {
                    tab(index)
                        .is_some_and(|tab| tab.active)
                        .then_some(styles::ACTIVE)
                })
                .child(
                    el("div")
                        .class(styles::TITLE)
                        .text_dyn(move || tab(index).map(|tab| tab.title).unwrap_or_default()),
                )
                // Closing a tab detaches the sessions in it.
                .child(
                    el("div")
                        .class(styles::CLOSE)
                        .text("×")
                        .on("click", move |_| {
                            shell.change(|model| {
                                model.exit_workspace_mode();
                                model.close_tab_index(index);
                            });
                            stop_propagation();
                        }),
                )
                .on("click", move |_| {
                    shell.change(|model| {
                        model.exit_workspace_mode();
                        model.activate_tab_index(index);
                    });
                })
        },
    )
}

/// Builds the shell over `opening`, in its configuration's theme; `spawn`
/// starts the session of each terminal pane that has none running.
pub fn view(spawn: Spawn, opening: Opening) -> (El, Shell) {
    let model = RwSignal::new(opening.model);
    let theme = Theme::from_config(&opening.config);
    let configured_size = Cell::new(theme.font_size);
    let sessions = (opening.sessions.into_iter())
        .map(|(id, session)| (id, Rc::new(session)))
        .collect();
    let agents = (opening.agent_sessions.into_iter())
        .map(|(id, session)| (id, Rc::new(session)))
        .collect();
    let shell = Shell {
        model,
        theme: RwSignal::new(theme),
        mode: Memo::new(move |_| model.with(Workspace::is_workspace_mode_active)),
        cursor: Memo::new(move |_| model.with(Workspace::cursor_pane_id)),
        modal: RwSignal::new(None),
        query: RwSignal::new(String::new()),
        chosen: RwSignal::new(0),
        resizing: RwSignal::new(None),
        rearranged: RwSignal::new(0),
        kept: StoredValue::new_local(Rc::new(Kept {
            panes: RefCell::default(),
            sessions: RefCell::new(sessions),
            agents: RefCell::new(agents),
            spawn_agent: opening.agents,
            summarizer: RefCell::new(summarizer(&opening.config)),
            starting: RefCell::default(),
            keymap: RefCell::new(Keymap::from_config(&opening.config)),
            spawn,
            reload: opening.reload,
            configured_size,
            waker: RefCell::default(),
            wake: Wake::default(),
            open_url: opening.open_url.unwrap_or_else(|| Rc::new(|_| {})),
            notify: opening.notify,
            models: opening.models,
            agent_runtime: opening.agent_runtime,
            reload_terminals: opening.reload_terminals,
            reloading: Cell::new(None),
            looked_for: RefCell::default(),
            board_activity: RwSignal::new(HashMap::new()),
            refocus: Cell::new(false),
            board: opening.board,
            config: RefCell::new(opening.config.clone()),
            save_theme: opening.save_theme,
            arrived: Arc::default(),
            owner: Owner::current().expect("an owner"),
        })),
        root: el("div"),
    };
    let root = shell.root;
    shell.reconcile();

    // A change of the model that no handler made (a shell that has gone)
    // reaches the panes and the sessions too.
    Effect::new(move |_| {
        shell.model.track();
        shell.reconcile();
    });
    each_layout(move |_| shell.take_in_sessions());
    // The model is kept as it changes, though not at every step of a
    // boundary being dragged.
    if let Some(mut save) = opening.save {
        Effect::new(move |_| {
            let dragging = shell.resizing.with(Option::is_some);
            shell.model.with(|model| {
                if !dragging {
                    save(model);
                }
            });
        });
    }

    root.attr("tabindex", "0")
        .class(styles::WORKSPACE)
        .class_dyn(move || {
            shell
                .resizing
                .with(Option::is_some)
                .then_some(styles::RESIZING)
        })
        // On the keys' way in: a view with the focus, a terminal above
        // all, would otherwise have the shell's chords before the shell.
        .on_capture("keydown", move |event| keys::pressed(shell, event))
        .on("pointermove", move |event| {
            arrangement::dragged(shell, event)
        })
        .on("pointerup", move |_| {
            if shell.resizing.with_untracked(Option::is_some) {
                shell.resizing.update(|resizing| *resizing = None);
            }
        })
        .child(strip(shell))
        .child(arrangement::view(shell))
        .each(
            move || shell.modal.get().into_iter().collect::<Vec<_>>(),
            |modal| *modal,
            move |modal| match modal {
                Modal::Palette => palette::view(shell),
                Modal::Sessions => sessions::view(shell),
                Modal::Chooser(placement) => chooser::view(shell, *placement),
                Modal::Models(session) => match shell.kept().models.clone() {
                    Some(source) => models::view(shell, *session, source),
                    None => el("div"),
                },
            },
        );
    shell.focus_active();
    (themed(move || shell.theme.get(), root), shell)
}
