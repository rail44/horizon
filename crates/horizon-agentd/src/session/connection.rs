//! One connection's view onto the process-lifetime session state: the
//! handlers behind the hub's session-scoped control calls.

use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::{Command, Event, SessionId};
use horizon_agent::persistence::event_log::WriterHandle;
use horizon_agent::wire::{
    AgentWireEvent, HostToolRequest, HostToolResponse, ModelAlias, ProviderSummary, SessionNew,
    SessionSummary,
};
use tokio::sync::mpsc::UnboundedSender;

use super::events::send_session_event;
use super::spawn::spawn_session_thread;
use super::state::{lock_unpoisoned, AgentdState};

/// How long [`Connection::replay_events`] waits for a live session's own
/// thread to answer a replay request. **Not** purely a local channel hop:
/// a just-resumed session's thread does real work before it ever reaches
/// the loop that drains the `replay` channel, including blocking on
/// [`AgentdState::wait_for_duckdb_store`] -- which is deliberately *not*
/// ordered against [`AgentdState::mark_resume_ready`] (`Control::
/// SessionList`/`SessionLoad`'s own readiness gate), so a client can see a
/// resumed session as "listed" before its thread has gotten anywhere near
/// this channel. Under real contention (many agentd processes competing
/// for CPU/disk, e.g. the full workspace test suite running in parallel)
/// that DuckDB rebuild-or-open wait can genuinely take several seconds,
/// and a timeout here has no way to distinguish "thread not there yet"
/// from "session truly has no history" -- it silently falls back to an
/// empty `Vec` either way (see the call site). A production `session_load`
/// racing this hard would misreport a real session as empty, so this is
/// sized generously to make that misfire vanishingly rare while still
/// bounding a genuinely wedged session thread. (Originally 5s -- too tight
/// under load, see `docs/tasks/backlog.md` #27. This crate's e2e tests
/// independently hit a comparable real-PTY stall past 60s under a
/// deliberately extreme concurrent `cargo build --release` loop during that
/// fix's own validation -- see `TERMINAL_UPDATE_TIMEOUT`'s doc comment in
/// `tests/e2e.rs` -- so this is sized with the same margin.)
const REPLAY_TIMEOUT: Duration = Duration::from_secs(120);

/// One connection's view onto the process-lifetime [`AgentdState`] — thin by
/// design (step 4): every map that used to live here moved to `AgentdState`
/// so sessions survive a reconnect, leaving `Connection` as just the `Arc`
/// handle plus the methods that make sense scoped to "the current
/// connection" (installing/clearing `outgoing`).
#[derive(Clone)]
pub(crate) struct Connection {
    state: Arc<AgentdState>,
}

impl Connection {
    pub(crate) fn new(state: Arc<AgentdState>) -> Self {
        Self { state }
    }

    pub(crate) fn register_board(&self, root: std::path::PathBuf) -> Result<(), String> {
        let root =
            crate::worktree::project_root(&root).ok_or("Board root is not a Git repository")?;
        self.state.register_board(root);
        Ok(())
    }

    /// Every configured provider with its model aliases and availability —
    /// the model picker's data. Reads the agent config (the same table the
    /// registry was built from), so it reflects the loaded `[[providers]]`
    /// surface, or the legacy `[provider]` fold-in when the file has none;
    /// entries run in the config file's order, aliases in their own listing
    /// order. `available` is the build-time-resolved key presence — the
    /// same rule the registry itself follows; a mid-session environment
    /// change is honored by a *switch*, not by this listing.
    pub(crate) fn list_providers(&self) -> Vec<ProviderSummary> {
        let config = lock_unpoisoned(&self.state.agent_config);
        let table = &config.providers;
        let mut summaries: Vec<ProviderSummary> = table
            .entries
            .iter()
            .map(|entry| ProviderSummary {
                name: entry.name.clone(),
                base_url: entry.base_url.clone(),
                api_key_env: entry.api_key_env.clone(),
                models: entry
                    .models
                    .iter()
                    .map(|(alias, model)| ModelAlias {
                        alias: alias.clone(),
                        model: model.clone(),
                    })
                    .collect(),
                available: entry.api_key_present,
                default: entry.name == table.default_name,
            })
            .collect();
        // The `[[moa]]` entries ride the same shape as one more group whose
        // "models" are the entry names, so the picker and
        // `set_session_model` need no MoA-specific wire surface. Omitted
        // entirely when nothing is configured.
        if !config.moa.entries.is_empty() {
            // One flag for the group, as `ProviderSummary` carries: false
            // when no entry could run at all, which grays the group out the
            // way a key-less provider entry is grayed out. A group with some
            // usable entries stays selectable and an unusable entry inside
            // it is refused by `set_session_model`.
            let available = config
                .moa
                .entries
                .iter()
                .any(|entry| entry.aggregator.api_key_present);
            summaries.push(ProviderSummary {
                name: horizon_agent::config::MOA_PROVIDER_NAME.to_string(),
                base_url: None,
                api_key_env: String::new(),
                models: config
                    .moa
                    .entries
                    .iter()
                    .map(|entry| ModelAlias {
                        alias: entry.name.clone(),
                        model: entry.name.clone(),
                    })
                    .collect(),
                available,
                default: false,
            });
        }
        summaries
    }

    /// Applies a mid-session provider/model switch (latest turn wins):
    /// validates the pair against the current surface, resolves the model
    /// id (alias first — the picker only offers aliases — else the raw id,
    /// the same pass-through `role.model` accepts), records the resolved id
    /// on the session (so a (re)attach re-announces the switched model),
    /// forwards `Command::SetSessionModel` to the session thread (its next
    /// turn builds with the target entry), and re-announces
    /// [`AgentWireEvent::SessionModel`] — the owner-agreed design's
    /// "resolution results ride the existing announcement".
    ///
    /// The command is forwarded before the announcement so a turn that
    /// starts on the switch reports the switched model in its own
    /// `ProviderRequestSent`, not the previous one.
    pub(crate) fn set_session_model(
        &self,
        session_id: SessionId,
        provider: String,
        model: String,
    ) -> Result<(), String> {
        let resolved = {
            let config = lock_unpoisoned(&self.state.agent_config);
            if model.is_empty() {
                return Err("A model id is required.".to_string());
            }
            if provider == horizon_agent::config::MOA_PROVIDER_NAME {
                // The announced model is the aggregator's: it is what the
                // session's provider requests actually name.
                let Some(entry) = config.moa.entry(&model) else {
                    return Err(format!("Unknown moa entry `{model}`."));
                };
                // Refused here as well as in the session loop, so the caller
                // gets the reason back from the call instead of only seeing
                // an error event on the session.
                if !entry.aggregator.api_key_present {
                    return Err(format!(
                        "moa entry `{model}` is unavailable: its `{}` provider's key variable \
                         {} is not set.",
                        entry.aggregator.provider, entry.aggregator.api_key_env
                    ));
                }
                entry.aggregator.model.clone()
            } else {
                let Some(entry) = config.providers.entry(&provider) else {
                    return Err(format!("Unknown provider `{provider}`."));
                };
                entry
                    .models
                    .iter()
                    .find(|(alias, _)| alias == &model)
                    .map(|(_, id)| id.clone())
                    .unwrap_or_else(|| model.clone())
            }
        };
        let inbound = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let Some(entry) = sessions.get_mut(&session_id) else {
                return Err(format!("Unknown session {session_id:?}."));
            };
            entry.model = Some(resolved.clone());
            entry.inbound.clone()
        };
        let _ = inbound.send(Command::SetSessionModel {
            provider,
            model: model.clone(),
        });
        send_session_event(
            &self.state,
            session_id,
            AgentWireEvent::SessionModel(resolved),
        );
        Ok(())
    }

    pub(crate) fn ensure_board_organizer(
        &self,
        root: std::path::PathBuf,
    ) -> Result<SessionId, String> {
        self.register_board(root.clone())?;
        crate::board_flow::organizer_session(&self.state, &root)
    }

    /// Installs the current connection's host-tool bridge (the local half
    /// behind `HubHello::host_tools`) — the connection-global counterpart
    /// of the per-attachment subscribers [`Self::subscribe_agent`] installs.
    pub(crate) fn connect_host_tools(&self, outgoing: UnboundedSender<HostToolRequest>) {
        *self.state.host_tools_outgoing.lock().unwrap() = Some(outgoing);
    }

    /// Clears the connection-global host-tool bridge on disconnect, so a
    /// session thread's `execute_auto` fails fast instead of enqueueing
    /// into a bridge whose pump already died with the connection. The
    /// per-session subscribers are deliberately *not* swept here: each
    /// attachment's bridge dies with its own pump, and
    /// [`send_session_event`] already drops an entry lazily on its first
    /// failed send (a fresh attach replaces it anyway).
    pub(crate) fn disconnect(&self) {
        *self.state.host_tools_outgoing.lock().unwrap() = None;
    }

    /// Subscribes an attachment to `session_id`'s wire events, replacing
    /// any previous attachment's subscription (one client connection at a
    /// time; a re-attach supersedes). Returns the local receiving half the
    /// hub pumps into the attachment's remote channel.
    pub(crate) fn subscribe_agent(
        &self,
        session_id: SessionId,
    ) -> tokio::sync::mpsc::UnboundedReceiver<AgentWireEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        lock_unpoisoned(&self.state.agent_subscribers).insert(session_id, tx);
        rx
    }

    /// Pushes a session-scoped wire event to the session's current
    /// subscriber, if any — the hub's own send path (replay, model
    /// re-announcement), same semantics as every session thread's sends.
    pub(crate) fn send_session_event(&self, session_id: SessionId, event: AgentWireEvent) {
        send_session_event(&self.state, session_id, event);
    }

    /// Spawns the session thread for a `Control::SessionNew`. Reuses the
    /// crate's existing spawn shape (`ProviderRegistry::start_session`) --
    /// the same call the deleted in-process agent runtime used to make
    /// before every agent session moved here.
    pub(crate) fn handle_session_new(&self, new: SessionNew) {
        spawn_session_thread(
            self.state.clone(),
            new.session_id,
            new.provider_id,
            new.role_id,
            new.workspace_root,
            new.spawn_source_session_id,
            new.isolate,
            None,
            Vec::new(),
        );
    }

    /// Routes a `Command` envelope scoped to `session_id` to that session's
    /// thread. A miss (unknown session id -- stale/mistargeted envelope) is
    /// logged and dropped rather than panicking.
    pub(crate) fn route_command(&self, session_id: SessionId, command: Command) {
        if !self.state.send_command(session_id, command) {
            eprintln!("horizon-agentd: command for unknown session {session_id:?}");
        }
    }

    /// Routes an incoming `Control::HostToolResponse` back to whichever
    /// session thread's `host_tools::AgentdHostTools::execute_auto` call is blocked
    /// waiting for this exact `request_id`.
    pub(crate) fn handle_host_tool_response(&self, response: HostToolResponse) {
        let sender = self
            .state
            .pending_host_tool_requests
            .lock()
            .unwrap()
            .remove(&response.request_id.0);
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }

    /// Delegates to [`AgentdState::wait_until_resume_ready`] -- see `main`'s
    /// bind-first startup fix: `Control::SessionList`/`Control::SessionLoad`
    /// must block on this before answering, so a client that connects while
    /// `resume_persisted_sessions` is still running doesn't see an
    /// incomplete (or, right after bind, empty) session list.
    pub(crate) async fn wait_until_resume_ready(&self) {
        self.state.wait_until_resume_ready().await;
    }

    /// Delegates to [`AgentdState::skipped_lines_summary`] -- see the hub's
    /// `hello` (`crate::hub`), which waits for [`Self::wait_until_resume_ready`]
    /// first so this always reflects the finished startup read.
    pub(crate) fn skipped_lines_summary(&self) -> Option<String> {
        self.state.skipped_lines_summary()
    }

    /// Delegates to [`AgentdState::reload_provider_config`] -- the
    /// daemon-side half of a `Reload Config`: rebuild the provider
    /// registry/agent config from the config file without respawning the
    /// process. See that method's doc comment for the granularity.
    pub(crate) fn reload_provider_config(&self) -> Result<(), String> {
        self.state.reload_provider_config()
    }

    /// Every session a client may see. Exploration sessions
    /// (`docs/agent-explore-design.md` decision 3: "invisible to the UI")
    /// are withheld: they are never attached to a pane, they live only as
    /// long as the `task` call waiting on them, and offering one
    /// in the session manager's attach list would invite a user into a
    /// read-only session that is about to be terminated under them. They
    /// remain fully first-class in the event log and DuckDB projection,
    /// which is where their cost is actually measured.
    pub(crate) fn session_list(&self) -> Vec<SessionSummary> {
        self.state
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, entry)| {
                !entry
                    .role_id
                    .as_ref()
                    .is_some_and(horizon_agent::roles::is_exploration)
            })
            .map(|(session_id, entry)| SessionSummary {
                session_id: *session_id,
                provider_id: entry.provider_id.clone(),
                role_id: entry.role_id.clone(),
                parent_session_id: entry.parent_session_id,
                workspace_root: entry.workspace_root.clone(),
            })
            .collect()
    }

    /// This session's resolved model id, if any -- see [`super::state::SessionEntry::model`]'s
    /// doc comment. `None` for an unknown `session_id` too (a stale/racing
    /// `session_load`), same "nothing to report" shape [`Self::session_list`]
    /// uses for a missing entry.
    pub(crate) fn session_model(&self, session_id: SessionId) -> Option<String> {
        self.state
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .and_then(|entry| entry.model.clone())
    }

    /// Delegates to [`AgentdState::writer`] -- the hub's `drain` uses this
    /// to flush the event log's writer channel to disk before the process
    /// exits (`crate::run`'s SIGTERM arm does the same, straight off
    /// [`AgentdState::writer`], since it has no connection in hand). An
    /// `append` returning only means a record was *enqueued*; the writer's
    /// background thread is what actually writes and flushes it (see
    /// `WriterHandle::open`'s "Ordering guarantee" doc comment), and
    /// forwarding an event to this connection over the wire happens after
    /// that same enqueue, not after it's durable. Without this, a client
    /// that drains right after observing a session's latest event over the
    /// wire could still race the writer and lose it -- unlike a `kill -9`,
    /// an exit this process actually gets to run code on has no excuse to
    /// ever do that.
    pub(crate) fn writer(&self) -> Option<WriterHandle> {
        self.state.writer()
    }

    /// Handles `Control::SessionLoad`: asks `session_id`'s own thread (if
    /// live) to hand back everything its `LiveState::events()` has
    /// accumulated -- already-committed history plus anything folded in
    /// since -- so the caller (the hub's `attach_agent`) can forward it to
    /// the requesting client as ordinary session events. Per the
    /// design's "v1 bootstrap" note, this is exactly the events list, not a
    /// server-side frame snapshot (a later optimization). An unknown
    /// session id resolves to an empty list rather than an error -- nothing
    /// to replay.
    ///
    /// Runs the actual wait on a `spawn_blocking` thread rather than
    /// blocking this async call's caller directly, so a slow (or wedged)
    /// session thread can't stall this connection's envelope-reading loop
    /// for unrelated traffic.
    pub(crate) async fn replay_events(&self, session_id: SessionId) -> Vec<Event> {
        let replay_tx = self
            .state
            .sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(|entry| entry.replay.clone());
        let Some(replay_tx) = replay_tx else {
            return Vec::new();
        };

        tokio::task::spawn_blocking(move || {
            let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
            if replay_tx.send(reply_tx).is_err() {
                return Vec::new();
            }
            reply_rx.recv_timeout(REPLAY_TIMEOUT).unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::SessionEntry;
    use crate::session::test_support::{judge_test_state, state_with_rig_config};
    use crossbeam_channel::{unbounded, Sender};
    use horizon_agent::config::{NamedProviderConfig, ProviderKind, ProvidersTable};
    use horizon_agent::contract::ProviderId;
    use horizon_agent::persistence::projection::duckdb::SharedDuckdbStore;
    use horizon_agent::registry::ProviderRegistry;
    use horizon_agent::roles::RoleId;
    use std::sync::Arc;

    /// Decision 3, "invisible to the UI": a live exploration session is
    /// hosted exactly like any other, but is withheld from the client's
    /// session list so it can never be offered as something to attach to.
    #[test]
    fn a_live_exploration_session_is_withheld_from_the_client_session_list() {
        let state = judge_test_state();
        let provider_id = ProviderId("builtin.agent.mock".to_string());
        let explore_id = SessionId::new();
        let ordinary_id = SessionId::new();

        spawn_session_thread(
            state.clone(),
            explore_id,
            provider_id.clone(),
            Some(RoleId(horizon_agent::roles::EXPLORE_ROLE_ID.to_string())),
            None,
            None,
            false,
            None,
            Vec::new(),
        );
        spawn_session_thread(
            state.clone(),
            ordinary_id,
            provider_id,
            None,
            None,
            None,
            false,
            None,
            Vec::new(),
        );

        assert!(
            state.sessions.lock().unwrap().contains_key(&explore_id),
            "the exploration session is still a first-class hosted session"
        );
        let listed: Vec<SessionId> = Connection::new(state)
            .session_list()
            .into_iter()
            .map(|summary| summary.session_id)
            .collect();
        assert!(!listed.contains(&explore_id), "{listed:?}");
        assert!(listed.contains(&ordinary_id), "{listed:?}");
    }

    /// [`Connection::session_model`] answers from whatever
    /// [`resolve_and_announce_session_model`] stored on the session's
    /// `SessionEntry` -- the read side of the same "attach re-announces it"
    /// path `Control::SessionLoad`'s handler uses.
    #[test]
    fn connection_session_model_reads_the_stored_value_for_a_known_session_only() {
        let state = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let (inbound_tx, _inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("stored-model".to_string()),
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: None,
                worktree: None,
            },
        );

        let connection = Connection {
            state: state.clone(),
        };
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("stored-model")
        );
        assert_eq!(connection.session_model(SessionId::new()), None);
    }

    /// [`Connection::session_list`] must report the authoritative,
    /// post-isolation `workspace_root` from the session's own `SessionEntry`
    /// -- the wire-level counterpart of the state-level assertion above,
    /// and the coordinator's requested regression guard: the workspace
    /// model on the Horizon side reads exactly this field to correct its
    /// own pre-spawn value (`WorkspaceShell::spawn_agent_resume`/
    /// `spawn_workspace_restore`).
    #[test]
    fn session_list_reports_the_entrys_workspace_root_and_parent() {
        let state = state_with_rig_config(true, "test-model");
        let session_id = SessionId::new();
        let parent_id = SessionId::new();
        let (inbound_tx, _inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: None,
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: Some(parent_id),
                workspace_root: Some(std::path::PathBuf::from(
                    "/tmp/repo/.horizon/worktrees/abcd1234",
                )),
                worktree: None,
            },
        );

        let connection = Connection { state };
        let summaries = connection.session_list();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, session_id);
        assert_eq!(summaries[0].parent_session_id, Some(parent_id));
        assert_eq!(
            summaries[0].workspace_root,
            Some(std::path::PathBuf::from(
                "/tmp/repo/.horizon/worktrees/abcd1234"
            ))
        );
    }
    /// A hermetic two-provider surface (never `from_env_and_provider`'s
    /// real env reads): one openai-compatible entry with its key present,
    /// one anthropic entry with its key unset — the two-provider
    /// completion condition's state-level shape.
    fn two_provider_state() -> (Arc<AgentdState>, Vec<NamedProviderConfig>) {
        two_provider_state_with_moa(horizon_agent::config::MoaTable::default())
    }

    fn two_provider_state_with_moa(
        moa: horizon_agent::config::MoaTable,
    ) -> (Arc<AgentdState>, Vec<NamedProviderConfig>) {
        let entries = vec![
            NamedProviderConfig {
                name: "openai".to_string(),
                kind: ProviderKind::OpenAiCompatible,
                base_url: None,
                api_key_env: "OPENAI_API_KEY".to_string(),
                api_key_present: true,
                models: vec![("fast".to_string(), "m-fast".to_string())],
            },
            NamedProviderConfig {
                name: "claude".to_string(),
                kind: ProviderKind::Anthropic,
                base_url: None,
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                api_key_present: false,
                models: vec![("opus".to_string(), "m-opus".to_string())],
            },
        ];
        let agent_config = horizon_agent::config::AgentConfig {
            rig: horizon_agent::config::RigAgentConfig {
                api_key_present: true,
                model: "test-model".to_string(),
                ..Default::default()
            },
            providers: ProvidersTable {
                entries: entries.clone(),
                default_name: "openai".to_string(),
            },
            moa,
            persistence: horizon_agent::config::AgentPersistenceConfig {
                event_log_path: std::path::PathBuf::from(
                    "/tmp/horizon-connection-test-events.jsonl",
                ),
                duckdb_path: None,
            },
            tools: horizon_agent::config::AgentToolsConfig::default(),
        };
        let state = Arc::new(AgentdState::new(
            ProviderRegistry::builtin_with_config(
                agent_config.clone(),
                SharedDuckdbStore::unavailable(),
            ),
            agent_config,
            None,
            SharedDuckdbStore::unavailable(),
            None,
            Vec::new(),
            Vec::new(),
        ));
        (state, entries)
    }

    /// `list_providers` reports every configured provider — both of them,
    /// with the key-less one registered but unavailable (`available:
    /// false`, never dropped) — in file order, with the default flagged.
    #[test]
    fn list_providers_reports_both_entries_and_marks_the_unavailable_one() {
        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        let summaries = connection.list_providers();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].name, "openai");
        assert!(summaries[0].available);
        assert!(summaries[0].default);
        assert_eq!(
            summaries[0].models,
            vec![horizon_agent::wire::ModelAlias {
                alias: "fast".to_string(),
                model: "m-fast".to_string(),
            }]
        );
        assert_eq!(summaries[1].name, "claude");
        assert!(!summaries[1].available);
        assert!(!summaries[1].default);
        // The key variable's NAME is reported (never a value), so a picker
        // can explain unavailability.
        assert_eq!(summaries[1].api_key_env, "ANTHROPIC_API_KEY");
    }

    /// `set_session_model` resolves the alias, records the resolved model
    /// on the session, forwards `Command::SetSessionModel` to the session
    /// thread (the next turn builds with it), and re-announces
    /// `SessionModel` — the owner-agreed "resolution rides the existing
    /// announcement".
    #[test]
    fn set_session_model_resolves_announces_and_forwards_the_switch() {
        let (state, _entries) = two_provider_state();
        let session_id = SessionId::new();
        let (inbound_tx, inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("test-model".to_string()),
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: None,
                worktree: None,
            },
        );
        let connection = Connection { state };
        let mut events = connection.subscribe_agent(session_id);
        connection
            .set_session_model(session_id, "claude".to_string(), "opus".to_string())
            .unwrap();

        // The resolved model (the alias -> id) landed on the session — a
        // re-attach would re-announce the switched model, not the old one.
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("m-opus")
        );
        // The switch command reached the session thread's inbound queue.
        let command = inbound_rx.recv().unwrap();
        assert!(
            matches!(
                &command,
                Command::SetSessionModel { provider, model }
                    if provider == "claude" && model == "opus"
            ),
            "{command:?}"
        );
        // And the resolution announcement rode the existing SessionModel
        // event.
        let sent = events.try_recv().unwrap();
        assert!(
            matches!(&sent, horizon_agent::wire::AgentWireEvent::SessionModel(model) if model == "m-opus"),
            "{sent:?}"
        );
    }

    fn moa_member(
        provider: &str,
        model: &str,
        api_key_present: bool,
    ) -> horizon_agent::config::MoaMember {
        horizon_agent::config::MoaMember {
            api_key_present,
            api_key_env: format!("{}_API_KEY", provider.to_uppercase()),
            ..horizon_agent::config::MoaMember::new(provider.to_string(), model.to_string())
        }
    }

    /// `mix` aggregates on the available `openai` entry; `stranded` on the
    /// key-less `claude` one.
    fn moa_table() -> horizon_agent::config::MoaTable {
        horizon_agent::config::MoaTable {
            entries: vec![
                horizon_agent::config::MoaEntry {
                    name: "mix".to_string(),
                    aggregator: moa_member("openai", "m-aggregate", true),
                    proposers: vec![moa_member("openai", "m-fast", true)],
                },
                horizon_agent::config::MoaEntry {
                    name: "stranded".to_string(),
                    aggregator: moa_member("claude", "m-opus", false),
                    proposers: vec![moa_member("openai", "m-fast", true)],
                },
            ],
        }
    }

    /// The configured `[[moa]]` entries are offered as one more group
    /// whose items are the entry names; a surface with none adds nothing.
    #[test]
    fn list_providers_offers_the_moa_entries_as_their_own_group() {
        let (state, _entries) = two_provider_state_with_moa(moa_table());
        let connection = Connection { state };
        let summaries = connection.list_providers();
        assert_eq!(summaries.len(), 3);
        let group = summaries.last().unwrap();
        assert_eq!(group.name, "moa");
        assert!(!group.default);
        assert!(group.available, "its aggregator's key is present");
        assert_eq!(
            group.models,
            vec![
                horizon_agent::wire::ModelAlias {
                    alias: "mix".to_string(),
                    model: "mix".to_string(),
                },
                // Listed even though its aggregator has no key: the group
                // carries one availability flag, so an unusable entry is
                // refused on confirm rather than hidden.
                horizon_agent::wire::ModelAlias {
                    alias: "stranded".to_string(),
                    model: "stranded".to_string(),
                },
            ]
        );

        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        assert_eq!(connection.list_providers().len(), 2);
    }

    /// Selecting a MoA entry announces the aggregator's model (what the
    /// session's requests name) and forwards the selection unchanged.
    #[test]
    fn set_session_model_accepts_a_moa_entry_and_announces_the_aggregator_model() {
        let (state, _entries) = two_provider_state_with_moa(moa_table());
        let session_id = SessionId::new();
        let (inbound_tx, inbound_rx) = unbounded::<Command>();
        let (replay_tx, _replay_rx) = unbounded::<Sender<Vec<Event>>>();
        state.sessions.lock().unwrap().insert(
            session_id,
            SessionEntry {
                provider_id: ProviderId("builtin.agent.rig".to_string()),
                role_id: None,
                model: Some("test-model".to_string()),
                inbound: inbound_tx,
                replay: replay_tx,
                parent_session_id: None,
                workspace_root: None,
                worktree: None,
            },
        );
        let connection = Connection { state };
        connection
            .set_session_model(session_id, "moa".to_string(), "mix".to_string())
            .unwrap();
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("m-aggregate")
        );
        let command = inbound_rx.recv().unwrap();
        assert!(
            matches!(
                &command,
                Command::SetSessionModel { provider, model }
                    if provider == "moa" && model == "mix"
            ),
            "{command:?}"
        );

        let error = connection
            .set_session_model(session_id, "moa".to_string(), "typo".to_string())
            .unwrap_err();
        assert!(error.contains("Unknown moa entry `typo`"), "{error}");

        // An entry whose aggregator's key variable is unset is refused with
        // the reason, not accepted into a session that would then answer
        // from the deterministic fallback responder.
        let error = connection
            .set_session_model(session_id, "moa".to_string(), "stranded".to_string())
            .unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
        assert!(error.contains("CLAUDE_API_KEY"), "{error}");
        assert_eq!(
            connection.session_model(session_id).as_deref(),
            Some("m-aggregate"),
            "the refused switch left the previous selection in place"
        );
    }

    /// The group is grayed out only when no entry could run at all; with a
    /// mix it stays selectable and the unusable entry is refused on confirm.
    #[test]
    fn the_moa_group_is_unavailable_only_when_no_entry_can_run() {
        let (state, _entries) = two_provider_state_with_moa(moa_table());
        let connection = Connection { state };
        assert!(connection.list_providers().last().unwrap().available);

        let stranded_only = horizon_agent::config::MoaTable {
            entries: vec![horizon_agent::config::MoaEntry {
                name: "stranded".to_string(),
                aggregator: moa_member("claude", "m-opus", false),
                proposers: vec![moa_member("openai", "m-fast", true)],
            }],
        };
        let (state, _entries) = two_provider_state_with_moa(stranded_only);
        let connection = Connection { state };
        let group = connection.list_providers().last().unwrap().clone();
        assert_eq!(group.name, "moa");
        assert!(!group.available);
    }

    #[test]
    fn set_session_model_rejects_an_unknown_provider_and_an_unknown_session() {
        let (state, _entries) = two_provider_state();
        let connection = Connection { state };
        let error = connection
            .set_session_model(SessionId::new(), "typo".to_string(), "m".to_string())
            .unwrap_err();
        assert!(error.contains("Unknown provider `typo`"), "{error}");

        // A known provider against an unknown session is the caller's bug
        // too — an error, not a silent no-op.
        let error = connection
            .set_session_model(SessionId::new(), "openai".to_string(), "fast".to_string())
            .unwrap_err();
        assert!(error.contains("Unknown session"), "{error}");
    }
}
