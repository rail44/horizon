//! The workspace's sessions as plugins are given them (`horizon:ui`'s
//! `sessions` and `session-control`): what the model says of each, said
//! again whenever the model changes, and what a plugin asks done to one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{With, WithUntracked};
use horizon_workspace::lineage::{abbreviate_directory, project_root};
use horizon_workspace::types::{Attention, Doing, SessionActivity, SessionSummary};
use horizon_workspace::{SessionId, Workspace};

use super::Shell;
use crate::plugin_host::{
    Session, SessionActivity as Told, SessionAsk, SessionAsks, SessionAttention, SessionProject,
};

/// The project a session that works in `directory` is in: the repository
/// by the name of its directory, or else the directory by its path.
fn project(directory: &Path) -> SessionProject {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match project_root(directory) {
        Some(root) => SessionProject {
            name: (root.file_name()).map_or_else(
                || root.display().to_string(),
                |name| name.to_string_lossy().into(),
            ),
            root: root.to_string_lossy().into_owned(),
        },
        None => SessionProject {
            name: abbreviate_directory(directory, None, home.as_deref()),
            root: directory.to_string_lossy().into_owned(),
        },
    }
}

fn told(activity: SessionActivity) -> Told {
    Told {
        state: activity.state().to_string(),
        attention: activity.attention().map(|attention| match attention {
            Attention::Approval => SessionAttention::Approval,
            Attention::Input => SessionAttention::Input,
            Attention::Login => SessionAttention::Login,
            Attention::Blocked => SessionAttention::Blocked,
            Attention::Failed => SessionAttention::Failed,
            Attention::Unseen => SessionAttention::Unseen,
        }),
        working: activity.working(),
        app: match &activity.doing {
            Doing::Program(program) => program.app.clone(),
            Doing::Agent(_) => None,
        },
        since_ms: activity.since_ms,
        seen: activity.seen,
        detail: activity.detail,
    }
}

/// A session as a plugin is told of it. A directory's project is found
/// once: finding it reads the directory's ancestors.
fn session(summary: SessionSummary, projects: &mut HashMap<PathBuf, SessionProject>) -> Session {
    let project = summary.directory.as_ref().map(|directory| {
        (projects.entry(directory.clone()))
            .or_insert_with(|| project(directory))
            .clone()
    });
    Session {
        id: summary.id.as_uuid().to_string(),
        kind: summary.kind.label().to_string(),
        title: summary.title,
        attached: summary.attached,
        directory: (summary.directory).map(|directory| directory.to_string_lossy().into_owned()),
        project,
        parent: (summary.parent_session_id).map(|parent| parent.as_uuid().to_string()),
        activity: summary.activity.map(told),
    }
}

impl Shell {
    /// Keeps what plugins are told the sessions are up with the model.
    pub(super) fn feed_sessions(&self) {
        let shell = *self;
        let projects = RefCell::new(HashMap::new());
        Effect::new(move |_| {
            let sessions = shell.model.with(Workspace::session_summaries);
            let mut projects = projects.borrow_mut();
            let sessions = (sessions.into_iter())
                .map(|summary| session(summary, &mut projects))
                .collect();
            shell.kept().sessions_feed.set(sessions);
        });
    }

    /// What this shell does with its sessions for a plugin.
    pub(super) fn session_asks(&self) -> SessionAsks {
        let shell = *self;
        SessionAsks {
            feed: self.kept().sessions_feed.clone(),
            act: Rc::new(move |ask| shell.asked_of_session(ask)),
        }
    }

    /// Does to a session what a plugin asked. A session the workspace
    /// does not have is left alone.
    fn asked_of_session(&self, ask: SessionAsk) {
        let named = |id: &str| id.parse::<uuid::Uuid>().ok().map(SessionId::from_uuid);
        match ask {
            SessionAsk::Show(id) => {
                if let Some(session) = named(&id) {
                    self.show_session(session);
                }
            }
            SessionAsk::Terminate(id) => {
                if let Some(session) = named(&id) {
                    self.terminate_session(session);
                }
            }
            SessionAsk::TerminateWithDerived(id) => {
                if let Some(session) = named(&id) {
                    self.terminate_session_with_derived(session);
                }
            }
            SessionAsk::OpenTerminal(id) => {
                let directory = named(&id).and_then(|session| {
                    (self.model).with_untracked(|model| {
                        model.session_directory(session).map(Path::to_path_buf)
                    })
                });
                if let Some(directory) = directory {
                    self.open_terminal_in(directory);
                }
            }
        }
    }
}

impl Shell {
    /// A new terminal tab whose shell starts in `directory`.
    pub(super) fn open_terminal_in(&self, directory: PathBuf) {
        let kept = self.kept();
        self.change(|model| {
            model.exit_workspace_mode();
            let session = model.open_tab_with_new_session_activated(
                horizon_workspace::SessionKind::Terminal,
                true,
            );
            (kept.starting.borrow_mut()).insert(session, super::Start::within(directory));
        });
    }
}
