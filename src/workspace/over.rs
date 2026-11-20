//! What is shown over the workspace: a sheet in the middle of it, with
//! the keys. A press outside the sheet leaves it, and so does Escape that
//! what is in it did not take; the keys then go back to the pane that
//! had them.
//!
//! What is in the sheet says how large it is.

use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::traits::{Get, Set, WithValue};
use horizon_ui::{Component, El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use super::{Modal, Over, Shell, styles};
use crate::plugin_host::ModalAsks;

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
        .child(el("div").class(styles::DIM_BEHIND))
        .child(sheet)
}

/// Has an extension's view over the workspace (`horizon:ui/modal`) taken
/// out of its sheet, and its extension told, by whatever takes its place
/// there or leaves. The view is the extension's: the sheet is made
/// around it and removed from around it, and the view itself is neither
/// made nor removed here.
pub(super) fn follow_extensions(shell: Shell) {
    Effect::new(move |_| {
        let now = shell.modal.get();
        let kept = shell.kept();
        let mut over = kept.over.borrow_mut();
        if (over.as_ref()).is_some_and(|over| now != Some(Modal::Extension(over.asked)))
            && let Some(over) = over.take()
        {
            over.gone();
        }
    });
}

impl Over {
    /// Takes the view out of the sheet, the sheet away, and says so.
    fn gone(self) {
        self.view.detach();
        self.sheet.remove();
        let _ = self.closed.send(true);
    }
}

impl Shell {
    /// What this shell does to put an extension's view over the workspace.
    /// The view is `by`'s: the instance made for a command, if one of
    /// those asks.
    pub(super) fn modal_asks(&self, by: Option<u64>) -> ModalAsks {
        let shell = *self;
        ModalAsks {
            open: Rc::new(move |node, closed| {
                let kept = shell.kept();
                let asked = kept.over_asked.get() + 1;
                kept.over_asked.set(asked);
                let view = El(node);
                let sheet = self::view(shell, view);
                let before = kept.over.replace(Some(Over {
                    asked,
                    by,
                    view,
                    sheet,
                    closed,
                }));
                if let Some(before) = before {
                    before.gone();
                }
                shell.root.child(sheet);
                shell.open(Modal::Extension(asked));
                view.focus_within();
            }),
            close: Rc::new(move |node| {
                // (An extension let go of as the shell itself goes asks
                // this of a shell that is gone.)
                let Some(kept) = shell.kept.try_with_value(Rc::clone) else {
                    return;
                };
                let there = (kept.over.borrow().as_ref()).is_some_and(|over| over.view.0 == node);
                if there {
                    shell.modal.set(None);
                    shell.focus_active();
                }
            }),
        }
    }
}
