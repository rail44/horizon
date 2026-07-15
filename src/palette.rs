//! The command palette, built on gpui-component's searchable `List`
//! (reuse over port — docs/gpui-migration-design.md): the component owns
//! the query input, filtering lifecycle, selection, and keyboard
//! handling; Horizon supplies the command catalog (the shared pure
//! command model) and executes the confirmed `CommandId`. The shell
//! subscribes to `ListEvent` for confirm/cancel, so this delegate holds
//! no back-pointer.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::{h_flex, Icon, IconName, IndexPath};
use horizon_workspace::commands::{filter_command_entries, CommandEntry};

use crate::theme;

pub struct PaletteDelegate {
    all: Vec<CommandEntry>,
    filtered: Vec<CommandEntry>,
    // The currently-selected row, mirrored from `set_selected_index` --
    // the delegate-side accessor `render_item` needs to know whether the
    // row it's rendering is the one sitting on `theme::surface_selected()`
    // (`docs/theme-design.md`'s 2026-07-15 contrast audit, item 2), since
    // `ListState` doesn't expose its own selection to the delegate any
    // other way.
    selected: Option<IndexPath>,
}

impl PaletteDelegate {
    pub fn new(entries: Vec<CommandEntry>) -> Self {
        Self {
            filtered: entries.clone(),
            all: entries,
            selected: None,
        }
    }

    pub fn entry_at(&self, index: IndexPath) -> Option<&CommandEntry> {
        self.filtered.get(index.row)
    }
}

impl ListDelegate for PaletteDelegate {
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
        self.filtered = filter_command_entries(self.all.clone(), query);
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _window: &mut Window,
        _cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let entry = self.filtered.get(index.row)?;
        let selected = self.selected == Some(index);
        // The selected row's on-screen surface is `theme::surface_
        // selected()`, not the plain background every other row sits on
        // -- floor a role's text color against it when this row is the
        // selected one (item 2 of the 2026-07-15 contrast audit), the
        // same `readable_on` treatment `src/agent/view.rs` already gives
        // text painted on a non-background surface. `text_subtle` (the
        // disabled-command case) stays unsnapped even when selected --
        // decorative by definition, exempt from the text floor
        // (`docs/theme-design.md`), same rule every other snap call site
        // in this codebase follows.
        let snap = |color: Hsla| {
            if selected {
                theme::readable_on(color, theme::surface_selected())
            } else {
                color
            }
        };
        let title_color = if !entry.enabled {
            theme::text_subtle()
        } else if entry.spec.destructive {
            snap(theme::danger())
        } else {
            snap(theme::text_primary())
        };
        let description_color = snap(theme::text_muted());
        Some(
            ListItem::new(index).child(
                div()
                    .flex()
                    .flex_col()
                    .py_0p5()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(title_color)
                            .child(entry.spec.title),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(description_color)
                            .child(entry.spec.description),
                    ),
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

    // Same visual as gpui-component's own default (an `Inbox` icon,
    // centered), but colored at full opacity through a theme role instead
    // of `muted_foreground.opacity(0.6)` -- item 6 of the 2026-07-15
    // contrast audit (that default landed the icon at ~2.25:1 against
    // `background`).
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
