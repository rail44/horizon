//! The agent as a plugin: a view of the session it is given, and the
//! commands that answer the session's approvals and steer its turn.

use std::rc::Rc;

use horizon_shell_runtime::AgentCommand;
use horizon_shell_runtime::agent_model::ToolCallIdentity;
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callable, Callback};

use super::{Agent, AgentSession, live};
use crate::agentd::Agentd;
use crate::plugin::{Command, Enabled, Mounted, Plugin, Running, SessionPlugin};

pub struct AgentPlugin {
    /// The session the pane shows, which the shell keeps.
    pub session: Rc<AgentSession>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentPaneCommand {
    Approve,
    Deny,
    Cancel,
    SwitchModel,
    SwitchEffort,
}

/// What the agent asks of whoever shows it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentAsked {
    /// The provider and model the session goes on with, to choose.
    SwitchModel,
    /// The reasoning effort the session goes on with, to choose.
    SwitchEffort,
}

impl AgentPlugin {
    /// [`Plugin::mount`], and the view's own state, for whatever looks at
    /// what the view shows (the tests, the measurements).
    pub fn mount_keeping(
        self,
        ask: Callback<AgentAsked>,
    ) -> (Mounted<(), AgentPaneCommand>, Agent) {
        let session = self.session;
        let (view, agent) = live(session.clone(), move || ask.run(AgentAsked::SwitchModel));
        let oldest = |session: &AgentSession| -> Option<ToolCallIdentity> {
            session.pending_approvals().into_iter().next()
        };
        let running = session.clone();
        let run = Callback::new(move |command| {
            let sent = match command {
                AgentPaneCommand::Approve => {
                    oldest(&running).map(|identity| AgentCommand::Approve { identity })
                }
                AgentPaneCommand::Deny => oldest(&running).map(|identity| AgentCommand::Deny {
                    identity,
                    reason: None,
                }),
                AgentPaneCommand::Cancel => Some(AgentCommand::Cancel),
                AgentPaneCommand::SwitchModel => return ask.run(AgentAsked::SwitchModel),
                AgentPaneCommand::SwitchEffort => return ask.run(AgentAsked::SwitchEffort),
            };
            if let Some(sent) = sent {
                running.send(sent);
            }
        });
        let enabled = Enabled::new(move |command| {
            let standing = session.standing();
            match command {
                AgentPaneCommand::Approve | AgentPaneCommand::Deny => standing.pending_approval,
                AgentPaneCommand::Cancel => standing.turn_in_flight,
                AgentPaneCommand::SwitchModel => true,
                AgentPaneCommand::SwitchEffort => session.effort().is_some(),
            }
        });
        let mounted = Mounted {
            view,
            run,
            enabled: Some(enabled),
            state: Signal::stored_local(()),
        };
        (mounted, agent)
    }
}

impl Plugin for AgentPlugin {
    const NAME: &'static str = "agent";
    const TITLE: &'static str = "AI Agent";
    type State = ();
    type Command = AgentPaneCommand;
    type Asked = AgentAsked;

    fn commands() -> Vec<Command<AgentPaneCommand>> {
        use AgentPaneCommand::*;
        let command = |name, title, description, command| Command {
            name,
            title,
            description,
            destructive: false,
            command,
        };
        vec![
            command(
                "approve-tool-call",
                "Approve Pending Tool Call",
                "Approve the pending tool call awaiting approval.",
                Approve,
            ),
            command(
                "deny-tool-call",
                "Deny Pending Tool Call",
                "Deny the pending tool call awaiting approval.",
                Deny,
            ),
            command(
                "cancel-agent-turn",
                "Cancel Agent Turn",
                "Cancel the agent turn currently in flight.",
                Cancel,
            ),
            command(
                "switch-model",
                "Switch Model…",
                "Switch the active agent session's provider and model.",
                SwitchModel,
            ),
            command(
                "switch-effort",
                "Switch Effort…",
                "Switch how much the active agent session's model reasons.",
                SwitchEffort,
            ),
        ]
    }

    fn create(given: crate::plugin::Given) -> Result<(), String> {
        Self::begin(given).map(|_| ())
    }

    fn mount(self, _: (), ask: Callback<AgentAsked>) -> Mounted<(), AgentPaneCommand> {
        self.mount_keeping(ask).0
    }
}

impl SessionPlugin for AgentPlugin {
    const KIND: horizon_workspace::SessionKind = horizon_workspace::SessionKind::Agent;
    type Session = AgentSession;
    type Runtime = Agentd;

    /// The role it works in (`--role`), one a person can start an agent
    /// in, and what it is told first (`--prompt`).
    fn begin(mut given: crate::plugin::Given) -> Result<crate::plugin::Begins, String> {
        let role = given
            .named("role")
            .map(|role| {
                let known = horizon_agent::packages::Packages::default().user_launchable();
                if !known.iter().any(|known| known.id == role) {
                    let available: Vec<&str> = known.iter().map(|known| known.title).collect();
                    return Err(format!(
                        "unknown role: {role} (available: {})",
                        available.join(", ")
                    ));
                }
                Ok(horizon_agent::roles::RoleId(role))
            })
            .transpose()?;
        let prompt = given.named("prompt");
        given.done()?;
        Ok(crate::plugin::Begins { role, prompt })
    }

    fn running(runtime: &Agentd) -> Result<Vec<Running>, String> {
        runtime.running()
    }

    fn end(session: &AgentSession) {
        session.send(AgentCommand::Close);
    }
}
