//! The shell around the views: the tab strip, the active tab's panes in
//! their splits, workspace mode, and the palette of commands.
//!
//! The model is `horizon-workspace`'s, the one the GPUI shell projects:
//! every operation here is one of its operations, and what is on screen
//! follows it. A pane's element is made once, when the model first has the
//! pane, and lives until the model lets the pane go. Splitting, closing,
//! resizing and changing tab change only the boxes around it.
//!
//! So far a pane shows a terminal, an agent or a Markdown file. Creating
//! one asks which of the first two, when there is an agent runtime to
//! start one with; a Markdown file is opened by naming it
//! (`horizon markdown <path>`).
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
use horizon_terminal_core::{TerminalCommand, TerminalUpdate};
use horizon_theme_css::{Theme, themed};
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::{LocalStorage, Owner, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{
    Get, GetUntracked, Set, Track, Update, With, WithUntracked, WithValue,
};
use horizon_ui::{El, each_layout, el, relayout, stop_propagation};
use horizon_workspace::title::derive_session_title;
use horizon_workspace::types::TabSummary;
use horizon_workspace::{PaneId, PaneKind, SessionId, SessionKind, SplitAxis, ViewKind, Workspace};

use crate::agent::{self, Agent, AgentSession};
use crate::markdown::{self, Markdown};
use crate::terminal::{self, Session, Terminal};
use crate::wake::Wake;

mod arrangement;
mod chooser;
mod commands;
mod control;
mod keymap;
mod keys;
mod models;
mod palette;
mod picker;
mod sessions;

// Every rule sits under the view's own class: the page's style sheet is one
// for all the views on it.
pub const CSS: &str = "
    .workspace { position: relative; display: flex; flex-direction: column; height: 100%;
                 outline: none; background: var(--background); color: var(--text);
                 font-family: var(--font-family); font-size: var(--body-size);
                 line-height: var(--body-line);
        &.resizing { user-select: none; }
        .tabs { flex: none; display: flex; gap: 4px; padding: 4px 28px 4px 8px;
                background: var(--tab-track); border-top: 1px solid var(--border);
                border-bottom: 1px solid var(--border); }
        .tab-label { flex: 1; min-width: 0; display: flex; padding: 0 4px 0 8px;
                     border-radius: 6px; line-height: 24px; color: var(--tab-text);
                     cursor: pointer; }
        .tab-label.active { background: var(--background); color: var(--text); }
        .tab-label .title { flex: 1; min-width: 0; text-align: center; overflow: hidden;
                            white-space: nowrap; text-overflow: ellipsis; }
        .tab-label .close { flex: none; width: 16px; text-align: center; border-radius: 4px;
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
                overflow: hidden; border: 2px solid var(--background);
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
                  border: 1px solid var(--border); border-radius: 8px;
                  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18), 0 2px 8px rgba(0, 0, 0, 0.1); }
        .search { flex: none; position: relative; display: flex; align-items: center;
                  height: 32px; padding: 0 12px; border-bottom: 1px solid var(--border); }
        .search input { flex: 1; font-family: var(--font-family);
                        font-size: var(--body-size); line-height: 20px; padding: 0;
                        border: none; outline: none; background: transparent;
                        color: var(--text); }
        .search .placeholder { position: absolute; left: 12px; top: 6px; line-height: 20px;
                               color: var(--text-muted); pointer-events: none; }
        .items { flex: 1; min-height: 0; overflow-y: auto; }
        .item { display: flex; padding: 7px 12px; cursor: pointer; }
        .item.command { flex-direction: column; }
        .item.session { align-items: baseline; gap: 8px; }
        .item .name { line-height: 20px; }
        .item .about, .item .attached, .item .detached {
            font-size: var(--meta-size); line-height: var(--meta-line);
            color: var(--text-muted); }
        .item .attached { color: var(--success); }
        .item.destructive .name { color: var(--danger); }
        .item.disabled { cursor: default; }
        .item.disabled .name { color: var(--text-muted); }
        .item.chosen { background: var(--surface-selected); }
    }
";

/// The page's style sheet with the theme's, this view's and those of the
/// views its panes show.
pub fn css() -> String {
    [
        horizon_theme_css::CSS,
        CSS,
        terminal::CSS,
        agent::CSS,
        markdown::CSS,
    ]
    .concat()
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

/// The font sizes the font-size commands stay within, as the GPUI shell's
/// do (`src/ui/font.rs`).
const MIN_FONT_SIZE: f32 = 5.;
const MAX_FONT_SIZE: f32 = 40.;

/// What summarizes a first message into a title under `config`.
fn summarizer(config: &RawConfig) -> Option<Arc<AuxiliaryClient>> {
    let small = config.resolved_small_model().ok()?;
    Some(Arc::new(AuxiliaryClient::new(AuxiliaryConfig {
        base_url: small.provider.base_url,
        api_key_env: small.provider.api_key_env,
        model: small.model,
    })))
}

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
    /// What the model picker lists and switches with.
    pub models: Option<Arc<dyn Models>>,
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
            models: None,
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
    markdown: Option<Markdown>,
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
    models: Option<Arc<dyn Models>>,
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
        let (mut terminal, mut agent, mut document) = (None, None, None);
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
                    let look = self.theme.with_untracked(Theme::terminal);
                    let (root, view) = terminal::view(session, look);
                    terminal = Some((session_id, view));
                    root
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
                (PaneKind::View(ViewKind::Markdown), ..) => {
                    let path = (self.model)
                        .with_untracked(|model| model.pane_document(id).map(PathBuf::from));
                    let (root, view) =
                        markdown::view(path, kept.open_url.clone(), kept.wake.clone());
                    // The file it shows is the pane's in the model, so
                    // that it comes back on the same one: a link followed
                    // moves it.
                    Effect::new(move |_| {
                        let Some(path) = view.path.get() else {
                            return;
                        };
                        let kept = shell
                            .model
                            .with_untracked(|model| model.pane_document(id) == Some(&path));
                        if !kept {
                            shell.model.update(|model| {
                                model.set_pane_document(id, path);
                            });
                        }
                    });
                    document = Some(view);
                    root
                }
                _ => el("div")
                    .class("empty")
                    .attr("tabindex", "0")
                    .text("The Blitz shell does not show this view yet."),
            };
            el("div")
                // The focused pane's border is the theme's quiet line; in
                // workspace mode the cursor's is the accent, and every
                // pane is veiled.
                .class_dyn(
                    move || match (shell.cursor.get() == Some(id), shell.mode.get()) {
                        (true, true) => "pane cursor".to_string(),
                        (true, false) => "pane focused".to_string(),
                        (false, _) => "pane".to_string(),
                    },
                )
                .on("pointerdown", move |_| shell.pane_pressed(id))
                .child(content)
                .when(move || shell.mode.get(), || el("div").class("veil"))
        });
        PaneView {
            element,
            owner,
            terminal,
            agent,
            markdown: document,
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
        let (mut titles, mut directories, mut exited) = (Vec::new(), Vec::new(), Vec::new());
        for view in self.kept().panes.borrow().values() {
            let Some((session, terminal)) = &view.terminal else {
                continue;
            };
            // A program's title is shaped as any title is; one that
            // shapes to nothing is no title.
            if let Some(title) = terminal.take_title() {
                titles.push((*session, title.as_deref().and_then(derive_session_title)));
            }
            if let Some(directory) = terminal.take_directory() {
                directories.push((*session, directory));
            }
            if terminal.exited() {
                exited.push(*session);
            }
        }
        // A session no pane shows has no view to take its updates in.
        let kept = self.kept();
        let shown: HashSet<SessionId> = (kept.panes.borrow().values())
            .filter_map(|view| view.terminal.as_ref().map(|(session, _)| *session))
            .collect();
        for (id, session) in kept.sessions.borrow().iter() {
            if shown.contains(id) {
                continue;
            }
            for update in session.take_updates() {
                match update {
                    TerminalUpdate::Exited => exited.push(*id),
                    TerminalUpdate::Title(title) => {
                        titles.push((*id, title.as_deref().and_then(derive_session_title)));
                    }
                    TerminalUpdate::WorkingDirectory(directory) => {
                        directories.push((*id, directory));
                    }
                    _ => {}
                }
            }
        }
        for (id, session) in kept.agents.borrow().iter() {
            if let Some(title) = session.take_title() {
                titles.push((*id, Some(title)));
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

    /// The Markdown view a pane shows.
    pub fn markdown(&self, pane: PaneId) -> Option<Markdown> {
        self.kept().panes.borrow().get(&pane)?.markdown
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
            theme.font_size = (theme.font_size + steps).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
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
    el("div").class("tabs").each(
        move || (0..tabs.with(Vec::len)).collect::<Vec<_>>(),
        |index| *index,
        move |index| {
            let index = *index;
            el("div")
                .class_dyn(move || match tab(index) {
                    Some(tab) if tab.active => "tab-label active".to_string(),
                    _ => "tab-label".to_string(),
                })
                .child(
                    el("div")
                        .class("title")
                        .text_dyn(move || tab(index).map(|tab| tab.title).unwrap_or_default()),
                )
                // Closing a tab detaches the sessions in it.
                .child(el("div").class("close").text("×").on("click", move |_| {
                    shell.change(|model| {
                        model.exit_workspace_mode();
                        model.close_tab_index(index);
                    });
                    stop_propagation();
                }))
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
            models: opening.models,
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
    // The terminals draw themselves: a change of theme is handed to each.
    Effect::new(move |_| {
        let look = shell.theme.with(Theme::terminal);
        for view in shell.kept().panes.borrow().values() {
            if let Some((_, terminal)) = &view.terminal {
                terminal.set_look(look.clone());
            }
        }
    });
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
        .class_dyn(move || match shell.resizing.with(Option::is_some) {
            true => "workspace resizing".to_string(),
            false => "workspace".to_string(),
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
