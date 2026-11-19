//! The session manager: the list of the workspace's sessions (the
//! extension `plugins/sessions`) in a sheet over the workspace (`over`).
//! Going to a session, or opening a terminal where one works, leaves it.

use std::path::PathBuf;
use std::rc::Rc;

use horizon_ui::reactive_graph::traits::Set;
use horizon_ui::{El, el};
use horizon_workspace::SessionKind;

use super::{Shell, plugins, styles};
use crate::plugin_host::{Asks, SessionAsk, SessionAsks};

/// The extension's name.
const LIST: &str = "sessions";

pub(super) fn view(shell: Shell) -> El {
    let inside = el("div").class(styles::MANAGER);
    match plugins::extension(LIST) {
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
            inside.child(shell.run_extension(&extension, asks, None).0)
        }
        None => inside.child(Shell::no_extension(
            "The list of sessions is an extension, and it is not built.",
        )),
    }
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
