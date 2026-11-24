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
//! it (`horizon new markdown <path>`), and a thread from the list.
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
use horizon_terminal_core::{TerminalNotification, TerminalUpdate};
use horizon_theme_css::{Theme, themed};
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::{LocalStorage, Owner, StoredValue};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{
    Get, GetUntracked, Set, Track, Update, With, WithUntracked, WithValue,
};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callback, El, each_layout, el, relayout};
use horizon_workspace::title::derive_session_title;
use horizon_workspace::{PaneId, SessionId, SessionKind, Side, SplitAxis, Workspace};

use crate::agent::plugin::{AgentAsked, AgentPlugin};
use crate::agent::{Agent, AgentSession};
use crate::board;
use crate::plugin_host::OpenUrl;
use crate::terminal::plugin::TerminalPlugin;
use crate::terminal::{Session, Terminal};
use crate::wake::Wake;
use held::Held;

mod activity;
mod arrangement;
mod board_sessions;
mod chooser;
mod commands;
mod control;
mod extensions;
mod finder;
pub(crate) mod held;
mod hints;
mod keymap;
mod keys;
mod layouts;
mod lent_cursor;
mod links;
mod models;
mod move_to;
mod moving;
mod over;
mod palette;
mod picker;
mod plugins;
mod reload;
mod session_feed;

pub(crate) mod styles {
    horizon_ui::css_module! {"
    /* The workspace over the status bar at the bottom of the window. */
    .window { display: flex; flex-direction: column; height: 100%; }
    .status-place { flex: none; }
    .workspace { position: relative; display: flex; flex-direction: column; flex: 1;
                 min-height: 0;
                 box-sizing: border-box; padding: var(--u8);
                 outline: none; background: var(--background); color: var(--text);
                 font-family: var(--font-family); font-size: var(--body-size);
                 line-height: var(--body-line);
        &.resizing { user-select: none; }
        &.moving { user-select: none; cursor: grabbing; }

        /* A tab group: its strip above, with no frame of its own, and the
           tab it shows in a card under it. The tabs share the strip's
           width, a little apart; those not shown are their words, a faint
           line between two of them, and the one shown is outlined. The
           line where a tab or a view moving would go is drawn between
           two of them, clear of the outline. */
        .tabs { flex: none; display: flex; gap: var(--u4); padding: 0 0 var(--u4); }
        .tab-label { position: relative; flex: 1; min-width: 0; display: flex;
                     padding: 0 var(--u4) 0 var(--u8);
                     border: var(--line) solid transparent; border-radius: var(--radius);
                     font-size: var(--body-size); line-height: calc(var(--u4) * 6);
                     color: var(--text-muted); cursor: pointer; }
        .tab-label:not(.active) + .tab-label:not(.active) {
            border-left-color: var(--surface-chrome); border-radius: 0; }
        .tab-label.active { border-color: var(--border); color: var(--text); }
        .tab-label.active.focused { border-color: var(--accent); }
        .tab-label .title { flex: 1; min-width: 0; overflow: hidden; white-space: nowrap;
                            text-overflow: ellipsis; }
        .tab-label .close { flex: none; width: 16px; text-align: center; border-radius: var(--radius);
                            opacity: 0; }
        .tab-label:hover .close { opacity: 1; }
        .tab-label .close:hover { background: var(--surface-selected); }
        .tabs.cursor .tab-label.active { background: var(--selection); border-color: var(--accent); }
        .tabs.drop { border-radius: var(--radius); background: var(--surface-selected); }
        .tab-label.cursor { background: var(--selection); }
        .tab { flex: 1; min-height: 0; display: flex; }
        .group { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
        .shown-tab { flex: 1; min-width: 0; min-height: 0; display: flex; overflow: hidden;
                     box-sizing: border-box; padding: var(--u2);
                     border: var(--line) solid var(--border); border-radius: var(--radius-large);
                     background: var(--background); }

        /* Cards apart by a gap; the panes of a split in a tab by a line: a
           boundary that is its line alone, taken by its grip, as wide to
           take as a gap is and over the panes on either side. */
        .split { flex: 1; min-width: 0; min-height: 0; display: flex; }
        .split.down { flex-direction: column; }
        .cell { flex-shrink: 1; flex-basis: 0; min-width: 0; min-height: 0; display: flex; }
        .divider { flex: none; }
        .split.across > .divider { width: var(--u8); cursor: col-resize; }
        .split.down > .divider { height: var(--u8); cursor: row-resize; }
        .split.in-tab > .divider { position: relative; }
        .split.in-tab.across > .divider { width: 0; border-left: var(--line) solid var(--border); }
        .split.in-tab.down > .divider { height: 0; border-top: var(--line) solid var(--border); }
        .split.in-tab > .divider > .grip { position: absolute; z-index: 2; }
        .split.in-tab.across > .divider > .grip {
            top: 0; bottom: 0; left: calc((var(--u2) + var(--line)) * -1);
            right: calc(var(--u2) * -1); }
        .split.in-tab.down > .divider > .grip {
            left: 0; right: 0; top: calc((var(--u2) + var(--line)) * -1);
            bottom: calc(var(--u2) * -1); }

        /* A pane: a card of its own in no group, and so is a tab that is one
           pane. The focus is its line in the accent; in workspace mode every
           pane is veiled, the cursor's in the accent. What it shows is kept
           in from its rounded corners. */
        .pane { position: relative; flex: 1; min-width: 0; min-height: 0;
                box-sizing: border-box; display: flex; flex-direction: column;
                overflow: hidden; padding: var(--u4); border: var(--line) solid var(--border);
                border-radius: var(--radius-large); background: var(--background); }
        .pane.focused { border-color: var(--accent); }
        .shown-tab.alone { padding: 0; border: none; border-radius: 0; background: transparent; }
        /* The panes of a split in a tab are parts of its card, with no line
           of their own: the card's and the boundaries' are theirs. The
           focus is drawn just outside the pane, over the line there -- the
           card's at its edge, a boundary's beside it -- the same width
           and so on it at any display scale; rounded at the card's
           corners. What a pane shows is kept in from a boundary as a gap
           keeps it. */
        .shown-tab.parted { padding: 0; overflow: visible; }
        .pane.part { border: none; border-radius: 0; background: transparent;
                     padding: calc(var(--u4) + var(--u2) + var(--line)); }
        .pane.part.focused { z-index: 1; outline: var(--line) solid var(--accent); }
        .pane.part.edge-top { padding-top: var(--u4); }
        .pane.part.edge-right { padding-right: var(--u4); }
        .pane.part.edge-bottom { padding-bottom: var(--u4); }
        .pane.part.edge-left { padding-left: var(--u4); }
        .pane.part.edge-top.edge-left {
            border-top-left-radius: calc(var(--radius-large) - var(--line)); }
        .pane.part.edge-top.edge-right {
            border-top-right-radius: calc(var(--radius-large) - var(--line)); }
        .pane.part.edge-bottom.edge-left {
            border-bottom-left-radius: calc(var(--radius-large) - var(--line)); }
        .pane.part.edge-bottom.edge-right {
            border-bottom-right-radius: calc(var(--radius-large) - var(--line)); }
        .pane > * { flex: 1; min-height: 0; }
        .pane > .veil { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 50;
                        background: var(--background); opacity: 0.5; pointer-events: none; }
        .pane.cursor > .veil { background: var(--accent); opacity: 0.15; }
        .pane > .lifted { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 55;
                          background: var(--background); opacity: 0.5; pointer-events: none; }
        /* Where a view moving would go: a line on the side of a pane it
           would go beside, or between two tabs' labels. */
        .landing { position: absolute; z-index: 55; background: var(--accent);
                   pointer-events: none; }
        .landing.to-left { left: 0; top: 0; bottom: 0; width: var(--line-strong); }
        .landing.to-right { right: 0; top: 0; bottom: 0; width: var(--line-strong); }
        .landing.to-top { left: 0; right: 0; top: 0; height: var(--line-strong); }
        .landing.to-bottom { left: 0; right: 0; bottom: 0; height: var(--line-strong); }
        .tab-label > .landing.to-left {
            left: calc((var(--u2) + var(--line) + var(--line-strong) / 2) * -1); }
        .tab-label > .landing.to-right {
            right: calc((var(--u2) + var(--line) + var(--line-strong) / 2) * -1); }

        /* A pane's tag: its name between the mark it is taken by and the
           button it is closed with, in its corner, while the pointer is
           over the pane or in workspace mode. */
        .pane > .tag { position: absolute; top: var(--u4); right: var(--u4); z-index: 56;
                       flex: none; display: flex; align-items: center; gap: var(--u4);
                       max-width: 50%; padding: 0 var(--u2) 0 var(--u4);
                       border: var(--line) solid var(--surface-chrome); border-radius: var(--radius);
                       background: var(--band); font-size: var(--meta-size);
                       line-height: var(--meta-line); color: var(--text-muted);
                       white-space: nowrap; overflow: hidden; cursor: grab; opacity: 0; }
        /* (It takes a press while it does not show too: the pointer is over
           the pane, and it shows, wherever it can be pressed.) */
        .pane:hover > .tag, .pane > .tag.in-mode { opacity: 1; }
        .pane:hover > .tag.away, .pane > .tag.away { opacity: 0; pointer-events: none; }
        .tag > .tag-name { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
        .tag:hover { color: var(--text); }
        .tag > .tag-close { flex: none; width: var(--u16); text-align: center;
                            border-radius: var(--radius); cursor: pointer; }
        .tag > .tag-close:hover { background: var(--surface-selected); }

        .carried { position: absolute; z-index: 200; padding: var(--u4) var(--u8);
                   border: var(--line) solid var(--border); border-radius: var(--radius);
                   background: var(--surface-raised); box-shadow: var(--shadow-popup);
                   font-size: var(--meta-size); line-height: var(--meta-line);
                   white-space: nowrap; pointer-events: none; }
        .carried.hidden { display: none; }
        .empty { flex: 1; display: flex; align-items: center; justify-content: center;
                 color: var(--text-muted); outline: none; }
        /* What a workspace with no tab shows: the views a tab can show, to
           search and pick from, framed as a card is. */
        .choices { width: 380px; display: flex; overflow: hidden;
                   border: var(--line) solid var(--border); border-radius: var(--radius-large);
                   background: var(--background); color: var(--text); }
        .choices > .picker { width: 100%; max-height: 320px; }

        /* What is over the workspace: a sheet framed as the cards are, what
           is behind it dimmed. */
        .scrim { position: absolute; left: 0; top: 0; right: 0; bottom: 0; z-index: 100;
                 display: flex; justify-content: center; align-items: flex-start;
                 padding-top: 97px; }
        .scrim > .dim-behind { position: absolute; left: 0; top: 0; right: 0; bottom: 0;
                               background: var(--text); opacity: 0.12; pointer-events: none; }
        .sheet { position: relative; display: flex; overflow: hidden; background: var(--background);
                 border: var(--line) solid var(--border); border-radius: var(--radius-large); }
        .picker { width: 560px; max-height: 400px; display: flex; flex-direction: column; }
        .search { flex: none; position: relative; display: flex; align-items: center;
                  height: var(--u32); padding: 0 var(--u12);
                  border-bottom: var(--line) solid var(--border); }
        .search > * { flex: 1; font-size: var(--body-size); line-height: calc(var(--u4) * 5); }
        .items { flex: 1; min-height: 0; overflow-y: auto; padding: var(--u4); }
        .item { display: flex; padding: var(--u4) var(--u8); border-radius: var(--radius);
                cursor: pointer; }
        .item.command { flex-direction: column; }
        .item .name { line-height: calc(var(--u4) * 5); }
        .item .about { font-size: var(--meta-size); line-height: var(--meta-line);
                       color: var(--text-muted); }
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
    let config = AuxiliaryConfig::resolve(
        &small.provider.vendor,
        small.provider.base_url,
        small.provider.api_key_env,
        small.model,
    );
    Some(Arc::new(AuxiliaryClient::new(config.ok()?)))
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
    /// Switches the session's reasoning effort to the level `level` names.
    fn set_effort(&self, session: SessionId, level: &str) -> Result<(), String>;
}

/// What is done on the UI thread with what a thread came back with.
type Arrived = Box<dyn FnOnce(Shell) + Send>;

/// An agent session a runtime has, with what the runtime knows of it.
pub type RunningAgent = crate::plugin::Running;

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

/// What `Reload Terminal Runtime` does with the runtime: has it hand its
/// terminals to its next image, and attaches to every terminal it then
/// runs. Called off the UI thread.
pub type ReloadTerminals = Arc<
    dyn Fn() -> Result<Vec<(uuid::Uuid, horizon_shell_runtime::TerminalSessionHandle)>, String>
        + Send
        + Sync,
>;

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

/// Reads the extensions, off the UI thread: the files the configuration
/// names, then the shell's own (`extension::load_all`).
pub type ReadExtensions =
    Arc<dyn Fn(&[String], &[PathBuf]) -> Vec<Arc<crate::extension::Extension>> + Send + Sync>;

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
    pub open_url: Option<OpenUrl>,
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
    /// The components of the extensions that come with the shell
    /// (`extension::own`), beside the ones the configuration names.
    pub extensions: Vec<PathBuf>,
    /// Reads them, in place of `extension::load_all`: a test's, to hold
    /// the reading until it lets it go.
    pub read_extensions: Option<ReadExtensions>,
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
            extensions: Vec::new(),
            read_extensions: None,
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

/// Where a new pane goes: a new tab, or beside what the cursor is on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Placement {
    NewTab,
    Split(Side),
}

/// The list over everything, when one is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Modal {
    Palette,
    /// Which view the new pane at this place shows.
    Chooser(Placement),
    /// Which model an agent session goes on with.
    Models(SessionId),
    /// Which reasoning effort an agent session goes on with: the model
    /// picker, at its effort step.
    Effort(SessionId),
    /// Which file the new pane of this extension at this place is made
    /// on.
    Finder(&'static str, Placement),
    /// A view an extension put there (`horizon:ui/modal`): this one of
    /// those it has been asked for.
    Extension(u64),
    /// Which of the configuration's layouts to put the workspace in.
    Layouts,
    /// Which tab the pane the cursor is on is moved to.
    MoveTo,
}

/// A view an extension has over the workspace.
struct Over {
    /// Which of those asked for it is.
    asked: u64,
    /// The instance made for a command that put it there, if one did.
    by: Option<u64>,
    view: El,
    /// The sheet made around it.
    sheet: El,
    /// Told once the view is there no longer.
    closed: crate::plugin_host::ModalClosed,
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
    /// The edges of its tab's card it is at, where it is a part of a split
    /// tab (`arrangement`): told the pane itself, as what it is drawn by.
    place: RwSignal<Option<arrangement::Edges>>,
}

/// What the shell keeps beside the model.
struct Kept {
    panes: RefCell<HashMap<PaneId, PaneView>>,
    /// Each tab group's strip, as made: where a view moved with the
    /// pointer would be a new tab.
    strip_elements: RefCell<Vec<(horizon_workspace::types::TabGroupId, El)>>,
    /// The tabs' labels on the page, by their group and place in its
    /// strip: where a view moving can go.
    tab_labels: RefCell<Vec<(El, horizon_workspace::types::TabGroupId, usize)>>,
    /// The sessions of the model, whether a pane shows them or not:
    /// closing a pane lets go of its view and keeps its session.
    sessions: Held<TerminalPlugin>,
    agents: Held<AgentPlugin>,
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
    open_url: OpenUrl,
    /// The drag in progress that a view began: its moves are heard at the
    /// shell's root, wherever the pointer goes.
    drags: horizon_components::Drags,
    notify: Option<Notify>,
    models: Option<Arc<dyn Models>>,
    agent_runtime: Option<AgentRuntime>,
    reload_terminals: Option<ReloadTerminals>,
    reloading: Cell<Option<Reloading>>,
    /// The sessions a board named that this workspace did not have, and
    /// whether the agent runtime has answered about each.
    looked_for: RefCell<HashMap<SessionId, bool>>,
    /// The components of the extensions that come with the shell.
    own_extensions: Vec<PathBuf>,
    /// What reads the extensions.
    read_extensions: ReadExtensions,
    /// The extensions in use for now from files `horizon extension use`
    /// named, by their names: each in place of the one of its name until
    /// it is reset.
    tried_extensions: RefCell<Vec<(&'static str, PathBuf)>>,
    /// The extensions have been read, since the shell was started.
    extensions_read: Cell<bool>,
    /// The extensions whose components are watched.
    watched: RefCell<Vec<extensions::Watched>>,
    /// What plugins are told the workspace's sessions are.
    sessions_feed: Arc<crate::plugin_host::SessionsFeed>,
    /// What plugins are told the workspace's focus and cursor are.
    workspace_feed: Arc<crate::plugin_host::WorkspaceFeed>,
    /// Where the status bar is shown, and what shows it (`hints`).
    status_place: Cell<Option<El>>,
    status_bar: RefCell<Option<hints::Shown>>,
    /// The view an extension has over the workspace, and how many were
    /// asked for.
    over: RefCell<Option<Over>>,
    over_asked: Cell<u64>,
    /// The list a workspace with no tab shows, while it has none.
    choices: Cell<Option<El>>,
    /// The extensions carrying out commands of their own.
    commanded: RefCell<Vec<commands::Commanded>>,
    commanded_count: Cell<u64>,
    /// What the sessions the board views name are doing: every board view
    /// is given it.
    board_activity: RwSignal<HashMap<SessionId, crate::board::activity::BoardSessionActivity>>,
    /// The next arranging gives the active pane the keys.
    refocus: Cell<bool>,
    board: Option<board::Source>,
    /// The configuration in use: what a theme's seed is read from, and
    /// what one being edited replaces the theme of.
    config: RefCell<RawConfig>,
    /// Whether `startup_layout` has been put in, once the extensions were
    /// read.
    startup_layout_applied: Cell<bool>,
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
    /// What is typed into the list over the workspace, and which of its
    /// rows is chosen.
    search: picker::Search,
    /// The extensions' views that hold workspace mode's cursor, and the
    /// cursor of the one with the focus, shown (`lent_cursor`).
    held: RwSignal<Vec<lent_cursor::Held>>,
    lent: RwSignal<Option<lent_cursor::Lent>>,
    /// The boundary being dragged.
    resizing: RwSignal<Option<arrangement::Resize>>,
    /// The view being moved with the pointer.
    moving: RwSignal<Option<moving::Moving>>,
    /// Changed to have the panes arranged again though the tab's shape is
    /// the same: a pane's view was let go of, to be made again.
    rearranged: RwSignal<u32>,
    /// How many times the status bar was asked to be shown again
    /// (`hints`).
    status_asked: RwSignal<u32>,
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
        self.search.clear();
        self.modal.set(Some(modal));
    }

    /// Makes a view for each pane the model has and the page does not,
    /// lets go of the views of panes the model no longer has, and ends the
    /// sessions it no longer has. Doing it again changes nothing.
    fn reconcile(&self) {
        let kept = self.kept();
        let (panes, sessions) = self.model.with_untracked(|model| {
            let panes: Vec<_> = (model.all_pane_ids().into_iter())
                .map(|id| {
                    let session = (model.pane_session_id(id))
                        .and_then(|session| Some((model.session_kind(session)?, session)));
                    (id, session)
                })
                .collect();
            let sessions: HashSet<SessionId> = (model.session_summaries().iter())
                .map(|session| session.id)
                .collect();
            (panes, sessions)
        });
        kept.sessions.keep_only(&sessions);
        kept.agents.keep_only(&sessions);
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
        for (id, session) in panes {
            if kept.panes.borrow().contains_key(&id) {
                continue;
            }
            let view = self.pane(id, session);
            kept.panes.borrow_mut().insert(id, view);
        }
    }

    fn pane(&self, id: PaneId, session: Option<(SessionKind, SessionId)>) -> PaneView {
        let (shell, kept) = (*self, self.kept());
        let owner = kept.owner.child();
        let (mut terminal, mut agent) = (None, None);
        let mut shown_plugin = None;
        let mut bound = None;
        let place = owner.with(|| RwSignal::new(None::<arrangement::Edges>));
        let start = |session| {
            kept.starting
                .borrow_mut()
                .remove(&session)
                .unwrap_or_default()
        };
        let element = owner.with(|| {
            let content = match (session, &kept.spawn_agent) {
                (Some((SessionKind::Terminal, session_id)), _) => {
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
                    let (mounted, view) = TerminalPlugin {
                        session,
                        look: Signal::derive(move || shell.theme.with(Theme::terminal)),
                        open_link: Some(Rc::new(move |link: &str| shell.follow_link(link))),
                        drags: kept.drags.clone(),
                    }
                    .mount_keeping();
                    terminal = Some((session_id, view));
                    let (root, plugin) = self.shown::<TerminalPlugin>(id, mounted);
                    shown_plugin = Some(plugin);
                    root
                }
                // (Its session is attached to again when the runtime is
                // back, and the pane made again.)
                (Some((SessionKind::Agent, _)), _)
                    if kept.reloading.get() == Some(Reloading::Agents) =>
                {
                    el("div")
                        .class(styles::EMPTY)
                        .attr("tabindex", "0")
                        .text("Reloading the agent runtime")
                }
                (Some((SessionKind::Agent, session_id)), Some(spawn)) => {
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
                    let ask = Callback::new(move |asked| match asked {
                        AgentAsked::SwitchModel => shell.switch_model(session_id),
                        AgentAsked::SwitchEffort => shell.switch_effort(session_id),
                    });
                    let (mounted, view) = AgentPlugin { session }.mount_keeping(ask);
                    agent = Some(view);
                    let (root, plugin) = self.shown::<AgentPlugin>(id, mounted);
                    shown_plugin = Some(plugin);
                    root
                }
                (None, _) => {
                    let shown = (self.model).with_untracked(|model| model.pane_plugin(id).cloned());
                    let named = RwSignal::new(Vec::new());
                    let mounted = shown.and_then(|shown| self.mount_plugin(id, &shown, named));
                    match mounted {
                        Some((root, plugin)) => {
                            bound = Some(named);
                            shown_plugin = Some(plugin);
                            root
                        }
                        // (An extension's pane kept from the last run is
                        // made again when the extensions have been read.)
                        None => el("div").class(styles::EMPTY).attr("tabindex", "0").text(
                            match self.extensions_read() {
                                true => "This view's plugin is not here.",
                                false => "Reading this view's plugin.",
                            },
                        ),
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
                .class_dyn(move || place.get().map(|_| styles::PART))
                .class_dyn(move || place.get().filter(|at| at.top).map(|_| styles::EDGE_TOP))
                .class_dyn(move || (place.get().filter(|at| at.right)).map(|_| styles::EDGE_RIGHT))
                .class_dyn(move || {
                    (place.get().filter(|at| at.bottom)).map(|_| styles::EDGE_BOTTOM)
                })
                .class_dyn(move || place.get().filter(|at| at.left).map(|_| styles::EDGE_LEFT))
                .class_dyn(move || {
                    // (While a view's cursor is shown, the pane the view
                    // is in keeps the focus's line: the keys are its.)
                    if shell.lent_shown() {
                        return match shell.lent_cursor_on(id) {
                            true => Some(styles::CURSOR),
                            false => shell.lent_from(id).then_some(styles::FOCUSED),
                        };
                    }
                    match (shell.cursor.get() == Some(id), shell.mode.get()) {
                        (true, true) => Some(styles::CURSOR),
                        (true, false) => Some(styles::FOCUSED),
                        (false, _) => None,
                    }
                })
                .on("pointerdown", move |_| shell.pane_pressed(id))
                .child(content)
                .when(move || shell.veiled(id), || el("div").class(styles::VEIL))
                // Where a view moving would go beside it: a line on that side.
                .when(
                    move || shell.drop_side(id).is_some(),
                    move || {
                        el("div").class(styles::LANDING).class_dyn(move || {
                            shell.drop_side(id).map(|side| match side {
                                Side::Left => styles::TO_LEFT,
                                Side::Right => styles::TO_RIGHT,
                                Side::Up => styles::TO_TOP,
                                Side::Down => styles::TO_BOTTOM,
                            })
                        })
                    },
                )
                // (Faded by what is over it, not by its own opacity: a pane
                // made a stacking context and back is not hit as it was.)
                .when(
                    move || shell.is_moving(id),
                    || el("div").class(styles::LIFTED),
                )
                .child(moving::tag(shell, id))
        });
        PaneView {
            element,
            owner,
            terminal,
            agent,
            plugin: shown_plugin,
            bound,
            place,
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
        // (With no pane, the list the workspace shows has the keys.)
        let choices = (self.model)
            .with_untracked(Workspace::is_empty)
            .then(|| self.kept().choices.get())
            .flatten();
        match (element, choices) {
            (Some(element), _) => element.focus_within(),
            (None, Some(choices)) => choices.focus_within(),
            (None, None) => self.root.focus(),
        };
    }

    /// Takes in what the sessions reported since the last layout: a
    /// title, a working directory, what a session is doing (for a
    /// terminal, what the program in it says of its own state), and that
    /// the shell in a terminal has gone, which ends its session.
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
        let (mut notifications, mut activities) = (Vec::new(), Vec::new());
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
                    TerminalUpdate::ProgramStatus(status) => {
                        let (doing, detail) = status.map(activity::reported).unzip();
                        activities.push((*id, doing, detail.flatten()));
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
            // An agent session is asked what it is doing, and the model
            // told where that is not what it has.
            let (doing, detail) = (activity::of_agent(session.activity()), session.words());
            let known = self.model.with_untracked(|model| {
                (model.session_activity(*id))
                    .is_some_and(|known| known.doing == doing && known.detail == detail)
            });
            if !known {
                activities.push((*id, Some(doing), detail));
            }
        }
        // The session the owner is looking at: the active pane's, and
        // none while the window is away.
        let looked_at = (self.model)
            .with_untracked(Workspace::active_session_id)
            .filter(|_| crate::window::active());
        // A notification is for a session the owner is not looking at.
        if let Some(notify) = kept.notify.as_ref() {
            for (session, notification) in notifications {
                if looked_at != Some(session) {
                    notify(session, notification);
                }
            }
        }
        // What a session came to be doing is news until the session is
        // looked at, which may be now, or at a layout long after.
        let news = looked_at.filter(|looked_at| {
            activities.iter().any(|(session, ..)| session == looked_at)
                || self.model.with_untracked(|model| {
                    (model.session_activity(*looked_at)).is_some_and(|activity| !activity.seen)
                })
        });
        if titles.is_empty()
            && directories.is_empty()
            && exited.is_empty()
            && activities.is_empty()
            && news.is_none()
        {
            return;
        }
        let ended = !exited.is_empty();
        let now_ms = (std::time::SystemTime::now())
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        self.model.update(|model| {
            for (session, title) in titles {
                model.set_session_derived_title(session, title.as_deref());
            }
            for (session, directory) in directories {
                model.set_session_directory(session, directory);
            }
            for (session, doing, detail) in activities {
                model.set_session_activity(session, doing, detail, now_ms);
            }
            if let Some(session) = news {
                model.mark_session_seen(session);
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
            self.show_session(session);
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
    /// focus. A split goes beside what the cursor is on.
    fn create(&self, kind: SessionKind, role: Option<RoleId>, placement: Placement) {
        let kept = self.kept();
        self.change(|model| {
            let from = model.active_session_id();
            let made = Some(match placement {
                Placement::NewTab => model.new_tab_at_cursor_with_new_session(kind),
                Placement::Split(side) => model.split_cursor_with_new_session(kind, side, true),
            });
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
        if self.kept().spawn_agent.is_some() || !plugins::extensions().is_empty() {
            self.open(Modal::Chooser(placement));
        } else {
            self.create(SessionKind::Terminal, None, placement);
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

    /// Opens the model picker at its effort step, if there is a runtime to
    /// ask.
    fn switch_effort(&self, session: SessionId) {
        if self.kept().models.is_some() {
            self.open(Modal::Effort(session));
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

    /// Whether the extensions have been read since the shell was
    /// started: they are read once it is drawn, and a pane of one is
    /// there to make from then.
    pub fn extensions_read(&self) -> bool {
        self.kept().extensions_read.get()
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
            (model.tab_panes(pane).into_iter()).find(|pane| {
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
    /// Brings a session in front of its owner, with the keys: its pane,
    /// or a pane of it beside the active one.
    pub(super) fn show_session(&self, session: SessionId) {
        self.change(|model| {
            model.show_session(session);
        });
    }

    /// Ends a session; `false` for one the workspace does not have. The
    /// focus stays where it is, which may be under an open list.
    pub(super) fn terminate_session(&self, session: SessionId) -> bool {
        let mut ended = false;
        self.model_changed(|model| ended = model.terminate_session(session));
        ended
    }

    /// Ends a session and every session derived from it.
    pub(super) fn terminate_session_with_derived(&self, session: SessionId) {
        self.model_changed(|model| {
            model.terminate_session_with_derived(session);
        });
    }

    /// A new pane at `placement` showing the plugin `plugin`.
    pub fn create_plugin(&self, plugin: horizon_workspace::PluginPane, placement: Placement) {
        self.change(|model| {
            match placement {
                Placement::NewTab => _ = model.new_tab_at_cursor_with_view(plugin),
                Placement::Split(side) => _ = model.split_cursor_with_view(plugin, side),
            }
            model.exit_workspace_mode();
        });
    }

    /// The agent view a pane shows.
    pub fn agent(&self, pane: PaneId) -> Option<Agent> {
        self.kept().panes.borrow().get(&pane)?.agent
    }

    /// The node a pane is drawn in, while it is drawn.
    pub fn pane_element(&self, pane: PaneId) -> Option<horizon_ui::blitz_dom::NodeId> {
        Some(self.kept().panes.borrow().get(&pane)?.element.0)
    }

    /// The terminal a pane shows.
    /// A runtime is being reloaded: asked for, and not yet back with its
    /// sessions attached again.
    pub fn reloading_a_runtime(&self) -> bool {
        self.kept().reloading.get().is_some()
    }

    pub fn terminal(&self, pane: PaneId) -> Option<Terminal> {
        let kept = self.kept();
        let panes = kept.panes.borrow();
        let (_, terminal) = panes.get(&pane)?.terminal.as_ref()?;
        Some(terminal.clone())
    }

    /// Takes a configuration in: its theme, its font and size, its
    /// chords, in place of the ones in use.
    pub fn apply_config(&self, config: &RawConfig) {
        crate::window::set_animation_every_nth_refresh(config.ui.animation_every_nth_refresh);
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
        self.reload_extensions(config.extensions.clone());
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

/// Builds the shell over `opening`, in its configuration's theme; `spawn`
/// starts the session of each terminal pane that has none running.
pub fn view(spawn: Spawn, opening: Opening) -> (El, Shell) {
    // (The extensions are read once the shell is drawn, off the UI
    // thread: reading one is compiling it.)
    plugins::take_in(Vec::new());
    let configured = opening.config.extensions.clone();
    let model = RwSignal::new(opening.model);
    let theme = Theme::from_config(&opening.config);
    crate::window::set_animation_every_nth_refresh(opening.config.ui.animation_every_nth_refresh);
    let configured_size = Cell::new(theme.font_size);
    let sessions = Held::default();
    (sessions.borrow_mut())
        .extend((opening.sessions.into_iter()).map(|(id, session)| (id, Rc::new(session))));
    let agents = Held::default();
    (agents.borrow_mut())
        .extend((opening.agent_sessions.into_iter()).map(|(id, session)| (id, Rc::new(session))));
    let shell = Shell {
        model,
        theme: RwSignal::new(theme),
        mode: Memo::new(move |_| model.with(Workspace::is_workspace_mode_active)),
        cursor: Memo::new(move |_| model.with(Workspace::cursor_pane_id)),
        modal: RwSignal::new(None),
        search: picker::Search::new(),
        held: RwSignal::new(Vec::new()),
        lent: RwSignal::new(None),
        resizing: RwSignal::new(None),
        moving: RwSignal::new(None),
        rearranged: RwSignal::new(0),
        status_asked: RwSignal::new(0),
        kept: StoredValue::new_local(Rc::new(Kept {
            panes: RefCell::default(),
            strip_elements: RefCell::default(),
            tab_labels: RefCell::default(),
            sessions,
            agents,
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
            drags: Default::default(),
            notify: opening.notify,
            models: opening.models,
            agent_runtime: opening.agent_runtime,
            reload_terminals: opening.reload_terminals,
            reloading: Cell::new(None),
            looked_for: RefCell::default(),
            own_extensions: opening.extensions,
            read_extensions: (opening.read_extensions)
                .unwrap_or_else(|| Arc::new(crate::extension::load_all)),
            tried_extensions: RefCell::default(),
            extensions_read: Cell::new(false),
            watched: RefCell::default(),
            sessions_feed: Arc::default(),
            workspace_feed: Arc::default(),
            status_place: Cell::new(None),
            status_bar: RefCell::new(None),
            over: RefCell::default(),
            over_asked: Cell::new(0),
            choices: Cell::new(None),
            commanded: RefCell::default(),
            commanded_count: Cell::new(0),
            board_activity: RwSignal::new(HashMap::new()),
            refocus: Cell::new(false),
            board: opening.board,
            config: RefCell::new(opening.config.clone()),
            startup_layout_applied: Cell::new(false),
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
    shell.feed_sessions();
    shell.feed_workspace();
    shell.follow_status_bar();
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

    // A drag a view began (a terminal's selection) goes on wherever the
    // pointer is.
    let root = shell.kept().drags.wire(root);
    root.attr("tabindex", "0")
        .class(styles::WORKSPACE)
        .class_dyn(move || {
            (
                shell
                    .resizing
                    .with(Option::is_some)
                    .then_some(styles::RESIZING),
                shell.moving_any().then_some(styles::MOVING),
            )
        })
        // On the keys' way in: a view with the focus, a terminal above
        // all, would otherwise have the shell's chords before the shell.
        .on_capture("keydown", move |event| keys::pressed(shell, event))
        .on("pointermove", move |event| {
            arrangement::dragged(shell, event);
            moving::moved(shell, event);
        })
        .on("pointerup", move |_| {
            if shell.resizing.with_untracked(Option::is_some) {
                shell.resizing.update(|resizing| *resizing = None);
            }
            moving::released(shell);
        })
        .child(arrangement::view(shell))
        .child(moving::carried(shell))
        .each(
            // (An extension's view is put in a sheet made around it when it
            // is asked for: the view is the extension's to remove.)
            move || {
                let own = |modal: &Modal| !matches!(modal, Modal::Extension(_));
                shell
                    .modal
                    .get()
                    .into_iter()
                    .filter(own)
                    .collect::<Vec<_>>()
            },
            |modal| *modal,
            move |modal| {
                let inside = match modal {
                    Modal::Palette => palette::view(shell),
                    Modal::Chooser(placement) => chooser::view(shell, shell.search, *placement),
                    Modal::Finder(plugin, placement) => finder::view(shell, plugin, *placement),
                    Modal::Models(session) => match shell.kept().models.clone() {
                        Some(source) => models::view(shell, *session, source, false),
                        None => el("div"),
                    },
                    Modal::Effort(session) => match shell.kept().models.clone() {
                        Some(source) => models::view(shell, *session, source, true),
                        None => el("div"),
                    },
                    Modal::Extension(_) => el("div"),
                    Modal::Layouts => layouts::view(shell),
                    Modal::MoveTo => move_to::view(shell),
                };
                over::view(shell, inside)
            },
        );
    over::follow_extensions(shell);
    shell.follow_lent_cursor();
    shell.focus_active();
    shell.reload_extensions(configured);
    horizon_ui::each_effects(move || shell.follow_extensions());
    horizon_ui::each_effects(move || shell.let_go_of_commanded());
    let place = el("div").class(styles::STATUS_PLACE);
    shell.kept().status_place.set(Some(place));
    let window = el("div")
        .class(styles::WINDOW)
        .child(shell.say_own_keys(root))
        .child(place);
    (themed(move || shell.theme.get(), window), shell)
}
