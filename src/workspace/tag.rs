//! A pane's tag (`docs/workspace-layout-design.md`): in its top right
//! corner, clear of a terminal's text, which begins at the left. It holds
//! the grip and the pane's name, by which the pane is taken and moved
//! (`super::moving`), and a button that closes the pane. Every pane has
//! one, a pane that is all of its tab too, and it shows while the pointer
//! is over the pane or in workspace mode.

use horizon_components::Icon;
use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::traits::{Get, With};
use horizon_ui::{El, el, prevent_default, stop_propagation};
use horizon_workspace::PaneId;

use super::{Shell, styles};

pub(super) fn tag(shell: Shell, pane: PaneId) -> El {
    el("div")
        .class(styles::TAG)
        // (What it shows by is on the tag itself, not on the pane or the
        // page: a class an element around it gains is not taken into its
        // style in every case.)
        .class_dyn(move || {
            (
                shell.mode.get().then_some(styles::IN_MODE),
                shell.moving_any().then_some(styles::AWAY),
            )
        })
        .child(grip(shell, pane))
        .child(close(shell, pane))
}

/// The grip and the pane's name: what the pane is taken by.
fn grip(shell: Shell, pane: PaneId) -> El {
    el("div")
        .class(styles::TAG_GRIP)
        .attr("title", "Drag to move this view")
        .child(Icon::DotsSixVertical)
        .child(el("span").class(styles::TAG_NAME).text_dyn(move || {
            (shell.model.with(|model| model.pane_label(pane))).unwrap_or_default()
        }))
        .on("pointerdown", move |event| {
            if let DomEventData::PointerDown(pointer) = &event.data {
                shell.begin_moving(pane, pointer.page_x(), pointer.page_y());
            }
            // (Not a press in the pane: its view keeps the focus as it
            // was, and nothing is selected.)
            prevent_default();
            stop_propagation();
        })
}

/// Closes the pane, or its tab when it is the tab's last, as workspace
/// mode's `x` does on it. The sessions shown there go on: closing detaches
/// them.
fn close(shell: Shell, pane: PaneId) -> El {
    el("div")
        .class(styles::TAG_CLOSE)
        .attr("title", "Close this view")
        .text("×")
        // (Neither a move nor a press in the pane.)
        .on("pointerdown", |_| {
            prevent_default();
            stop_propagation();
        })
        .on("click", move |_| {
            shell.close_pane_of(pane);
            stop_propagation();
        })
}
