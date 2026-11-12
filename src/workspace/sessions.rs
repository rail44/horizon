//! The session manager: every session of the workspace, shown or
//! detached, as the tree of what each was derived from. Enter goes to the
//! chosen session's pane, or puts a detached session in a pane beside the
//! active one. Ctrl+Enter ends it, Ctrl+Shift+T ends it with everything
//! derived from it, and Ctrl+O opens a terminal in its directory.

use std::path::PathBuf;

use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, With, WithUntracked};
use horizon_workspace::lineage::{
    SessionRow, abbreviate_directory, enclosing_repo_root, order_as_lineage_tree,
    subtree_session_ids,
};
use horizon_workspace::types::SessionSummary;
use horizon_workspace::{SessionKind, Workspace};
use keyboard_types::Key;

use super::Shell;
use super::picker::{self, Picked, Row};

/// A step of the tree's indentation, in CSS pixels.
const INDENT: f32 = 16.;

/// The sessions whose titles hold `query`, as the tree they make.
fn matching(model: &Workspace, query: &str) -> Vec<SessionRow> {
    let query = query.trim().to_ascii_lowercase();
    let sessions: Vec<SessionSummary> = (model.session_summaries().into_iter())
        .filter(|session| query.is_empty() || session.title.to_ascii_lowercase().contains(&query))
        .collect();
    order_as_lineage_tree(&sessions)
}

fn row(session: &SessionRow) -> Row {
    let summary = &session.summary;
    let title = match session.depth {
        0 => summary.title.clone(),
        _ => format!("↳ {}", summary.title),
    };
    let mut parts = vec![("name", title)];
    parts.push(match summary.attached {
        true => ("attached", "attached".to_string()),
        false => ("detached", "detached".to_string()),
    });
    if let Some(directory) = &summary.directory {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let repository = enclosing_repo_root(directory);
        let short = abbreviate_directory(directory, repository.as_deref(), home.as_deref());
        parts.push(("about", short));
    }
    if session.has_children {
        parts.push(("about", "subtree".to_string()));
    }
    Row {
        key: format!("{:?} {parts:?}", summary.id),
        class: "session",
        parts,
        indent: session.depth as f32 * INDENT,
    }
}

pub(super) fn view(shell: Shell) -> El {
    let rows = move |query: &str| {
        (shell.model).with(|model| matching(model, query).iter().map(row).collect())
    };
    let picked = move |index: usize, picked: Picked| {
        let chosen = (shell.model).with_untracked(|model| {
            (matching(model, &shell.query.get_untracked()).get(index))
                .map(|row| row.summary.clone())
        });
        let Some(session) = chosen else {
            return;
        };
        match picked {
            Picked::Confirmed => {
                shell.modal.set(None);
                shell.change(|model| {
                    match model.pane_location_for_session(session.id) {
                        Some((tab, pane)) => {
                            model.activate_pane_index(tab, pane);
                        }
                        None => {
                            model.attach_existing_session_to_split_activated(session.id, true);
                        }
                    }
                    model.exit_workspace_mode();
                });
            }
            // The list stays, without the session.
            Picked::ConfirmedSecondary => {
                shell.model_changed(|model| {
                    model.terminate_session(session.id);
                });
            }
            Picked::Chord(key) => {
                let character = match &key.key {
                    Key::Character(character) => character.to_lowercase(),
                    _ => return,
                };
                match (character.as_str(), key.modifiers.shift()) {
                    ("t", true) => shell.model_changed(|model| {
                        let sessions = model.session_summaries();
                        for id in subtree_session_ids(&sessions, session.id) {
                            model.terminate_session(id);
                        }
                    }),
                    ("o", false) => {
                        if let Some(directory) = session.directory {
                            shell.modal.set(None);
                            shell.open_terminal_in(directory);
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    picker::view(shell, rows, picked)
}

impl Shell {
    /// A new terminal tab whose shell starts in `directory`.
    pub(super) fn open_terminal_in(&self, directory: PathBuf) {
        let kept = self.kept();
        self.change(|model| {
            model.exit_workspace_mode();
            let session = model.open_tab_with_new_session_activated(SessionKind::Terminal, true);
            (kept.starting.borrow_mut()).insert(session, super::Start::within(directory));
        });
    }
}
