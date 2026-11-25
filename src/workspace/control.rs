//! What the control plane asks of the shell: the external commands this
//! shell carries out so far (terminals, agents, sessions), and the two
//! queries.

use horizon_control::contract::{
    EnvelopeBody, Query, SessionEntry, Sessions, State, Workspace as Snapshot,
};
use horizon_control::host::executor::{ControlRequest, error_body};
use horizon_shell_runtime::AgentCommand;
use horizon_shell_runtime::control_invoke::{self, Command};
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::commands::{CommandId, core_commands};
use horizon_workspace::{PaneId, PluginPane, SessionId, SessionKind, Workspace};

use super::{Shell, Start, plugins};
use crate::agent::plugin::AgentPlugin;
use crate::plugin::{Begins, Given, Plugin as _, SessionPlugin as _};
use crate::preview::Target;
use crate::terminal::plugin::TerminalPlugin;

/// The answer to a command this shell has no view or runtime for yet.
const NOT_YET: &str = "this command is not carried out yet";

impl Shell {
    pub fn control(&self, request: &ControlRequest) -> EnvelopeBody {
        match request {
            ControlRequest::Invoke(invoke) => (control_invoke::parse(invoke))
                .and_then(|command| self.invoke(command))
                .unwrap_or_else(error_body),
            ControlRequest::Query(query) => self.query(query),
        }
    }

    /// Carries out what the control plane asks and answers through
    /// `reply`: at once, or, for what is a round trip to the agent
    /// runtime, from a thread when that is back.
    pub fn control_answering(
        &self,
        request: &ControlRequest,
        reply: impl FnOnce(EnvelopeBody) + Send + 'static,
    ) {
        let later = match request {
            ControlRequest::Invoke(invoke) => control_invoke::parse(invoke).ok(),
            ControlRequest::Query(_) => None,
        };
        match later {
            Some(Command::SetModel {
                session_id,
                provider,
                model,
            }) => {
                let Some(models) = self.kept().models.clone() else {
                    reply(error_body("the agent runtime is not running"));
                    return;
                };
                std::thread::spawn(move || {
                    reply(match models.switch(session_id, &provider, &model) {
                        Ok(()) => EnvelopeBody::Ok { session_id: None },
                        Err(error) => error_body(format!("model switch failed: {error}")),
                    });
                });
            }
            Some(Command::SetEffort { session_id, level }) => {
                let Some(models) = self.kept().models.clone() else {
                    reply(error_body("the agent runtime is not running"));
                    return;
                };
                std::thread::spawn(move || {
                    reply(match models.set_effort(session_id, &level) {
                        Ok(()) => EnvelopeBody::Ok { session_id: None },
                        Err(error) => error_body(format!("effort switch failed: {error}")),
                    });
                });
            }
            // (Answered once the components are read, off the UI thread.)
            Some(Command::ExtensionUse { file }) => {
                self.use_extension(file, move |shell, used| {
                    reply(match used {
                        Ok(_) => shell.extensions_body(),
                        Err(error) => error_body(error),
                    });
                });
            }
            Some(Command::ExtensionReset { name }) => {
                let reply = std::sync::Arc::new(std::sync::Mutex::new(Some(reply)));
                let answered = reply.clone();
                let reset = self.reset_extensions(name.as_deref(), move |shell| {
                    if let Some(reply) = answered.lock().unwrap().take() {
                        reply(shell.extensions_body());
                    }
                });
                // (Refused, nothing is read and the answer is this one.)
                if let Err(error) = reset
                    && let Some(reply) = reply.lock().unwrap().take()
                {
                    reply(error_body(error));
                }
            }
            _ => reply(self.control(request)),
        }
    }

    fn invoke(&self, command: Command) -> Result<EnvelopeBody, String> {
        let done = Ok(EnvelopeBody::Ok { session_id: None });
        match command {
            Command::Attach {
                session_id,
                activate,
            } => {
                let mut attached = None;
                self.model_changed(|model| {
                    attached =
                        model.attach_existing_session_to_split_activated(session_id, activate);
                });
                attached.ok_or("unknown session")?;
                if activate {
                    self.focus_active();
                }
                done
            }
            Command::Terminate(session) => match self.terminate_session(session) {
                true => done,
                false => Err("unknown session".to_string()),
            },
            Command::TerminateAllDetached => {
                self.execute(CommandId::TerminateAllDetachedSessions);
                done
            }
            Command::Execute(command) => {
                self.execute(command);
                done
            }
            Command::New {
                plugin,
                arguments,
                named,
                directory,
                split,
                issuer,
                activate,
            } => {
                let given = Given {
                    arguments,
                    named,
                    directory,
                };
                self.new_pane(&plugin, given, split, issuer, activate)
            }
            Command::PluginCommand(name) => match self.command(&name) {
                Some(command) if !command.enabled => {
                    Err(format!("`{name}` cannot be carried out now"))
                }
                Some(command) if self.run(&command) => done,
                _ => Err(format!(
                    "unknown command `{name}`: not one of the shell's, nor of the plugin the \
                     cursor's pane shows"
                )),
            },
            Command::Approve {
                session_id,
                identity,
            } => self.tell(session_id, AgentCommand::Approve { identity }),
            Command::Deny {
                session_id,
                identity,
                reason,
            } => self.tell(session_id, AgentCommand::Deny { identity, reason }),
            Command::CancelTurn(session) => self.tell(session, AgentCommand::Cancel),
            Command::ContinueTurn(session) => self.tell(session, AgentCommand::ContinueTurn),
            Command::Send { session_id, text } => {
                self.tell(session_id, AgentCommand::Prompt { text })
            }
            _ => Err(NOT_YET.to_string()),
        }
    }

    /// A new pane of the plugin `name`, made from what was `given` for it
    /// as the plugin says its pane is made. A terminal or an agent is a
    /// new session, begun where the session split, or else the one that
    /// asked (`issuer`), is. A view that shows the same already is made
    /// again in its place.
    fn new_pane(
        &self,
        name: &str,
        given: Given,
        split: Option<(SessionId, horizon_workspace::SplitAxis)>,
        issuer: Option<SessionId>,
        activate: bool,
    ) -> Result<EnvelopeBody, String> {
        if name == TerminalPlugin::NAME {
            let begins = TerminalPlugin::begin(given)?;
            return self.new_session(SessionKind::Terminal, begins, split, issuer, activate);
        }
        if name == AgentPlugin::NAME {
            let begins = AgentPlugin::begin(given)?;
            return self.new_session(SessionKind::Agent, begins, split, issuer, activate);
        }
        let made = match plugins::created(name, given.clone()) {
            Some(made) => made?,
            None => self.created_by_extension(name, given)?,
        };
        if let Some(Some(target)) = plugins::previewed(&made) {
            return self.open_preview(target, split, activate);
        }
        let shown = self.model.with_untracked(|model| {
            (model.all_pane_ids().into_iter()).find(|pane| {
                (model.pane_plugin(*pane))
                    .is_some_and(|plugin| plugin.name == made.name && plugin.state == made.state)
            })
        });
        if let Some(pane) = shown {
            self.remake_pane(pane);
            if activate {
                self.change(|model| _ = model.reveal_pane(pane));
            }
            return Ok(EnvelopeBody::Ok { session_id: None });
        }
        let mut opened = None;
        self.model_changed(|model| {
            opened = match split {
                Some((target, axis)) => model.split_session_with_view(target, made, axis, activate),
                None => Some(model.open_tab_with_view_activated(made, activate)),
            };
        });
        // (The model has the pane's state before its view is made.)
        self.reconcile();
        opened.ok_or("unknown split target session")?;
        if activate {
            self.focus_active();
        }
        Ok(EnvelopeBody::Ok { session_id: None })
    }

    /// The pane the extension `name` makes from what was `given`.
    pub(super) fn created_by_extension(
        &self,
        name: &str,
        mut given: Given,
    ) -> Result<PluginPane, String> {
        let extension = plugins::extension(name).ok_or_else(|| {
            if !self.extensions_read() {
                return format!(
                    "the extensions are still being read; `{name}` may be one of them: ask again"
                );
            }
            let mut known = plugins::NAMES.to_vec();
            let extensions = plugins::extensions();
            known.extend(extensions.iter().map(|extension| extension.name));
            format!("unknown plugin `{name}` (there are: {})", known.join(", "))
        })?;
        let state = match extension.created_from(&mut given)? {
            Some(state) => serde_json::Value::String(state),
            None => serde_json::Value::Null,
        };
        given.done()?;
        Ok(PluginPane {
            name: extension.name.to_string(),
            title: extension.title.clone(),
            state,
        })
    }

    /// A new session of `kind` in a pane, as `begins` says it begins.
    fn new_session(
        &self,
        kind: SessionKind,
        begins: Begins,
        split: Option<(SessionId, horizon_workspace::SplitAxis)>,
        issuer: Option<SessionId>,
        activate: bool,
    ) -> Result<EnvelopeBody, String> {
        let kept = self.kept();
        if kind == SessionKind::Agent && kept.spawn_agent.is_none() {
            return Err(NOT_YET.to_string());
        }
        let Begins { role, prompt } = begins;
        let mut made = None;
        self.model_changed(|model| {
            made = match split {
                Some((target, axis)) => {
                    model.split_session_with_new_session(target, kind, axis, activate)
                }
                None => Some(model.open_tab_with_new_session_activated(kind, activate)),
            };
            // It starts where the session split, or else the one that
            // asked, is.
            if let Some(made) = made {
                let start = Start {
                    from: split.map(|(target, _)| target).or(issuer),
                    directory: None,
                    role,
                };
                kept.starting.borrow_mut().insert(made, start);
            }
        });
        // (The pane, and the session with it, exist from here.)
        self.reconcile();
        let made = made.ok_or("unknown split target session")?;
        if let Some(text) = prompt
            && let Some(session) = kept.agents.borrow().get(&made)
        {
            session.send(AgentCommand::Prompt { text });
        }
        if activate {
            self.focus_active();
        }
        Ok(EnvelopeBody::Ok {
            session_id: Some(made.as_uuid().to_string()),
        })
    }

    /// A preview pane on `target`: the pane that shows its file already,
    /// which reads it again; or an empty preview pane under the cursor;
    /// or a new one.
    fn open_preview(
        &self,
        target: Target,
        split: Option<(SessionId, horizon_workspace::SplitAxis)>,
        activate: bool,
    ) -> Result<EnvelopeBody, String> {
        if !target.path.is_file() {
            return Err(format!("not a file: {}", target.path.display()));
        }
        let previews: Vec<(PaneId, Option<Target>)> = self.model.with_untracked(|model| {
            (model.all_pane_ids().into_iter())
                .filter_map(|pane| Some((pane, plugins::previewed(model.pane_plugin(pane)?)?)))
                .collect()
        });
        let showing = |path: &std::path::Path| {
            (previews.iter())
                .find(|(_, shown)| shown.as_ref().is_some_and(|shown| shown.path == path))
        };
        let cursor = self.model.with_untracked(Workspace::cursor_pane_id);
        let empty =
            (previews.iter()).find(|(pane, shown)| shown.is_none() && Some(*pane) == cursor);
        // The pane is made again on what it is to show, which reads the
        // file anew even if it is the one it showed.
        let path = target.path.clone();
        let shown = plugins::preview_of(Some(target));
        let pane = match showing(&path).or(empty) {
            Some((pane, _)) => {
                let pane = *pane;
                self.model
                    .update(|model| _ = model.set_pane_plugin(pane, shown));
                self.remake_pane(pane);
                pane
            }
            None => {
                let mut opened = None;
                self.model_changed(|model| {
                    opened = match split {
                        Some((session, axis)) => {
                            model.split_session_with_view(session, shown, axis, activate)
                        }
                        None => Some(model.open_tab_with_view_activated(shown, activate)),
                    };
                });
                self.reconcile();
                opened.ok_or("unknown split target session")?
            }
        };
        if activate {
            self.change(|model| _ = model.reveal_pane(pane));
            self.focus_active();
        }
        Ok(EnvelopeBody::Ok { session_id: None })
    }

    /// Sends an agent session a command.
    fn tell(&self, session: SessionId, command: AgentCommand) -> Result<EnvelopeBody, String> {
        let kept = self.kept();
        let agents = kept.agents.borrow();
        let session = agents.get(&session).ok_or("unknown session")?;
        session.send(command);
        Ok(EnvelopeBody::Ok { session_id: None })
    }

    fn query(&self, query: &Query) -> EnvelopeBody {
        match query.what.as_str() {
            "extensions" => self.extensions_body(),
            "sessions" => EnvelopeBody::Sessions(Sessions {
                sessions: self.model.with_untracked(|model| {
                    (model.session_summaries().into_iter())
                        .map(|session| SessionEntry {
                            session_id: session.id.as_uuid().to_string(),
                            kind: session.kind.label().to_string(),
                            attached: session.attached,
                            title: session.title,
                            activity: session.activity.map(super::activity::entry),
                        })
                        .collect()
                }),
            }),
            "state" => {
                let state = self.command_state();
                // (What the agent on the cursor's pane is doing.)
                let standing = (self.active_agent())
                    .map(|session| session.standing())
                    .unwrap_or_default();
                EnvelopeBody::State(State {
                    tab_count: state.tab_count,
                    visible_pane_count: state.visible_pane_count,
                    has_active_session: state.has_active_session,
                    detached_session_count: state.detached_session_count,
                    has_pending_approval: standing.pending_approval,
                    has_turn_in_flight: standing.turn_in_flight,
                    destructive_commands: destructive_commands(),
                })
            }
            "workspace" => EnvelopeBody::Workspace(Snapshot {
                snapshot: (self.model)
                    .with_untracked(horizon_workspace::snapshot::workspace_snapshot),
            }),
            other => error_body(format!("unknown query `{other}`")),
        }
    }
}

/// The destructive commands by their external names.
fn destructive_commands() -> Vec<String> {
    (core_commands().into_iter())
        .filter(|spec| spec.destructive)
        .filter_map(|spec| match spec.id {
            CommandId::TerminateAllDetachedSessions => Some("terminate-all-detached".to_string()),
            _ => None,
        })
        .chain(std::iter::once("terminate-session".to_string()))
        .collect()
}
