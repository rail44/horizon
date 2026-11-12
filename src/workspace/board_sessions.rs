//! The agent sessions the board leads to: its organizer, and the session
//! a task is bound to. Either may be one this workspace does not have
//! yet, which the agent runtime is asked about off the UI thread and
//! which is then taken into the workspace.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use horizon_board_view::activity::BoardSessionActivity;

use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Set, Update, WithUntracked};
use horizon_ui::relayout;
use horizon_workspace::{PaneKind, SessionId, SessionKind, Workspace};

use super::{RunningAgent, Shell};

const NOT_IN_RUNTIME: &str = "The session is not available in the current agent runtime";

/// The session's pane made the active one, or the session put in a pane
/// beside the active one.
fn show(model: &mut Workspace, session: SessionId) {
    match model.pane_location_for_session(session) {
        Some((tab, pane)) => {
            model.activate_pane_index(tab, pane);
        }
        None => {
            model.attach_existing_session_to_split_activated(session, true);
        }
    }
    model.exit_workspace_mode();
}

impl Shell {
    /// Takes a session the agent runtime has into the workspace, detached
    /// if it was not in it, and attaches to it. (Called after a layout:
    /// it makes no element.)
    fn adopt_agent(&self, running: RunningAgent) -> Result<(), String> {
        let kept = self.kept();
        let Some(runtime) = kept.agent_runtime.as_ref() else {
            return Err(NOT_IN_RUNTIME.to_string());
        };
        let session = running.session;
        let other_kind = self.model.with_untracked(|model| {
            (model.session_pane_kind(session)).is_some_and(|kind| kind != PaneKind::Agent)
        });
        if other_kind {
            return Err("the session id belongs to a different session kind".to_string());
        }
        self.model.update(|model| {
            model.register_detached_session(SessionKind::Agent, session);
            // What the runtime says of where it works and what it came
            // from is what holds.
            if let Some(directory) = running.directory {
                model.set_session_directory(session, directory);
            }
            if let Some(parent) = running.parent {
                model.set_session_parent(session, parent);
            }
        });
        if !kept.agents.borrow().contains_key(&session) {
            let attached = Rc::new((runtime.attach)(session));
            kept.agents.borrow_mut().insert(session, attached);
        }
        Ok(())
    }

    /// Tells each board view what the sessions its tasks are bound to are
    /// doing, after a layout. A session this workspace does not have is
    /// asked of the agent runtime once, and taken in detached if it runs
    /// there; until the answer it reads as loading, and after it as out
    /// of reach.
    pub(super) fn tell_boards(&self) {
        let kept = self.kept();
        let mut unknown = Vec::new();
        for view in kept.panes.borrow().values() {
            let (bound, states) = match (&view.board_list, &view.board_thread) {
                (Some(list), _) => (list.bound_sessions(), list.activity),
                (_, Some(thread)) => (thread.bound_sessions(), thread.activity),
                _ => continue,
            };
            let mut told = HashMap::new();
            for session in bound {
                let running = kept
                    .agents
                    .borrow()
                    .get(&session)
                    .map(|agent| agent.activity());
                let activity = running.unwrap_or_else(|| {
                    let answered = kept.looked_for.borrow().get(&session).copied();
                    if answered.is_none() && kept.agent_runtime.is_some() {
                        unknown.push(session);
                    }
                    match (answered, kept.agent_runtime.is_some()) {
                        (Some(true), _) | (_, false) => BoardSessionActivity::Unavailable,
                        _ => BoardSessionActivity::Loading,
                    }
                });
                told.insert(session, activity);
            }
            if states.with_untracked(|states| *states != told) {
                states.set(told);
                relayout();
            }
        }
        unknown.sort_unstable_by_key(|session| session.as_uuid());
        unknown.dedup();
        let Some(runtime) = kept.agent_runtime.as_ref().filter(|_| !unknown.is_empty()) else {
            return;
        };
        for session in &unknown {
            kept.looked_for.borrow_mut().insert(*session, false);
        }
        let find = runtime.find.clone();
        self.in_background(
            move || {
                let found: Vec<_> = (unknown.iter())
                    .filter_map(|session| find(*session).ok().flatten())
                    .collect();
                (unknown, found)
            },
            |shell, (asked, found)| {
                for running in found {
                    if let Err(error) = shell.adopt_agent(running) {
                        eprintln!("a session a board names was not taken in: {error}");
                    }
                }
                let kept = shell.kept();
                for session in asked {
                    kept.looked_for.borrow_mut().insert(session, true);
                }
            },
        );
    }

    /// The organizer of the board the cursor is on, in a pane: started if
    /// it is not running. What goes wrong is said in the board's view.
    pub(super) fn open_board_organizer(&self) {
        let kept = self.kept();
        let Some(pane) = self.model.with_untracked(Workspace::cursor_pane_id) else {
            return;
        };
        let board = (self.board_list(pane))
            .map(|list| (list.root(), list.notice))
            .or_else(|| (self.board_thread(pane)).map(|thread| (thread.root(), thread.notice)));
        let Some((Some(root), notice)) = board else {
            return;
        };
        let Some(runtime) = kept.agent_runtime.as_ref() else {
            return;
        };
        let registered: HashSet<SessionId> = self.model.with_untracked(|model| {
            (model.session_summaries().iter())
                .map(|session| session.id)
                .collect()
        });
        self.model.update(Workspace::commit_workspace_mode);
        let organizer = runtime.organizer.clone();
        self.in_background(
            move || organizer(root),
            move |shell, running| {
                let shown = running.and_then(|running| {
                    let session = running.session;
                    // One ended while this was on its way is not put back.
                    let known = (shell.model)
                        .with_untracked(|model| model.session_pane_kind(session).is_some());
                    if registered.contains(&session) && !known {
                        return Ok(());
                    }
                    shell.adopt_agent(running)?;
                    // The owner went elsewhere meanwhile: the session is
                    // there to attach, and nothing moves.
                    if shell.model.with_untracked(Workspace::cursor_pane_id) != Some(pane) {
                        return Ok(());
                    }
                    shell.kept().refocus.set(true);
                    shell.model.update(|model| show(model, session));
                    Ok(())
                });
                if let Err(error) = shown {
                    notice.set(Some(error));
                }
            },
        );
    }

    /// The session a task is bound to, in a pane. One this workspace does
    /// not have is looked for in the agent runtime; `notice` is where the
    /// thread says what went wrong.
    pub fn open_task_session(&self, session: SessionId, notice: RwSignal<Option<String>>) {
        let kept = self.kept();
        let known = (self.model).with_untracked(|model| model.session_pane_kind(session).is_some());
        let Some(runtime) = kept.agent_runtime.as_ref().filter(|_| !known) else {
            self.go_to_session(session);
            return;
        };
        let find = runtime.find.clone();
        self.in_background(
            move || find(session),
            move |shell, found| {
                let shown = (found.ok().flatten())
                    .ok_or_else(|| NOT_IN_RUNTIME.to_string())
                    .and_then(|running| shell.adopt_agent(running));
                match shown {
                    Ok(()) => {
                        shell.kept().refocus.set(true);
                        shell.model.update(|model| show(model, session));
                    }
                    Err(error) => notice.set(Some(error)),
                }
            },
        );
    }
}
