//! The project wiki (`horizon-wiki`) composed into the daemon
//! (docs/project-wiki-design.md): its page tools, its thread role and
//! skill, and its two outbox plug-ins — a trigger source handing each
//! owner comment to its thread's session, and a reply sink appending the
//! session's answer to the thread and suspending the session.
//!
//! One session answers one thread; its id is derived from the thread's
//! root annotation, so nothing records which session a thread has. It is
//! started on the thread's first comment, suspended once the thread has no
//! comment left to answer, and resumed by the next one.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Once};

use horizon_agent::contract::{SessionId, SessionInput};
use horizon_agent::roles::{RoleDefinition, RoleId};
use horizon_agent::tools::{ExternalOutput, ExternalTool};
use horizon_wiki::agent::{self, THREAD_ROLE_ID};
use horizon_wiki::annotation::{markdown_body, Agent, Annotation};
use horizon_wiki::{Store, Thread, WikiError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::outbox::{BoxFuture, Context, Reply, ReplySink, TriggerSource};
use crate::session::{resume_session, spawn_session_thread, AgentdState};

const KIND: &str = "wiki";

/// A wiki answer's `reply_to`: the store, the page's log, and the
/// annotation answered.
#[derive(Debug, Serialize, Deserialize)]
struct WikiAddress {
    kind: String,
    store: PathBuf,
    page: Uuid,
    parent: String,
}

/// Registers the wiki's tools, thread role and skill. Idempotent.
pub(crate) fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        horizon_agent::roles::register_external(vec![RoleDefinition {
            id: THREAD_ROLE_ID,
            title: agent::THREAD_ROLE_TITLE,
            prompt_section: agent::THREAD_ROLE_PROMPT,
            allowed_tool_ids: Some(agent::THREAD_ROLE_TOOLS),
            model: None,
            iteration_cap: None,
            include_repository_instructions: true,
            skill_ids: agent::THREAD_ROLE_SKILLS,
            summarize_on_cap: false,
            standing: false,
            unattended: true,
        }]);
        horizon_agent::skills::register_external_skill_sources(vec![
            horizon_wiki::skill::SKILL_SOURCE,
        ]);
        let tools = agent::tool_definitions()
            .into_iter()
            .map(|definition| {
                let id = definition.id;
                ExternalTool {
                    id: id.into(),
                    title: definition.title.into(),
                    description: definition.description.into(),
                    input_schema: definition.input_schema,
                    // What a page says reaches the prompt, as knowledge does.
                    requires_trusted_project: true,
                    output: Some(ExternalOutput {
                        type_name: definition.output_type.into(),
                        schema: definition.output_schema,
                    }),
                    execute: Arc::new(move |call| {
                        let root = call
                            .project_root
                            .as_deref()
                            .ok_or("The session is not in a repository, so it has no wiki")?;
                        let store =
                            Store::for_project(root).ok_or("No data directory for the wiki")?;
                        agent::run_tool(&store, id, call.input)
                    }),
                }
            })
            .collect();
        if let Err(error) = horizon_agent::tools::register_external_tools(tools) {
            eprintln!("horizon-agentd: wiki tools not registered: {error}");
        }
    });
}

/// The wiki's trigger source for the project at `root` (the daemon's start
/// directory); `None` outside a repository.
pub(crate) fn source(root: Option<PathBuf>) -> Option<Box<dyn TriggerSource>> {
    let project = crate::worktree::project_root(&root?)?;
    let store = Store::for_project(&project)?;
    Some(Box::new(WikiSource {
        project,
        store,
        lengths: HashMap::new(),
        waiting: HashSet::new(),
        threads: HashMap::new(),
        replying: None,
    }))
}

pub(crate) fn sink() -> Arc<dyn ReplySink> {
    Arc::new(WikiSink)
}

struct WikiSource {
    project: PathBuf,
    store: Store,
    /// Each page log's length when it was last read.
    lengths: HashMap<Uuid, u64>,
    /// Pages with a comment not yet accepted by its session.
    waiting: HashSet<Uuid>,
    /// Each page's threads as last read.
    threads: HashMap<Uuid, Vec<Thread>>,
    /// The comments last written as being answered.
    replying: Option<BTreeSet<String>>,
}

impl TriggerSource for WikiSource {
    fn tick<'a>(&'a mut self, ctx: Context<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mut error = None;
            for page in self.store.commented_pages().map_err(|e| e.to_string())? {
                let length = std::fs::metadata(self.store.thread_log_path(page))
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                if self.lengths.get(&page) == Some(&length) && !self.waiting.contains(&page) {
                    continue;
                }
                self.lengths.insert(page, length);
                let threads = match self.store.threads(page) {
                    Ok(threads) => threads,
                    Err(e) => {
                        error = Some(e.to_string());
                        continue;
                    }
                };
                let delivered = self.deliver_page(ctx, page, &threads);
                self.threads.insert(page, threads);
                match delivered {
                    Ok(true) => {
                        self.waiting.remove(&page);
                    }
                    Ok(false) => {
                        self.waiting.insert(page);
                    }
                    Err(e) => {
                        self.waiting.insert(page);
                        error = Some(e);
                    }
                }
            }
            if let Err(e) = self.publish_replying(ctx) {
                error = Some(e);
            }
            error.map_or(Ok(()), Err)
        })
    }
}

impl WikiSource {
    /// Hands every unanswered owner comment on `page` to its thread's
    /// session. `true` once each has been accepted.
    fn deliver_page(
        &self,
        ctx: Context<'_>,
        page: Uuid,
        threads: &[Thread],
    ) -> Result<bool, String> {
        let name = match self.store.page_name(page) {
            Ok(name) => name,
            Err(WikiError::NotFound(_)) => horizon_wiki::page::urn(page),
            Err(e) => return Err(e.to_string()),
        };
        let mut done = true;
        for thread in threads {
            let session = SessionId::from_uuid(agent::thread_session(&thread.root.id));
            for annotation in agent::unanswered(thread) {
                let id = format!("wiki:{}", annotation.id);
                if ctx.index.accepted.contains(&(session, id.clone())) {
                    continue;
                }
                self.ensure_started(ctx.state, session)?;
                let reply_to = serde_json::to_string(&WikiAddress {
                    kind: KIND.into(),
                    store: self.store.dir().to_path_buf(),
                    page,
                    parent: annotation.id.clone(),
                })
                .map_err(|e| e.to_string())?;
                done &= ctx.deliver_input(
                    session,
                    SessionInput {
                        id,
                        origin: "owner".into(),
                        text: agent::thread_input(&self.store, &name, thread, annotation),
                        reply_to: Some(reply_to),
                        resume_work: true,
                    },
                );
            }
        }
        Ok(done)
    }

    /// Writes which comments are being answered now, for the wiki view's
    /// progress line: those whose input the thread's session has accepted,
    /// that have no reply yet, while that session is running. Recomputed
    /// every pass, so a crash or a suspend leaves nothing stale behind;
    /// written only when it changes.
    fn publish_replying(&mut self, ctx: Context<'_>) -> Result<(), String> {
        let mut replying = BTreeSet::new();
        for thread in self.threads.values().flatten() {
            let session = SessionId::from_uuid(agent::thread_session(&thread.root.id));
            if !ctx.state.session_exists(session) {
                continue;
            }
            for annotation in agent::unanswered(thread) {
                let id = format!("wiki:{}", annotation.id);
                if ctx.index.accepted.contains(&(session, id)) {
                    replying.insert(annotation.id.clone());
                }
            }
        }
        if self.replying.as_ref() == Some(&replying) {
            return Ok(());
        }
        self.store
            .write_replying(&replying)
            .map_err(|e| e.to_string())?;
        self.replying = Some(replying);
        Ok(())
    }

    /// Starts `session` with the thread role, or resumes it when it has
    /// answered before.
    fn ensure_started(&self, state: &Arc<AgentdState>, session: SessionId) -> Result<(), String> {
        if state.session_exists(session) {
            return Ok(());
        }
        let writer = state
            .writer()
            .ok_or("Wiki threads require event persistence")?;
        writer.flush().map_err(|e| e.to_string())?;
        if !state.flushed_session_records(session)?.is_empty() {
            return resume_session(state, session);
        }
        let provider = state
            .providers
            .lock()
            .map_err(|e| e.to_string())?
            .default_provider_id();
        spawn_session_thread(
            state.clone(),
            session,
            provider,
            Some(RoleId(THREAD_ROLE_ID.into())),
            Some(self.project.clone()),
            None,
            None,
            Vec::new(),
        );
        Ok(())
    }
}

struct WikiSink;

impl ReplySink for WikiSink {
    fn kinds(&self) -> &'static [&'static str] {
        &[KIND]
    }

    fn recipient_key(&self, address: &str) -> Option<String> {
        let address: WikiAddress = serde_json::from_str(address).ok()?;
        Some(format!("wiki:{}:{}", address.store.display(), address.page))
    }

    fn deliver<'a>(
        &'a self,
        ctx: Context<'a>,
        reply: Reply<'a>,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let address: WikiAddress = serde_json::from_str(reply.address)
                .map_err(|e| format!("Invalid stored reply destination: {e}"))?;
            let store = Store::at(&address.store);
            let session = reply.source.as_uuid();
            let annotation = Annotation::reply_with_id(
                agent::reply_id(session, reply.id),
                &address.parent,
                Agent::session(session),
                markdown_body(reply.text),
            );
            match store.append_annotation(address.page, &annotation) {
                Ok(()) => {}
                // Recorded on an earlier pass whose receipt was lost.
                Err(WikiError::Invalid(_))
                    if store
                        .annotations(address.page)
                        .is_ok_and(|all| all.iter().any(|known| known.id == annotation.id)) => {}
                Err(e) => return Err(e.to_string()),
            }
            suspend_if_answered(ctx.state, &store, address.page, reply.source);
            Ok(true)
        })
    }
}

/// Suspends `session` when its thread ends with this session's reply, so
/// nothing is left for it to answer.
fn suspend_if_answered(state: &Arc<AgentdState>, store: &Store, page: Uuid, session: SessionId) {
    let Ok(threads) = store.threads(page) else {
        return;
    };
    let answered = threads.iter().any(|thread| {
        agent::thread_session(&thread.root.id) == session.as_uuid()
            && agent::unanswered(thread).is_empty()
    });
    if answered {
        state.suspend_session(session);
    }
}

#[cfg(test)]
mod tests;
