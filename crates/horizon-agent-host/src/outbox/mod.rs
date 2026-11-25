//! The agent outbox: delivers what sessions send and answer, and turns other
//! durable logs into session inputs, with receipts on both sides.
//!
//! The core folds the agent event log ([`log`]) into pending sends and
//! answers, delivers them in log order (one recipient waits for its earlier
//! delivery's receipt; others do not), and acknowledges each once it is
//! durable. What lies outside the agent log is plugged in: a
//! [`TriggerSource`] reads its own log and asks sessions to act on it, and a
//! [`ReplySink`] writes an answer back to the place its `reply_to` names.
//! An answer addressed to another session (`{"kind":"session","id":…}`) is
//! the core's own.

pub mod log;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use horizon_agent::contract::{Command, InputResult, SessionId, SessionInput};
use horizon_agent::persistence::event_log::Record;
use serde::Deserialize;

use crate::session::AgentdState;
use log::{Index, Pending, Tail};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The `kind` of a `reply_to` the core delivers itself.
const SESSION_KIND: &str = "session";

/// What a source or sink may read and do during one pass.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub state: &'a Arc<AgentdState>,
    pub index: &'a Index,
}

impl Context<'_> {
    /// Sends `input` to `target` unless the agent log already holds its
    /// receipt. `true` once the receipt exists; until then the same input is
    /// sent again on later passes, and the session dedupes it by id.
    pub fn deliver_input(&self, target: SessionId, input: SessionInput) -> bool {
        receipt(self.state, self.index, target, input)
    }
}

pub fn receipt(
    state: &Arc<AgentdState>,
    index: &Index,
    target: SessionId,
    input: SessionInput,
) -> bool {
    if index.accepted.contains(&(target, input.id.clone())) {
        return true;
    }
    state.send_command(target, Command::SessionInput(input));
    false
}

/// Reads a log of its own and turns its new entries into session inputs.
pub trait TriggerSource: Send + Sync {
    /// Sees every agent-log record as it is read, before delivery runs.
    fn observe(&mut self, _state: &Arc<AgentdState>, _record: &Record) {}
    /// One pass over the source's log.
    fn tick<'a>(&'a mut self, ctx: Context<'a>) -> BoxFuture<'a, Result<(), String>>;
}

/// One settled answer addressed to a sink.
pub struct Reply<'a> {
    /// The answering session.
    pub source: SessionId,
    /// The answer's delivery id, unique per source session.
    pub id: &'a str,
    /// The stored `reply_to`, a JSON object whose `kind` named this sink.
    pub address: &'a str,
    /// `session:<uuid>` for a success, `system` for a failure or interruption.
    pub author: String,
    pub text: String,
    /// When the answer was recorded, in Unix milliseconds.
    pub at: u64,
}

/// Writes answers back to the destinations whose `kind`s it owns.
pub trait ReplySink: Send + Sync {
    fn kinds(&self) -> &'static [&'static str];
    /// Answers with the same key wait for each other's receipts, in log
    /// order. `None` orders nothing.
    fn recipient_key(&self, address: &str) -> Option<String>;
    /// Delivers one answer. `Ok(true)` once it is durable, `Ok(false)` to be
    /// retried on the next pass.
    fn deliver<'a>(
        &'a self,
        ctx: Context<'a>,
        reply: Reply<'a>,
    ) -> BoxFuture<'a, Result<bool, String>>;
}

#[derive(Deserialize)]
struct Kind {
    kind: String,
}

#[derive(Deserialize)]
struct SessionAddress {
    id: String,
}

fn address_kind(address: &str) -> Result<String, String> {
    serde_json::from_str::<Kind>(address)
        .map(|kind| kind.kind)
        .map_err(|e| format!("Invalid stored reply destination: {e}"))
}

pub fn parse_session(id: &str) -> Result<SessionId, String> {
    uuid::Uuid::parse_str(id)
        .map(SessionId::from_uuid)
        .map_err(|e| format!("Invalid session identity: {e}"))
}

pub struct Outbox {
    state: Arc<AgentdState>,
    tail: Tail,
    index: Index,
    acknowledging: HashSet<(SessionId, String)>,
    sources: Vec<Box<dyn TriggerSource>>,
    sinks: Vec<Arc<dyn ReplySink>>,
}

impl Outbox {
    pub fn new(
        state: Arc<AgentdState>,
        path: PathBuf,
        sources: Vec<Box<dyn TriggerSource>>,
        sinks: Vec<Arc<dyn ReplySink>>,
    ) -> Self {
        Self {
            state,
            tail: Tail::new(path),
            index: Index::default(),
            acknowledging: HashSet::new(),
            sources,
            sinks,
        }
    }

    pub async fn tick(&mut self) -> Result<(), String> {
        for record in self.tail.read()? {
            for source in &mut self.sources {
                source.observe(&self.state, &record);
            }
            self.index.fold(record);
        }
        let mut error = self.deliver_outputs().await.err();
        let ctx = Context {
            state: &self.state,
            index: &self.index,
        };
        for source in &mut self.sources {
            if let Err(e) = source.tick(ctx).await {
                error = Some(e);
            }
        }
        error.map_or(Ok(()), Err)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn index(&self) -> &Index {
        &self.index
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn index_mut(&mut self) -> &mut Index {
        &mut self.index
    }

    fn sink(&self, kind: &str) -> Option<&Arc<dyn ReplySink>> {
        self.sinks.iter().find(|sink| sink.kinds().contains(&kind))
    }

    /// Wait for the first durable receipt before sending a later request to
    /// the same recipient. An offline recipient never stalls unrelated
    /// destinations.
    pub fn recipient_key(&self, pending: &Pending) -> Option<String> {
        match pending {
            Pending::Send { target, .. } => Some(format!("session:{}", target.as_uuid())),
            Pending::Answer { outcome, .. } => {
                let address = outcome.reply_to.as_deref()?;
                let kind = address_kind(address).ok()?;
                if kind == SESSION_KIND {
                    let target = serde_json::from_str::<SessionAddress>(address).ok()?;
                    return Some(format!("session:{}", target.id));
                }
                self.sink(&kind)?.recipient_key(address)
            }
        }
    }

    pub async fn deliver_outputs(&mut self) -> Result<(), String> {
        self.acknowledging
            .retain(|key| self.index.pending.contains_key(key));
        let pending = self.index.ordered_pending();
        let mut error = None;
        let mut waiting_recipients = HashSet::new();
        for ((source, id), pending) in pending {
            let recipient = self.recipient_key(&pending);
            if recipient
                .as_ref()
                .is_some_and(|key| waiting_recipients.contains(key))
            {
                continue;
            }
            if self.acknowledging.contains(&(source, id.clone()))
                && self.state.session_exists(source)
            {
                continue;
            }
            let done = match self.deliver_one(source, &id, pending).await {
                Ok(done) => done,
                Err(message) => {
                    if let Some(key) = recipient {
                        waiting_recipients.insert(key);
                    }
                    error = Some(message);
                    continue;
                }
            };
            if !done && let Some(key) = recipient {
                waiting_recipients.insert(key);
            }
            if done {
                match self.state.acknowledge_delivery(source, id.clone()) {
                    Ok(()) => {
                        self.acknowledging.insert((source, id));
                    }
                    Err(message) => error = Some(message),
                }
            }
        }
        error.map_or(Ok(()), Err)
    }

    pub async fn deliver_one(
        &self,
        source: SessionId,
        id: &str,
        pending: Pending,
    ) -> Result<bool, String> {
        let ctx = Context {
            state: &self.state,
            index: &self.index,
        };
        let (outcome, at) = match pending {
            Pending::Send { target, input, .. } => return Ok(ctx.deliver_input(target, input)),
            Pending::Answer { outcome, at, .. } => (outcome, at),
        };
        let Some(address) = outcome.reply_to.as_deref() else {
            return Ok(true);
        };
        let kind = address_kind(address)?;
        let (author, text) = match outcome.outcome {
            InputResult::Success { text } => (format!("session:{}", source.as_uuid()), text),
            InputResult::Failure { message } => ("system".into(), message),
            InputResult::Interrupted => ("system".into(), "Session interrupted.".into()),
        };
        if kind == SESSION_KIND {
            let target = serde_json::from_str::<SessionAddress>(address)
                .map_err(|e| format!("Invalid stored reply destination: {e}"))?;
            return Ok(ctx.deliver_input(
                parse_session(&target.id)?,
                SessionInput {
                    id: format!("session:{}:{id}", source.as_uuid()),
                    origin: author,
                    text,
                    reply_to: None,
                    resume_work: false,
                },
            ));
        }
        let sink = self
            .sink(&kind)
            .ok_or_else(|| format!("Invalid stored reply destination: no sink for `{kind}`"))?
            .clone();
        sink.deliver(
            ctx,
            Reply {
                source,
                id,
                address,
                author,
                text,
                at,
            },
        )
        .await
    }
}

/// Runs the outbox every 250 ms once session resume is ready.
pub fn spawn(
    state: Arc<AgentdState>,
    sources: Vec<Box<dyn TriggerSource>>,
    sinks: Vec<Arc<dyn ReplySink>>,
) {
    tokio::spawn(async move {
        state.wait_until_resume_ready().await;
        let path = state
            .agent_config
            .lock()
            .unwrap()
            .persistence
            .event_log_path
            .clone();
        let mut outbox = Outbox::new(state, path, sources, sinks);
        let mut last_error = None;
        loop {
            match outbox.tick().await {
                Ok(()) => last_error = None,
                Err(error) => {
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("horizon-agentd: outbox delivery: {error}");
                    }
                    last_error = Some(error);
                }
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
}
