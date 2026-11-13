//! `Reload Agent Runtime` and `Reload Terminal Runtime`: one runtime ended
//! and started again, the other left as it is
//! (`docs/terminald-split-design.md`).
//!
//! The agent runtime keeps its sessions, so they are attached to again
//! and their panes drawn again. The terminal runtime's terminals end with
//! it: they leave the workspace before it is ended, and a workspace left
//! empty gets one new terminal.

use std::rc::Rc;

use horizon_ui::reactive_graph::traits::{GetUntracked, Set, Update, WithUntracked};
use horizon_workspace::{PaneKind, SessionId, SessionInventory, SessionKind, Workspace};

use super::{Modal, Reloading, RunningAgent, Shell};

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
    fn remake_panes(&self, kind: PaneKind) {
        let kept = self.kept();
        let panes: Vec<_> = self.model.with_untracked(|model| {
            (model.all_pane_ids().into_iter())
                .filter(|id| model.pane_kind(*id) == Some(kind))
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
        self.remake_panes(PaneKind::Agent);
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
        self.model.update(|model| {
            let inventory = SessionInventory::new(
                sessions_of(model, SessionKind::Terminal),
                running.iter().map(|agent| agent.session).collect(),
            );
            if let Err(error) = model.reconcile_session_inventory(&inventory) {
                eprintln!("failed to bring the workspace to the agent runtime: {error}");
            }
            for agent in running {
                if let Some(directory) = agent.directory {
                    model.set_session_directory(agent.session, directory);
                }
                if let Some(parent) = agent.parent {
                    model.set_session_parent(agent.session, parent);
                }
            }
        });
        let known = (self.model).with_untracked(|model| sessions_of(model, SessionKind::Agent));
        for session in known {
            let attached = Rc::new((reload.attach)(session));
            kept.agents.borrow_mut().insert(session, attached);
        }
        kept.reloading.set(None);
        kept.refocus.set(true);
        self.remake_panes(PaneKind::Agent);
    }

    pub(super) fn reload_terminal_runtime(&self) {
        let kept = self.kept();
        let Some(restart) = kept.reload_terminals.clone() else {
            return;
        };
        if kept.reloading.get().is_some() {
            return;
        }
        kept.reloading.set(Some(Reloading::Terminals));
        // The terminals end with the runtime, which ends them itself.
        kept.sessions.borrow_mut().clear();
        self.change(|model| {
            for session in sessions_of(model, SessionKind::Terminal) {
                model.terminate_session(session);
            }
        });
        self.in_background(
            move || restart(),
            |shell, ()| {
                let kept = shell.kept();
                kept.reloading.set(None);
                kept.refocus.set(true);
                // (The arranging the change brings makes the pane.)
                shell.model.update(|model| {
                    if model.tab_count() == 0 {
                        model.open_tab_with_new_session_activated(SessionKind::Terminal, true);
                    }
                });
            },
        );
    }
}
