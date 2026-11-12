//! The view chooser: which view a new pane shows. A terminal, an agent,
//! an agent in one of the roles a person can start one in, or the board.

use horizon_agent::roles::{RoleId, user_launchable};
use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set};
use horizon_workspace::{SessionKind, ViewKind};

use super::picker::{self, Picked, Row};
use super::{Placement, Shell};

/// What a new pane shows.
enum Choice {
    Session(SessionKind, Option<RoleId>),
    View(ViewKind),
}

/// The views to choose from whose titles hold `query`.
fn matching(query: &str) -> Vec<(String, Choice)> {
    let mut choices = vec![
        (
            "Terminal".to_string(),
            Choice::Session(SessionKind::Terminal, None),
        ),
        (
            "Agent".to_string(),
            Choice::Session(SessionKind::Agent, None),
        ),
    ];
    for role in user_launchable() {
        let id = RoleId(role.id.to_string());
        let choice = Choice::Session(SessionKind::Agent, Some(id));
        choices.push((role.title.to_string(), choice));
    }
    choices.push(("Board".to_string(), Choice::View(ViewKind::BoardList)));
    let query = query.trim().to_ascii_lowercase();
    choices.retain(|(title, ..)| query.is_empty() || title.to_ascii_lowercase().contains(&query));
    choices
}

pub(super) fn view(shell: Shell, placement: Placement) -> El {
    let rows = |query: &str| {
        (matching(query).into_iter())
            .map(|(title, ..)| Row {
                key: title.clone(),
                class: "command",
                parts: vec![("name", title)],
                indent: 0.,
            })
            .collect()
    };
    let picked = move |index: usize, picked: Picked| {
        if !matches!(picked, Picked::Confirmed) {
            return;
        }
        let mut choices = matching(&shell.query.get_untracked());
        if index < choices.len() {
            let (_, choice) = choices.swap_remove(index);
            shell.modal.set(None);
            match choice {
                Choice::Session(kind, role) => shell.create(kind, role, placement),
                Choice::View(kind) => shell.create_view(kind, placement),
            }
        }
    };
    picker::view(shell, rows, picked)
}
