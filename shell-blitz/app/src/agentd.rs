//! The agent runtime as this shell uses it: a `horizon-agentd` of its
//! own, the sessions started in it, and what the daemon asks of the shell
//! or tells it outside any session (a host tool to answer, the directory
//! a session came to work in).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::Receiver;
use horizon_acp::HostToolRequest;
use horizon_agent::contract::SessionId as AgentSessionId;
use horizon_agent::registry::ProviderRegistry;
use horizon_shell_runtime::{AgentdHandle, WorkspaceRootUpdate};
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::SessionId;
use horizon_workspace::snapshot::workspace_snapshot;

use crate::agent::AgentSession;
use crate::workspace::{Shell, SpawnAgent};

enum Told {
    HostTool(HostToolRequest),
    Directory(AgentSessionId, WorkspaceRootUpdate),
}

type Wake = Arc<Mutex<Option<Waker>>>;

pub struct Agentd {
    pub handle: AgentdHandle,
    told: Receiver<Told>,
    waker: Wake,
}

fn agent_id(session: SessionId) -> AgentSessionId {
    AgentSessionId::from_uuid(session.as_uuid())
}

fn session_id(session: AgentSessionId) -> SessionId {
    SessionId::from_uuid(session.as_uuid())
}

impl Agentd {
    /// Connects to the daemon on `socket`, starting it if it is not
    /// running.
    pub fn start(socket: &Path, control_socket: &Path) -> Self {
        let (handle, host_tools, directories) = AgentdHandle::start(socket, control_socket);
        let (tell, told) = crossbeam_channel::unbounded();
        let waker = Wake::default();
        // Each of the daemon's two streams is passed on to the UI thread,
        // which is woken to take it in.
        fn pass_on<T: Send + 'static>(
            from: Receiver<T>,
            to: crossbeam_channel::Sender<Told>,
            waker: Wake,
            told: fn(T) -> Told,
        ) {
            std::thread::spawn(move || {
                for item in from {
                    if to.send(told(item)).is_err() {
                        return;
                    }
                    if let Some(waker) = waker.lock().unwrap().as_ref() {
                        waker.wake_by_ref();
                    }
                }
            });
        }
        pass_on(host_tools, tell.clone(), waker.clone(), Told::HostTool);
        pass_on(directories, tell, waker.clone(), |(session, update)| {
            Told::Directory(session, update)
        });
        Self {
            handle,
            told,
            waker,
        }
    }

    /// What starts an agent session of this runtime.
    pub fn spawn(&self) -> SpawnAgent {
        let handle = self.handle.clone();
        Box::new(move |starting| {
            AgentSession::attached(handle.start_session(
                agent_id(starting.session),
                ProviderRegistry::default().default_provider_id(),
                starting.role,
                starting.directory,
                starting.from.map(agent_id),
            ))
        })
    }

    /// A session of this runtime that is running already.
    pub fn attach(&self, session: SessionId) -> AgentSession {
        AgentSession::attached(self.handle.attach_session(agent_id(session)))
    }

    /// Takes in what the daemon has told since the last call: for each
    /// poll of the page, with the waker that makes it poll again.
    pub fn answer(&self, waker: &Waker, shell: &Shell) {
        *self.waker.lock().unwrap() = Some(waker.clone());
        for told in self.told.try_iter() {
            match told {
                Told::HostTool(request) => {
                    let output = match request.tool_id.as_str() {
                        "workspace.snapshot" => shell.model.with_untracked(workspace_snapshot),
                        other => serde_json::json!({
                            "error": format!("unknown host tool `{other}`")
                        }),
                    };
                    (self.handle.responder()).respond_host_tool(&request.request_id, output);
                }
                Told::Directory(session, update) => shell.model.update(|model| {
                    let session = session_id(session);
                    model.set_session_directory(session, update.workspace_root);
                    if let Some(parent) = update.parent_session_id {
                        model.set_session_parent(session, session_id(parent));
                    }
                }),
            }
        }
    }
}
