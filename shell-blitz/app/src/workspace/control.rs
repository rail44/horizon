//! What the control plane asks of the shell: the external commands this
//! shell carries out so far (terminals, agents, sessions), and the two
//! queries.

use horizon_control::contract::{EnvelopeBody, Query, SessionEntry, Sessions, State};
use horizon_control::host::executor::{ControlRequest, error_body};
use horizon_shell_runtime::AgentCommand;
use horizon_shell_runtime::control_invoke::{self, Command};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::commands::{CommandId, core_commands};
use horizon_workspace::{SessionId, SessionKind, ViewKind};

use super::commands::CARRIED_OUT;
use super::{Shell, Start};

/// The answer to a command this shell has no view or runtime for yet.
const NOT_YET: &str = "the Blitz shell does not carry this command out yet";

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
        if let ControlRequest::Invoke(invoke) = request
            && let Ok(Command::SetModel {
                session_id,
                provider,
                model,
            }) = control_invoke::parse(invoke)
        {
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
            return;
        }
        reply(self.control(request));
    }

    fn invoke(&self, command: Command) -> Result<EnvelopeBody, String> {
        let done = Ok(EnvelopeBody::Ok { session_id: None });
        match command {
            Command::NewSession {
                kind,
                role_id,
                split,
                issuer,
                activate,
                prompt,
            } => {
                let kept = self.kept();
                if kind == SessionKind::Agent && kept.spawn_agent.is_none() {
                    return Err(NOT_YET.to_string());
                }
                let mut made = None;
                self.model_changed(|model| {
                    made = match split {
                        Some((target, axis)) => {
                            model.split_session_with_new_session(target, kind, axis, activate)
                        }
                        None => Some(model.open_tab_with_new_session_activated(kind, activate)),
                    };
                    // It starts where the session split, or else the one
                    // that asked, is.
                    if let Some(made) = made {
                        let start = Start {
                            from: split.map(|(target, _)| target).or(issuer),
                            directory: None,
                            role: role_id,
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
            Command::Terminate(session) => {
                let mut ended = false;
                self.model_changed(|model| ended = model.terminate_session(session));
                match ended {
                    true => done,
                    false => Err("unknown session".to_string()),
                }
            }
            Command::TerminateAllDetached => {
                self.execute(CommandId::TerminateAllDetachedSessions);
                done
            }
            Command::Execute(command) if CARRIED_OUT.contains(&command) => {
                self.execute(command);
                done
            }
            Command::Markdown {
                path,
                split,
                activate,
            } => self.open_document(ViewKind::Markdown, path, split, activate),
            Command::Wiki {
                path,
                split,
                activate,
            } => self.open_document(ViewKind::Wiki, path, split, activate),
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

    /// A pane on the file at `path`, shown as `kind` shows one: the pane
    /// that shows it already, or a new one.
    fn open_document(
        &self,
        kind: ViewKind,
        path: std::path::PathBuf,
        split: Option<(SessionId, horizon_workspace::SplitAxis)>,
        activate: bool,
    ) -> Result<EnvelopeBody, String> {
        if !path.is_file() {
            return Err(format!("not a file: {}", path.display()));
        }
        // A pane that shows the file already reads it again.
        let shown = (self.model).with_untracked(|model| model.document_pane(kind, &path));
        if let Some(pane) = shown {
            match kind {
                ViewKind::Wiki => {
                    if let Some(view) = self.wiki(pane) {
                        view.open(path);
                    }
                }
                _ => {
                    if let Some(view) = self.markdown(pane) {
                        view.open(path);
                    }
                }
            }
            if activate {
                self.change(|model| {
                    if let Some((tab, pane)) = model.pane_position(pane) {
                        model.activate_pane_index(tab, pane);
                    }
                });
            }
            return Ok(EnvelopeBody::Ok { session_id: None });
        }
        let mut opened = None;
        self.model_changed(|model| {
            opened = match split {
                Some((target, axis)) => model.split_session_with_view(target, kind, axis, activate),
                None => Some(model.open_tab_with_view_activated(kind, activate)),
            };
            if let Some(pane) = opened {
                model.set_pane_document(pane, path);
            }
        });
        // (The model has the pane's file before its view is made.)
        self.reconcile();
        opened.ok_or("unknown split target session")?;
        if activate {
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
            "sessions" => EnvelopeBody::Sessions(Sessions {
                sessions: self.model.with_untracked(|model| {
                    (model.session_summaries().into_iter())
                        .map(|session| SessionEntry {
                            session_id: session.id.as_uuid().to_string(),
                            kind: session.kind.label().to_string(),
                            attached: session.attached,
                            title: session.title,
                        })
                        .collect()
                }),
            }),
            "state" => {
                let state = self.command_state();
                EnvelopeBody::State(State {
                    tab_count: state.tab_count,
                    visible_pane_count: state.visible_pane_count,
                    has_active_session: state.has_active_session,
                    detached_session_count: state.detached_session_count,
                    has_pending_approval: state.has_pending_approval,
                    has_turn_in_flight: state.has_turn_in_flight,
                    destructive_commands: destructive_commands(),
                })
            }
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
