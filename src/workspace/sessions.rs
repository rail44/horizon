//! The session manager: the list of the workspace's sessions (the
//! extension `plugins/sessions`) over everything. Going to a session, or
//! opening a terminal where one works, leaves it; so does Escape, or a
//! press outside.

use std::path::PathBuf;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::traits::Set;
use horizon_ui::{El, el, prevent_default, stop_propagation};
use horizon_workspace::SessionKind;
use keyboard_types::{Key, NamedKey};

use super::{Shell, plugins, styles};
use crate::plugin_host::{Asks, SessionAsk, SessionAsks};

/// The extension's name.
const LIST: &str = "sessions";

pub(super) fn view(shell: Shell) -> El {
    let close = move || {
        shell.modal.set(None);
        shell.focus_active();
    };
    let sheet = el("div")
        .class(styles::SHEET)
        // A press in the list is not one outside it.
        .on("pointerdown", |_| stop_propagation())
        // (Escape the list had no use for.)
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            if key.key == Key::Named(NamedKey::Escape) {
                close();
                prevent_default();
                stop_propagation();
            }
        });
    let sheet = match plugins::extension(LIST) {
        Some(extension) => {
            let SessionAsks { feed, act } = shell.session_asks();
            let asks = Asks {
                sessions: Some(SessionAsks {
                    feed,
                    act: Rc::new(move |ask| {
                        if matches!(ask, SessionAsk::Show(_) | SessionAsk::OpenTerminal(_)) {
                            shell.modal.set(None);
                        }
                        act(ask);
                    }),
                }),
                ..Asks::default()
            };
            sheet.child(shell.run_extension(&extension, asks, None).0)
        }
        None => sheet.child(Shell::no_extension(
            "The list of sessions is an extension, and it is not built.",
        )),
    };
    el("div")
        .class(styles::SCRIM)
        .on("pointerdown", move |_| close())
        .child(sheet)
}

impl Shell {
    /// A new terminal tab whose shell starts in `directory`.
    pub(super) fn open_terminal_in(&self, directory: PathBuf) {
        let kept = self.kept();
        self.change(|model| {
            model.exit_workspace_mode();
            let session = model.open_tab_with_new_session_activated(SessionKind::Terminal, true);
            (kept.starting.borrow_mut()).insert(session, super::Start::within(directory));
        });
    }
}
