//! Horizon's agent, composed: the host (`horizon-agent-host`) with
//! Horizon's guidance, the board and the wiki, serving one ACP client
//! (`docs/agent-stdio-child-design.md`). Everything it keeps or reaches
//! outside itself is given to it ([`Composition`]): the binary
//! (`main.rs`) gives what its environment names, and a test the places
//! of its own, so a test can compose the agent in its own process and
//! talk to it over an in-memory stream.

mod guidance;

use std::path::PathBuf;
use std::sync::Arc;

use horizon_agent::registry::ProviderRegistry;
use horizon_agent_host::extension::Extensions;
use horizon_agent_host::hub::{self, flush_event_log_before_exit};
use horizon_agent_host::session::AgentdState;
use horizon_agent_host::startup::{StartupHooks, spawn_resume_task};
use horizon_agent_host::{outbox, providers};
use horizon_board_flow::{BoardFlow, BoardPlaces};
use horizon_wiki_flow::WikiFlow;
use tokio::io::{AsyncRead, AsyncWrite};

/// Where the agent keeps what it keeps, and the daemon it writes the
/// board through.
#[derive(Clone, Debug)]
pub struct Places {
    /// Horizon's data home: the knowledge, board and wiki stores are under
    /// it.
    pub data_home: PathBuf,
    /// The session logs, one file per session.
    pub sessions_dir: PathBuf,
    /// The one log every session shared before the logs were split, split
    /// into `sessions_dir` once; `None` when there is none to split.
    pub legacy_event_log: Option<PathBuf>,
    /// The DuckDB projection of the logs, when there is one.
    pub state_db: Option<PathBuf>,
    /// The socket of the log daemon that writes the board.
    pub logd_socket: PathBuf,
}

impl Places {
    /// Where the environment names them (`HORIZON_DATA_HOME`,
    /// `XDG_DATA_HOME`, `HORIZON_AGENT_SESSIONS_DIR`,
    /// `HORIZON_AGENT_STATE_DB`, `HORIZON_LOGD_SOCKET`), else their
    /// defaults.
    pub fn from_env() -> Self {
        let persistence = horizon_agent::config::AgentPersistenceConfig::from_env();
        Self {
            data_home: persistence.data_home,
            sessions_dir: persistence.sessions_dir,
            legacy_event_log: persistence.legacy_event_log,
            state_db: persistence.duckdb_path,
            logd_socket: horizon_wire::socket::default_logd_socket_path(),
        }
    }

    /// The places a test keeps under `dir` alone.
    pub fn under(dir: &std::path::Path) -> Self {
        Self {
            data_home: dir.join("data"),
            sessions_dir: dir.join("sessions"),
            legacy_event_log: None,
            state_db: Some(dir.join("agent-state.duckdb")),
            logd_socket: dir.join("logd.sock"),
        }
    }
}

/// What the agent is composed from.
pub struct Composition {
    /// The configuration, as `config.toml` gives it.
    pub config: horizon_config::RawConfig,
    /// The file the configuration is read from again when it changes;
    /// `None`: it is not watched.
    pub config_path: Option<PathBuf>,
    pub places: Places,
    /// The directory the agent starts in: the wiki of its project is
    /// watched for threads.
    pub start_dir: Option<PathBuf>,
    /// The control plane of the shell that started the agent, which
    /// `workspace_snapshot` asks.
    pub control_socket: Option<PathBuf>,
    /// Whether sessions record file checkpoints (and a fork gets a
    /// worktree).
    pub checkpoints: bool,
    /// Runs before the session logs are read at startup: a test holds the
    /// startup with it.
    pub before_resume: Option<Box<dyn FnOnce() + Send>>,
    /// Has the mock provider's slow turns wait on it once their first word
    /// is out: a test reattaches while a turn surely streams.
    pub hold_mock_streams: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Told when startup is refused (the logs cannot be taken or read).
    pub refused: Box<dyn FnOnce(anyhow::Error) + Send>,
}

/// Horizon's guidance, the board and the wiki. What they add to the agent
/// (prompt sections, roles, skills and tools) is carried on the agent
/// configuration (`RigAgentConfig::packages`) to the providers and every
/// session.
pub fn extensions(start_dir: Option<PathBuf>, places: &Places) -> Extensions {
    Extensions::new(vec![
        Arc::new(guidance::HorizonGuidance),
        Arc::new(BoardFlow::new(BoardPlaces {
            data_home: places.data_home.clone(),
            logd_socket: places.logd_socket.clone(),
        })),
        Arc::new(WikiFlow::new(start_dir, places.data_home.clone())),
    ])
}

/// The agent composed and started: its sessions are read and resumed on a
/// background task, and the outbox delivers its answers.
pub struct Agent {
    state: Arc<AgentdState>,
    _config_watch: Option<horizon_agent_host::config_watch::RecommendedWatcher>,
}

impl Agent {
    pub fn compose(composition: Composition) -> Self {
        let Composition {
            config,
            config_path,
            places,
            start_dir,
            control_socket,
            checkpoints,
            before_resume,
            hold_mock_streams,
            refused,
        } = composition;
        let extensions = extensions(start_dir, &places);
        let mut agent_config = providers::agent_config(&config);
        agent_config.rig.packages = extensions.packages();
        agent_config.persistence = horizon_agent::config::AgentPersistenceConfig {
            sessions_dir: places.sessions_dir.clone(),
            legacy_event_log: places.legacy_event_log.clone(),
            duckdb_path: places.state_db.clone(),
            data_home: places.data_home.clone(),
        };
        eprintln!("horizon-agent: starting (model={})", agent_config.rig.model);

        let mut providers = ProviderRegistry::builtin_with_config(agent_config.clone());
        if let Some(hold) = hold_mock_streams {
            providers.hold_mock_streams(hold);
        }
        let mut state = AgentdState::new(
            providers,
            agent_config.clone(),
            None,
            config_path,
            horizon_config::project_grants(&config),
            horizon_config::trusted_projects(&config),
        )
        .with_checkpoints(checkpoints)
        .with_extensions(extensions.clone());
        if let Some(socket) = control_socket {
            state = state.with_control_socket(socket);
        }
        let state = Arc::new(state);
        // The providers follow the configuration file.
        let config_watch = horizon_agent_host::config_watch::watch_config(state.clone());

        spawn_resume_task(
            state.clone(),
            agent_config,
            StartupHooks {
                before_resume,
                refused,
            },
        );
        outbox::spawn(
            state.clone(),
            extensions.trigger_sources(),
            extensions.reply_sinks(),
        );
        Self {
            state,
            _config_watch: config_watch,
        }
    }

    /// Serves one client on `read`/`write` until it closes the connection,
    /// then flushes the session logs: an append only enqueues, so ending
    /// without the flush would drop what the writer thread has not taken
    /// yet.
    pub async fn serve<R, W>(&self, read: R, write: W)
    where
        R: AsyncRead + Send + 'static,
        W: AsyncWrite + Send + 'static,
    {
        serve_until(read, write, self.state.clone(), std::future::pending()).await;
    }

    /// The agent's state, for what a test reads of it directly.
    pub fn state(&self) -> &Arc<AgentdState> {
        &self.state
    }
}

/// Serves the client on `read`/`write` until it closes the connection or
/// `stop` completes, then flushes the session logs.
pub async fn serve_until<R, W>(
    read: R,
    write: W,
    state: Arc<AgentdState>,
    stop: impl std::future::Future<Output = ()>,
) where
    R: AsyncRead + Send + 'static,
    W: AsyncWrite + Send + 'static,
{
    tokio::select! {
        served = hub::serve(read, write, state.clone()) => {
            if let Err(error) = served {
                eprintln!("horizon-agent: the connection failed: {error}");
            }
        }
        () = stop => eprintln!("horizon-agent: terminated"),
    }
    flush_event_log_before_exit(state.writer());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every skill a package adds parses: composing the registry panics on
    /// one that does not, which would stop every session at startup.
    #[test]
    fn every_skill_the_packages_add_parses() {
        let places = Places::under(&std::env::temp_dir());
        let _ =
            horizon_agent::skills::SkillRegistry::embedded(&extensions(None, &places).packages());
    }
}
