//! What the control plane asks of the shell: the external commands this
//! shell carries out so far, and the two queries.

use horizon_control::contract::{EnvelopeBody, Query, SessionEntry, Sessions, State};
use horizon_control::host::executor::{ControlRequest, error_body};
use horizon_shell_runtime::control_invoke::{self, Command};
use horizon_ui::reactive_graph::traits::WithUntracked;
use horizon_workspace::commands::{CommandId, core_commands};
use horizon_workspace::{PaneKind, SessionKind};

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

    fn invoke(&self, command: Command) -> Result<EnvelopeBody, String> {
        let done = Ok(EnvelopeBody::Ok { session_id: None });
        match command {
            Command::NewSession {
                kind: SessionKind::Terminal,
                split,
                issuer,
                activate,
                ..
            } => {
                let kept = self.kept();
                let mut made = None;
                self.model_changed(|model| {
                    made =
                        match split {
                            Some((target, axis)) => model.split_session_with_new_session(
                                target,
                                SessionKind::Terminal,
                                axis,
                                activate,
                            ),
                            None => Some(model.open_tab_with_new_session_activated(
                                SessionKind::Terminal,
                                activate,
                            )),
                        };
                    // Its shell starts where the session split, or else
                    // the one that asked, is: if that is a terminal.
                    let from = (split.map(|(target, _)| target).or(issuer))
                        .filter(|from| model.session_pane_kind(*from) == Some(PaneKind::Terminal));
                    if let (Some(made), Some(from)) = (made, from) {
                        (kept.starting.borrow_mut()).insert(made, Start::Beside(from));
                    }
                });
                let made = made.ok_or("unknown split target session")?;
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
            _ => Err(NOT_YET.to_string()),
        }
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
