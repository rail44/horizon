//! The view chooser: the modal `New Tab…`/`Split Right…`/`Split Down…`
//! open to pick what the new pane hosts (Terminal, Agent, a role-tagged
//! agent flavor like Configuration Agent, or a session-less first-party
//! view like Theme Settings) — same searchable List pattern as the
//! palette; the shell stages the placement and executes the choice on
//! confirm.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, Icon, IconName, IndexPath};
use horizon_workspace::{PaneKind, ViewKind};

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
        ViewChoice {
            title: "Theme Settings",
            kind: PaneKind::View(ViewKind::ThemeSettings),
            role_id: None,
        },
    ]
}

pub struct ViewChooserDelegate {
    all: Vec<ViewChoice>,
    filtered: Vec<ViewChoice>,
    // The currently-selected row, mirrored from `set_selected_index` --
    // see `PaletteDelegate`'s own field doc (`src/palette.rs`) for why
    // this is the delegate's own responsibility to track.
    selected: Option<IndexPath>,
}

impl ViewChooserDelegate {
    pub fn new() -> Self {
        let all = view_choices();
        Self {
            filtered: all.clone(),
            all,
            selected: None,
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
        let mut title_color = theme::text_primary();
        // Floor against the selected-row surface rather than plain
        // `background` -- item 2 of the 2026-07-15 contrast audit; see
        // `PaletteDelegate::render_item`'s own comment.
        if self.selected == Some(index) {
            title_color = theme::readable_on(title_color, theme::surface_selected());
        }
        Some(
            ListItem::new(index).child(
                div()
                    .py_0p5()
                    .text_size(px(13.0))
                    .text_color(title_color)
                    .child(choice.title),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = index;
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_color(theme::readable_on(
                theme::text_muted(),
                rgb(theme::background()).into(),
            ))
            .child(Icon::new(IconName::Inbox).size_12())
    }
}
