//! Where the pane the cursor is on is moved to, chosen from a list (the
//! `Move to Tab…` command, `m` in workspace mode): a new tab of its group,
//! or any other tab on screen, beside that tab's active pane
//! (`docs/workspace-layout-design.md`).

use horizon_ui::El;
use horizon_ui::reactive_graph::traits::{GetUntracked, Set, WithUntracked};
use horizon_workspace::types::TabId;

use super::picker::{self, Row};
use super::{Shell, styles};

/// One place the pane can go to.
#[derive(Clone)]
enum To {
    NewTab,
    Tab(TabId),
}

/// The places, as the list shows them, for what is typed: a new tab first.
fn matching(shell: Shell, query: &str) -> Vec<(To, String, Option<String>)> {
    let query = query.trim().to_ascii_lowercase();
    let destinations = shell.model.with_untracked(|model| model.tabs_to_move_to());
    // (Which group a tab is in, where there are more than one.)
    let groups = (destinations.iter().map(|to| to.group).max()).unwrap_or(0) + 1;
    let mut places = vec![(To::NewTab, "New Tab".to_string(), None)];
    places.extend(destinations.into_iter().map(|to| {
        let group = (groups > 1).then(|| format!("group {}", to.group + 1));
        (To::Tab(to.tab), to.title, group)
    }));
    places.retain(|(_, title, _)| query.is_empty() || title.to_ascii_lowercase().contains(&query));
    places
}

pub(super) fn view(shell: Shell) -> El {
    let rows = move |query: &str| {
        (matching(shell, query).into_iter())
            .map(|(_, title, group)| Row {
                key: format!("{title} {}", group.clone().unwrap_or_default()),
                class: (styles::COMMAND, None),
                parts: [
                    Some((styles::NAME, title)),
                    group.map(|group| (styles::ABOUT, group)),
                ]
                .into_iter()
                .flatten()
                .collect(),
                indent: 0.,
            })
            .collect()
    };
    let picked = move |index: usize| {
        let places = matching(shell, &shell.search.query.get_untracked());
        let Some((to, ..)) = places.get(index).cloned() else {
            return;
        };
        shell.modal.set(None);
        shell.change(|model| {
            _ = match to {
                To::NewTab => model.move_cursor_pane_to_new_tab(),
                To::Tab(tab) => model.move_cursor_pane_to_tab(tab),
            }
        });
    };
    picker::view(shell.search, rows, picked, "move")
}
