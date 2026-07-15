//! The view chooser: the modal `New Tab…`/`Split Right…`/`Split Down…`
//! open to pick what the new pane hosts (Terminal, Agent, or a
//! role-tagged agent flavor like Configuration Agent) — same searchable
//! List pattern as the palette; the shell stages the placement and
//! executes the choice on confirm.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::IndexPath;
use horizon_workspace::PaneKind;

use crate::theme;

/// Where the chosen view goes — staged by the command that opened the
/// chooser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    NewTab,
    SplitRight,
    SplitDown,
}

#[derive(Clone)]
pub struct ViewChoice {
    pub title: &'static str,
    pub kind: PaneKind,
    pub role_id: Option<horizon_agent::roles::RoleId>,
}

pub fn view_choices() -> Vec<ViewChoice> {
    vec![
        ViewChoice {
            title: "Terminal",
            kind: PaneKind::Terminal,
            role_id: None,
        },
        ViewChoice {
            title: "Agent",
            kind: PaneKind::Agent,
            role_id: None,
        },
        ViewChoice {
            title: "Configuration Agent",
            kind: PaneKind::Agent,
            role_id: Some(horizon_agent::roles::RoleId(
                horizon_agent::roles::CONFIG_ROLE.id.to_string(),
            )),
        },
    ]
}

pub struct ViewChooserDelegate {
    all: Vec<ViewChoice>,
    filtered: Vec<ViewChoice>,
}

impl ViewChooserDelegate {
    pub fn new() -> Self {
        let all = view_choices();
        Self {
            filtered: all.clone(),
            all,
        }
    }

    pub fn choice_at(&self, index: IndexPath) -> Option<&ViewChoice> {
        self.filtered.get(index.row)
    }
}

impl ListDelegate for ViewChooserDelegate {
    type Item = ListItem;

    fn items_count(&self, _section: usize, _cx: &App) -> usize {
        self.filtered.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_ascii_lowercase();
        self.filtered = self
            .all
            .iter()
            .filter(|choice| query.is_empty() || choice.title.to_ascii_lowercase().contains(&query))
            .cloned()
            .collect();
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let choice = self.filtered.get(index.row)?;
        Some(
            ListItem::new(index).child(
                div()
                    .py_0p5()
                    .text_size(px(13.0))
                    .text_color(theme::text_primary())
                    .child(choice.title),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        _index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
    }
}
