//! The command palette, built on gpui-component's searchable `List`
//! (reuse over port — docs/gpui-migration-design.md): the component owns
//! the query input, filtering lifecycle, selection, and keyboard
//! handling; Horizon supplies the command catalog (the shared pure
//! command model) and executes the confirmed `CommandId`. The shell
//! subscribes to `ListEvent` for confirm/cancel, so this delegate holds
//! no back-pointer.

use gpui::*;
use gpui_component::list::{ListDelegate, ListItem, ListState};
use gpui_component::IndexPath;
use horizon_workspace::commands::{filter_command_entries, CommandEntry};

pub struct PaletteDelegate {
    all: Vec<CommandEntry>,
    filtered: Vec<CommandEntry>,
}

impl PaletteDelegate {
    pub fn new(entries: Vec<CommandEntry>) -> Self {
        Self {
            filtered: entries.clone(),
            all: entries,
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
        let title_color = if !entry.enabled {
            rgb(0x5f6370)
        } else if entry.spec.destructive {
            rgb(0xe06c75)
        } else {
            rgb(0xe9ecf2)
        };
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
                            .text_color(rgb(0x8a90a0))
                            .child(entry.spec.description),
                    ),
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
