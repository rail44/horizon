//! What is shown over the workspace: a sheet in the middle of it, with
//! the keys. A press outside the sheet leaves it, and so does Escape that
//! what is in it did not take; the keys then go back to the pane that
//! had them.
//!
//! What is in the sheet says how large it is.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::traits::Set;
use horizon_ui::{Component, El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use super::{Shell, styles};

pub(super) fn view(shell: Shell, inside: impl Component) -> El {
    let close = move || {
        shell.modal.set(None);
        shell.focus_active();
    };
    let sheet = el("div")
        .class(styles::SHEET)
        // A press in the sheet is not one outside it.
        .on("pointerdown", |_| stop_propagation())
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            if key.key == Key::Named(NamedKey::Escape) {
                close();
                prevent_default();
                stop_propagation();
            }
        })
        .child(inside);
    el("div")
        .class(styles::SCRIM)
        .on("pointerdown", move |_| close())
        .child(sheet)
}
