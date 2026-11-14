//! The terminal as a plugin: a view of the session it is given, and the
//! clipboard's commands for it.

use std::rc::Rc;

use horizon_terminal_core::TerminalCommand;
use horizon_theme_css::TerminalLook;
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callback, request_paste};

use super::{OpenUrl, Session, Terminal, TerminalView, build};
use crate::plugin::{Command, Mounted, Plugin, Running, SessionPlugin};
use crate::terminald::Terminald;

pub struct TerminalPlugin {
    /// The session the pane shows, which the shell keeps.
    pub session: Rc<Session>,
    pub look: Signal<TerminalLook>,
    pub open_url: Option<OpenUrl>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPaneCommand {
    CopySelection,
    Paste,
}

impl TerminalPlugin {
    /// [`Plugin::mount`], and the view's own state, for whatever looks at
    /// what the view shows (the tests, the measurements).
    pub fn mount_keeping(self) -> (Mounted<(), TerminalPaneCommand>, Terminal) {
        let session = self.session.clone();
        let (view, terminal) = build(TerminalView {
            session: self.session,
            look: self.look,
            open_url: self.open_url,
        });
        let run = Callback::new(move |command| match command {
            TerminalPaneCommand::CopySelection => session.send(TerminalCommand::CopySelection),
            // The clipboard's text arrives as a paste, at the view with
            // the keys.
            TerminalPaneCommand::Paste => request_paste(),
        });
        let mounted = Mounted {
            view,
            run,
            enabled: None,
            state: Signal::stored_local(()),
        };
        (mounted, terminal)
    }
}

impl Plugin for TerminalPlugin {
    const NAME: &'static str = "terminal";
    const TITLE: &'static str = "Terminal";
    type State = ();
    type Command = TerminalPaneCommand;
    type Asked = std::convert::Infallible;

    fn commands() -> Vec<Command<TerminalPaneCommand>> {
        vec![
            Command {
                name: "copy-terminal-selection",
                title: "Copy Terminal Selection",
                description: "Copy the active terminal's selection to the clipboard.",
                destructive: false,
                command: TerminalPaneCommand::CopySelection,
            },
            Command {
                name: "paste-into-terminal",
                title: "Paste into Terminal",
                description: "Paste the clipboard's text into the active terminal.",
                destructive: false,
                command: TerminalPaneCommand::Paste,
            },
        ]
    }

    fn mount(
        self,
        _: (),
        _: Callback<std::convert::Infallible>,
    ) -> Mounted<(), TerminalPaneCommand> {
        self.mount_keeping().0
    }
}

impl SessionPlugin for TerminalPlugin {
    const KIND: horizon_workspace::SessionKind = horizon_workspace::SessionKind::Terminal;
    type Session = Session;
    type Runtime = Terminald;

    fn running(runtime: &Terminald) -> Result<Vec<Running>, String> {
        let running = runtime.handle().terminal_list()?;
        Ok((running.iter())
            .map(|terminal| Running {
                session: horizon_workspace::SessionId::from_uuid(terminal.session_id),
                directory: None,
                parent: None,
            })
            .collect())
    }

    fn end(session: &Session) {
        session.send(TerminalCommand::Shutdown);
    }
}
