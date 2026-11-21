//! `Reload Agent Runtime` and `Reload Terminal Runtime`: one runtime ended
//! and started again, the other left as it is
//! (`docs/terminald-split-design.md`).
//!
//! Either runtime keeps its sessions, so they are attached to again and
//! their panes drawn again: the agent runtime's are its own to keep, the
//! terminal runtime's are handed to its next image
//! (`docs/terminald-handover-design.md`). A workspace the reload leaves
//! with no tab gets one new terminal.

use std::rc::Rc;

use horizon_shell_runtime::TerminalSessionHandle;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Update, WithUntracked};
use horizon_workspace::{SessionId, SessionInventory, SessionKind, Workspace};
use uuid::Uuid;

use super::held::{inventory_of, take_in};
use super::{Modal, Reloading, RunningAgent, Shell};
use crate::agent::plugin::AgentPlugin;
use crate::plugin::Running;
use crate::terminal::Session;
use crate::terminal::plugin::TerminalPlugin;

fn sessions_of(model: &Workspace, kind: SessionKind) -> Vec<SessionId> {
    (model.session_summaries().iter())
        .filter(|session| session.kind == kind)
        .map(|session| session.id)
        .collect()
}

impl Shell {
    /// Lets go of the views of the panes of `kind` and has the panes
    /// arranged again, which makes them anew. (Not here: this is also
    /// called after a layout, when no element can be made.)
    fn remake_panes(&self, kind: SessionKind) {
        let kept = self.kept();
        let panes: Vec<_> = self.model.with_untracked(|model| {
            (model.all_pane_ids().into_iter())
                .filter(|id| {
                    let session = model.pane_session_id(*id);
                    session.and_then(|session| model.session_kind(session)) == Some(kind)
                })
                .collect()
        });
        for id in panes {
            let view = kept.panes.borrow_mut().remove(&id);
            if let Some(view) = view {
                view.owner.cleanup();
                view.element.remove();
            }
        }
        self.rearranged
            .update(|times| *times = times.wrapping_add(1));
    }

    /// Lets go of one pane's view and has the panes arranged again, which
    /// makes it anew.
    pub(super) fn remake_pane(&self, pane: horizon_workspace::PaneId) {
        let view = self.kept().panes.borrow_mut().remove(&pane);
        if let Some(view) = view {
            view.owner.cleanup();
            view.element.remove();
        }
        self.rearranged
            .update(|times| *times = times.wrapping_add(1));
    }

    pub(super) fn reload_agent_runtime(&self) {
        let kept = self.kept();
        let Some(reload) = kept.agent_runtime.as_ref() else {
            return;
        };
        if kept.reloading.get().is_some() {
            return;
        }
        kept.reloading.set(Some(Reloading::Agents));
        // The session the model picker is for, and the list it shows, go
        // with the runtime.
        if matches!(self.modal.get_untracked(), Some(Modal::Models(_))) {
            self.modal.set(None);
        }
        // The sessions are let go of without a word to them: they are
        // the runtime's to keep. Their panes say what is going on.
        kept.agents.borrow_mut().clear();
        kept.looked_for.borrow_mut().clear();
        self.remake_panes(SessionKind::Agent);
        let restart = reload.restart.clone();
        self.in_background(
            move || restart(()),
            |shell, running| shell.agents_reloaded(running),
        );
    }

    fn agents_reloaded(&self, running: Result<Vec<RunningAgent>, String>) {
        let kept = self.kept();
        let Some(reload) = kept.agent_runtime.as_ref() else {
            return;
        };
        let running = match running {
            Ok(running) => running,
            // The panes stay as they are; the command can be given again.
            Err(error) => {
                eprintln!("failed to list agent sessions: {error}");
                kept.reloading.set(None);
                return;
            }
        };
        // The workspace goes by what the runtime has now, as it does when
        // the shell opens.
        // (The terminals are as the workspace has them: their runtime was
        // not asked.)
        self.model.update(|model| {
            let terminals = sessions_of(model, SessionKind::Terminal);
            let inventory = SessionInventory::new(
                (terminals.into_iter())
                    .map(|session| (SessionKind::Terminal, session))
                    .chain(inventory_of::<AgentPlugin>(&running)),
            );
            if let Err(error) = take_in(model, inventory, &running) {
                eprintln!("failed to bring the workspace to the agent runtime: {error}");
            }
        });
        let known = (self.model).with_untracked(|model| sessions_of(model, SessionKind::Agent));
        for session in known {
            let attached = Rc::new((reload.attach)(session));
            kept.agents.borrow_mut().insert(session, attached);
        }
        kept.reloading.set(None);
        kept.refocus.set(true);
        self.remake_panes(SessionKind::Agent);
    }

    pub(super) fn reload_terminal_runtime(&self) {
        let kept = self.kept();
        let Some(reload) = kept.reload_terminals.clone() else {
            return;
        };
        if kept.reloading.get().is_some() {
            return;
        }
        kept.reloading.set(Some(Reloading::Terminals));
        // The panes keep their sessions until the new ones are attached: a
        // pane made in between would start a terminal of its own.
        self.in_background(
            move || reload(),
            |shell, attached| shell.terminals_reloaded(attached),
        );
    }

    fn terminals_reloaded(&self, attached: Result<Vec<(Uuid, TerminalSessionHandle)>, String>) {
        let kept = self.kept();
        let attached = match attached {
            Ok(attached) => attached,
            // The panes are drawn again on what the new connection has
            // of their sessions; the command can be given again.
            Err(error) => {
                eprintln!("failed to list terminal sessions: {error}");
                Vec::new()
            }
        };
        let running: Vec<Running> = (attached.iter())
            .map(|(session, _)| Running {
                session: SessionId::from_uuid(*session),
                directory: None,
                parent: None,
            })
            .collect();
        // The workspace goes by what the runtime runs now, as it does when
        // the shell opens. (The agents are as the workspace has them:
        // their runtime was not asked.)
        self.model.update(|model| {
            let agents = sessions_of(model, SessionKind::Agent);
            let inventory = SessionInventory::new(
                inventory_of::<TerminalPlugin>(&running).chain(
                    agents
                        .into_iter()
                        .map(|session| (SessionKind::Agent, session)),
                ),
            );
            if let Err(error) = take_in(model, inventory, &running) {
                eprintln!("failed to bring the workspace to the terminal runtime: {error}");
            }
        });
        {
            let mut sessions = kept.sessions.borrow_mut();
            sessions.clear();
            for (session, handle) in attached {
                sessions.insert(
                    SessionId::from_uuid(session),
                    Rc::new(Session::attached(handle)),
                );
            }
        }
        kept.reloading.set(None);
        kept.refocus.set(true);
        self.model.update(|model| {
            if model.tab_count() == 0 {
                model.open_tab_with_new_session_activated(SessionKind::Terminal, true);
            }
        });
        self.remake_panes(SessionKind::Terminal);
    }
}
