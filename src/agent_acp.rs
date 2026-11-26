//! The agent runtime as this shell uses it: `horizon-agent-acp` as its
//! child, the sessions started in it, and what the runtime tells the shell
//! outside any session (the directory a session came to work in).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::task::Waker;

use crossbeam_channel::{Receiver, Sender};
use horizon_shell_runtime::AgentProgram;
use horizon_shell_runtime::{AgentAcpHandle, RuntimeNews};
use horizon_ui::reactive_graph::traits::Update;
use horizon_workspace::SessionId;

use crate::agent::AgentSession;
use crate::workspace::{AgentRuntime, Models, RunningAgent, Shell, SpawnAgent};

enum Told {
    News(RuntimeNews),
}

type Wake = Arc<Mutex<Option<Waker>>>;

/// The agent runtime: `horizon-agent-acp`, the shell's child, for as long
/// as the window lives.
#[derive(Clone)]
pub struct AgentAcp {
    handle: AgentAcpHandle,
    told: Receiver<Told>,
    waker: Wake,
}

impl AgentAcp {
    /// Starts the runtime. Its sessions' commands reach this shell
    /// through `control_socket`; its diagnostics are appended to `log`.
    pub fn start(program: AgentProgram, control_socket: &Path, log: &Path) -> Self {
        let (tell, told) = crossbeam_channel::unbounded();
        let waker = Wake::default();
        let handle = Self::connect(program, control_socket, log, &tell, &waker);
        Self {
            handle,
            told,
            waker,
        }
    }

    fn connect(
        program: AgentProgram,
        control_socket: &Path,
        log: &Path,
        tell: &Sender<Told>,
        waker: &Wake,
    ) -> AgentAcpHandle {
        let (handle, news) = AgentAcpHandle::start(program, control_socket, log);
        // The runtime's stream is passed on to the UI thread, which is woken
        // to take it in.
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
        pass_on(news, tell.clone(), waker.clone(), Told::News);
        handle
    }

    pub fn handle(&self) -> AgentAcpHandle {
        self.handle.clone()
    }

    /// The sessions the runtime has.
    pub fn running(&self) -> Result<Vec<RunningAgent>, String> {
        let sessions = self.handle().session_list()?;
        Ok((sessions.into_iter())
            .map(|agent| RunningAgent {
                session: agent.session_id,
                directory: agent.workspace_root,
                parent: None,
            })
            .collect())
    }

    /// This runtime as the board uses it.
    pub fn runtime(&self) -> AgentRuntime {
        let (attached, found) = (self.clone(), self.clone());
        AgentRuntime {
            attach: Box::new(move |session| attached.attach(session)),
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
            AgentSession::attached(
                (runtime.handle()).start_session(starting.session, starting.directory),
            )
        })
    }

    /// A session of this runtime that is running already.
    pub fn attach(&self, session: SessionId) -> AgentSession {
        AgentSession::attached(self.handle().attach_session(session))
    }

    /// Takes in what the daemon has told since the last call: for each
    /// poll of the page, with the waker that makes it poll again.
    pub fn answer(&self, waker: &Waker, shell: &Shell) {
        *self.waker.lock().unwrap() = Some(waker.clone());
        for told in self.told.try_iter() {
            match told {
                Told::News(RuntimeNews::Directory(session, directory)) => {
                    shell.model.update(|model| {
                        model.set_session_directory(session, directory);
                    })
                }
                Told::News(RuntimeNews::Named { was, is }) => shell.rename_agent_session(was, is),
                Told::News(RuntimeNews::Unnamed { was, error }) => {
                    shell.agent_session_unnamed(was, &error)
                }
            }
        }
    }
}

/// The model picker's switch, through the runtime.
pub struct RuntimeModels(pub AgentAcp);

impl Models for RuntimeModels {
    fn set_option(&self, session: SessionId, config_id: &str, value: &str) -> Result<(), String> {
        (self.0.handle()).set_session_option(session, config_id.to_string(), value.to_string())
    }
}
