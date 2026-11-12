//! The view chooser: which view a new pane shows. A terminal, an agent,
//! or an agent in one of the roles a person can start one in.

use horizon_agent::roles::{RoleId, user_launchable};
use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set};
use horizon_workspace::SessionKind;

use super::picker::{self, Picked, Row};
use super::{Placement, Shell};

/// The views to choose from whose titles hold `query`.
fn matching(query: &str) -> Vec<(String, SessionKind, Option<RoleId>)> {
    let mut choices = vec![
        ("Terminal".to_string(), SessionKind::Terminal, None),
        ("Agent".to_string(), SessionKind::Agent, None),
    ];
    for role in user_launchable() {
        let id = RoleId(role.id.to_string());
        choices.push((role.title.to_string(), SessionKind::Agent, Some(id)));
    }
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
            let (_, kind, role) = choices.swap_remove(index);
            shell.modal.set(None);
            shell.create(kind, role, placement);
        }
    };
    picker::view(shell, rows, picked)
}
