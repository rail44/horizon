//! The agent runtime as this shell uses it: a `horizon-agentd` of its
//! own, the sessions started in it, and what the daemon asks of the shell
//! or tells it outside any session (a host tool to answer, the directory
//! a session came to work in).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::{Receiver, Sender};
use horizon_acp::HostToolRequest;
use horizon_agent::contract::SessionId as AgentSessionId;
use horizon_agent::registry::ProviderRegistry;
use horizon_shell_runtime::{AgentdHandle, WorkspaceRootUpdate, wait_for_drain};
use horizon_ui::reactive_graph::traits::{Update, WithUntracked};
use horizon_workspace::SessionId;
use horizon_workspace::snapshot::workspace_snapshot;

use crate::agent::AgentSession;
use crate::workspace::{AgentRuntime, Models, RunningAgent, Shell, SpawnAgent};

enum Told {
    HostTool(HostToolRequest),
    Directory(AgentSessionId, WorkspaceRootUpdate),
}

type Wake = Arc<Mutex<Option<Waker>>>;

/// The runtime in use: `Reload Agent Runtime` puts another in its place.
#[derive(Clone)]
pub struct Agentd {
    handle: Arc<Mutex<AgentdHandle>>,
    socket: PathBuf,
    control_socket: PathBuf,
    tell: Sender<Told>,
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
        let (tell, told) = crossbeam_channel::unbounded();
        let waker = Wake::default();
        let handle = Self::connect(socket, control_socket, &tell, &waker);
        Self {
            handle: Arc::new(Mutex::new(handle)),
            socket: socket.to_path_buf(),
            control_socket: control_socket.to_path_buf(),
            tell,
            told,
            waker,
        }
    }

    fn connect(
        socket: &Path,
        control_socket: &Path,
        tell: &Sender<Told>,
        waker: &Wake,
    ) -> AgentdHandle {
        let (handle, host_tools, directories) = AgentdHandle::start(socket, control_socket);
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
        pass_on(
            directories,
            tell.clone(),
            waker.clone(),
            |(session, update)| Told::Directory(session, update),
        );
        handle
    }

    /// The runtime as it is now.
    pub fn handle(&self) -> AgentdHandle {
        self.handle.lock().unwrap().clone()
    }

    /// The sessions the runtime has.
    pub fn running(&self) -> Result<Vec<RunningAgent>, String> {
        let sessions = self.handle().session_list()?;
        Ok((sessions.into_iter())
            .map(|agent| RunningAgent {
                session: session_id(agent.session_id),
                directory: agent.workspace_root,
                parent: agent.parent_session_id.map(session_id),
            })
            .collect())
    }

    /// Ends the daemon and starts one in its place, which has the
    /// sessions the first kept. Waits for the first to be gone.
    pub fn restart(&self) {
        let old = self.handle();
        if old.begin_reload()
            && let Err(error) = wait_for_drain(&self.socket)
        {
            eprintln!("horizon-agentd did not drain cleanly: {error}");
        }
        old.stop_and_wait();
        let new = Self::connect(&self.socket, &self.control_socket, &self.tell, &self.waker);
        *self.handle.lock().unwrap() = new;
    }

    /// A session of the runtime, looked for until the runtime lists it:
    /// it says a session is made a moment before it lists it.
    fn listed(&self, session: SessionId) -> Result<Option<RunningAgent>, String> {
        for attempt in 0..20 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let found = (self.running()?.into_iter()).find(|agent| agent.session == session);
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// This runtime as the shell's reload command and board use it.
    pub fn runtime(&self) -> AgentRuntime {
        let (restarted, attached, organized, found) =
            (self.clone(), self.clone(), self.clone(), self.clone());
        AgentRuntime {
            restart: Arc::new(move |()| {
                restarted.restart();
                restarted.running()
            }),
            attach: Box::new(move |session| attached.attach(session)),
            organizer: Arc::new(move |root| {
                let made = organized.handle().ensure_board_organizer(root)?;
                (organized.listed(session_id(made))?)
                    .ok_or_else(|| "The board organizer is not available yet".to_string())
            }),
            find: Arc::new(move |session| {
                Ok(found
                    .running()?
                    .into_iter()
                    .find(|agent| agent.session == session))
            }),
        }
    }

    /// What starts an agent session of this runtime.
    pub fn spawn(&self) -> SpawnAgent {
        let runtime = self.clone();
        Box::new(move |starting| {
            AgentSession::attached(runtime.handle().start_session(
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
        AgentSession::attached(self.handle().attach_session(agent_id(session)))
    }

    /// Takes in what the daemon has told since the last call: for each
    /// poll of the page, with the waker that makes it poll again.
    pub fn answer(&self, waker: &Waker, shell: &Shell) {
        *self.waker.lock().unwrap() = Some(waker.clone());
        for told in self.told.try_iter() {
            match told {
                Told::HostTool(request) => {
                    let output = match request.tool_id.as_str() {
                        "workspace_snapshot" => shell.model.with_untracked(workspace_snapshot),
                        other => serde_json::json!({
                            "error": format!("unknown host tool `{other}`")
                        }),
                    };
                    (self.handle().responder()).respond_host_tool(&request.request_id, output);
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

/// The model picker's source: the daemon's own listings and switch.
pub struct RuntimeModels(pub Agentd);

impl Models for RuntimeModels {
    fn providers(&self) -> Result<Vec<horizon_acp::ProviderSummary>, String> {
        self.0.handle().list_providers()
    }

    fn models(&self, provider: &str) -> Result<Vec<String>, String> {
        (self.0.handle()).list_provider_models(provider.to_string())
    }

    fn switch(&self, session: SessionId, provider: &str, model: &str) -> Result<(), String> {
        (self.0.handle()).set_session_model(
            agent_id(session),
            provider.to_string(),
            model.to_string(),
        )
    }
}
