//! The shell around the views: the tab strip, the active tab's panes in
//! their splits, workspace mode, and the palette of commands.
//!
//! The model is `horizon-workspace`'s, the one the GPUI shell projects:
//! every operation here is one of its operations, and what is on screen
//! follows it. A pane's element is made once, when the model first has the
//! pane, and lives until the model lets the pane go. Splitting, closing,
//! resizing and changing tab change only the boxes around it.
//!
//! So far a pane shows a terminal. Creating one goes straight to a
//! terminal, where the GPUI shell asks which view first.
//!
//! Closing a pane lets go of its view and keeps its session, which the
//! session manager lists and puts back in a pane; a session ends when it
//! is terminated, or when the shell in it goes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::task::Waker;

use horizon_config::RawConfig;
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
use horizon_workspace::types::TabSummary;
use horizon_workspace::{PaneId, PaneKind, SessionId, Workspace};

use crate::terminal::{self, Session, Terminal};

mod arrangement;
mod commands;
mod keymap;
mod keys;
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
    [horizon_theme_css::CSS, CSS, terminal::CSS].concat()
}

pub use keymap::Keymap;

/// A terminal session the model has just made, to start: its shell
/// begins where the session it was made from is, or in a directory named
/// for it.
pub struct Starting {
    pub session: SessionId,
    pub from: Option<SessionId>,
    pub directory: Option<PathBuf>,
}

/// The font sizes the font-size commands stay within, as the GPUI shell's
/// do (`src/ui/font.rs`).
const MIN_FONT_SIZE: f32 = 5.;
const MAX_FONT_SIZE: f32 = 40.;

/// Starts a terminal session.
pub type Spawn = Box<dyn Fn(Starting) -> Session>;

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
            save: None,
        }
    }
}

/// Where a session not yet started is to begin.
enum Start {
    Beside(SessionId),
    In(PathBuf),
}

/// The list over everything, when one is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Modal {
    Palette,
    Sessions,
}

/// A pane on the page.
struct PaneView {
    element: El,
    /// What the pane's view made: effects that end with the pane.
    owner: Owner,
    terminal: Option<(SessionId, Terminal)>,
}

/// What the shell keeps beside the model.
struct Kept {
    panes: RefCell<HashMap<PaneId, PaneView>>,
    /// The sessions of the model, whether a pane shows them or not:
    /// closing a pane lets go of its view and keeps its session.
    sessions: RefCell<HashMap<SessionId, Rc<Session>>>,
    /// Where the sessions made and not yet started are to begin.
    starting: RefCell<HashMap<SessionId, Start>>,
    keymap: RefCell<Keymap>,
    spawn: Spawn,
    reload: Option<Reload>,
    /// The font size the configuration gives: what `Reset Font Size`
    /// goes back to.
    configured_size: Cell<f32>,
    waker: RefCell<Option<Waker>>,
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
                .filter_map(|id| Some((id, model.pane_kind(id)?, model.terminal_session_id(id))))
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
        let mut terminal = None;
        let element = owner.with(|| {
            let content = match (kind, session) {
                (PaneKind::Terminal, Some(session_id)) => {
                    let session = (kept.sessions.borrow_mut().entry(session_id))
                        .or_insert_with(|| {
                            let start = kept.starting.borrow_mut().remove(&session_id);
                            let (from, directory) = match start {
                                Some(Start::Beside(from)) => (Some(from), None),
                                Some(Start::In(directory)) => (None, Some(directory)),
                                None => (None, None),
                            };
                            Rc::new((kept.spawn)(Starting {
                                session: session_id,
                                from,
                                directory,
                            }))
                        })
                        .clone();
                    if let Some(waker) = kept.waker.borrow().clone() {
                        session.set_waker(waker);
                    }
                    let look = self.theme.with_untracked(Theme::terminal);
                    let (root, view) = terminal::view(session, look);
                    terminal = Some((session_id, view));
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
        let (mut titles, mut directories, mut exited) = (Vec::new(), Vec::new(), Vec::new());
        for view in self.kept().panes.borrow().values() {
            let Some((session, terminal)) = &view.terminal else {
                continue;
            };
            if let Some(title) = terminal.take_title() {
                titles.push((*session, title));
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
                    TerminalUpdate::Title(title) => titles.push((*id, title)),
                    TerminalUpdate::WorkingDirectory(directory) => {
                        directories.push((*id, directory));
                    }
                    _ => {}
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

    /// Who the terminals wake when their shells have printed.
    pub fn set_waker(&self, waker: Waker) {
        let kept = self.kept();
        for session in kept.sessions.borrow().values() {
            session.set_waker(waker.clone());
        }
        *kept.waker.borrow_mut() = Some(waker);
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
            starting: RefCell::default(),
            keymap: RefCell::new(Keymap::from_config(&opening.config)),
            spawn,
            reload: opening.reload,
            configured_size,
            waker: RefCell::default(),
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
            },
        );
    shell.focus_active();
    (themed(move || shell.theme.get(), root), shell)
}
