//! The board-event subscriber task: a daemon-lifetime background task that
//! subscribes to `horizon-logd`'s board-event poke stream, reads back events
//! past its cursor, feeds them through the wake policy, and triggers the
//! wake action when the policy decides to wake the keeper.
//!
//! The task is spawned from `main` (alongside `spawn_resume_task`) as a
//! `tokio::spawn` — fire-and-forget, cloning `Arc<AgentdState>` in. It runs
//! for the daemon's entire lifetime.
//!
//! ## Restart recovery
//!
//! The cursor (last-processed seq) is persisted to disk
//! ([`super::cursor`]). On daemon restart, the task loads the cursor and
//! re-subscribes from `since = Some(cursor)`. The first poke from logd is
//! the current-seq reply; if it exceeds the cursor, the task reads back the
//! missed events from the JSONL file and processes them before continuing
//! with live pokes.
//!
//! ## Stream reconnection
//!
//! If the subscribe stream breaks (logd restarted), the task reconnects with
//! the current cursor after a backoff delay. Pokes are lossy by design
//! (`docs/logd-design.md` decision 3); correctness lives in the cursor.

use std::path::PathBuf;
use std::time::Duration;

use horizon_board::{Envelope, Store};

use horizon_agent::contract::ProviderId;

use super::action::{DoneFuture, WakeAction, WakeInfo};
use super::cursor;
use super::policy::{WakeDecision, WakeState};

/// The backoff duration for reconnecting to logd after a stream break.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);

/// Spawns the board wake subscriber as a fire-and-forget tokio task.
///
/// The task runs for the daemon's lifetime. If the board store cannot be
/// resolved (the daemon's cwd is not in a git repo), the task logs a message
/// and returns without spawning — the daemon runs fine without the wake
/// subscriber, just without automatic keeper wake-up.
pub(crate) fn spawn(
    action: Box<dyn WakeAction>,
    _provider_id: ProviderId,
    workspace_root: Option<PathBuf>,
) {
    tokio::spawn(async move {
        let store = match resolve_store(&workspace_root) {
            Some(store) => store,
            None => {
                eprintln!(
                    "horizon-agentd: wake subscriber not starting \
                     (not in a git repo or no workspace root)"
                );
                return;
            }
        };

        let events_path = store.path().to_path_buf();
        let cursor_file = cursor::cursor_path(&events_path);
        let initial_cursor = cursor_file.as_ref().map(|p| cursor::load(p)).unwrap_or(0);

        eprintln!("horizon-agentd: board wake subscriber starting (cursor={initial_cursor})");

        let mut runner = SubscriberRunner {
            store,
            events_path,
            cursor_file,
            state: WakeState::new(initial_cursor),
            action,
            keeper_done: None,
            keeper_author: None,
        };
        runner.run().await;
    });
}

/// Resolves the board store from the workspace root (or the daemon's cwd
/// if no explicit root is given). Returns `None` if not in a git repo.
fn resolve_store(workspace_root: &Option<PathBuf>) -> Option<Store> {
    match workspace_root {
        Some(root) => Store::from_dir(root).ok(),
        None => Store::from_cwd().ok(),
    }
}

/// The running state of the subscriber task, kept across stream
/// reconnections.
struct SubscriberRunner {
    store: Store,
    events_path: PathBuf,
    cursor_file: Option<PathBuf>,
    state: WakeState,
    action: Box<dyn WakeAction>,
    /// The in-flight keeper's completion future, if a keeper is running.
    keeper_done: Option<DoneFuture>,
    /// The in-flight keeper's author string (for the policy's self-filter).
    keeper_author: Option<String>,
}

impl SubscriberRunner {
    /// The main loop: subscribe, process events, wake, reconnect on break.
    async fn run(&mut self) {
        loop {
            match self.subscribe_and_process().await {
                Ok(()) => {
                    // Stream ended normally (logd drain/shutdown). Reconnect
                    // after backoff.
                    eprintln!(
                        "horizon-agentd: board wake subscribe stream ended; \
                         reconnecting in {RECONNECT_BACKOFF:?}"
                    );
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                }
                Err(e) => {
                    eprintln!(
                        "horizon-agentd: board wake subscribe error: {e}; \
                         reconnecting in {RECONNECT_BACKOFF:?}"
                    );
                    tokio::time::sleep(RECONNECT_BACKOFF).await;
                }
            }
        }
    }

    /// Subscribes to logd, processes the cursor-on-connect reply and
    /// subsequent pokes, and returns when the stream ends or errors.
    async fn subscribe_and_process(&mut self) -> std::io::Result<()> {
        let since = Some(self.state.cursor());
        let mut stream = self
            .store
            .subscribe(since)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // First line: the cursor-on-connect reply (current seq). If it
        // exceeds our cursor, read back the missed events.
        if let Some(line) = stream.next_line().await? {
            if let Some(current_seq) = parse_poke_seq(&line) {
                self.catch_up_to(current_seq).await;
            }
        }

        // Subsequent lines: live pokes, one per appended event.
        loop {
            tokio::select! {
                // A new poke from logd.
                line = stream.next_line() => {
                    match line? {
                        Some(line) => {
                            if let Some(seq) = parse_poke_seq(&line) {
                                self.process_new_events(seq).await;
                            }
                        }
                        None => return Ok(()), // stream closed
                    }
                }
                // The in-flight keeper session has finished.
                _ = poll_keeper_done(&mut self.keeper_done), if self.keeper_done.is_some() => {
                    self.handle_keeper_finished().await;
                }
            }
        }
    }

    /// Reads back events from the JSONL file for all seqs in
    /// `(cursor, target_seq]` and feeds them into the policy. Used for
    /// catch-up on connect and on each live poke.
    async fn catch_up_to(&mut self, target_seq: u64) {
        let cursor = self.state.cursor();
        if target_seq <= cursor {
            return;
        }
        let envelopes = read_events_since(&self.events_path, cursor);
        self.feed_and_act(envelopes).await;
    }

    /// On a live poke with seq `N`, reads back any events between the
    /// current cursor and `N` and processes them.
    async fn process_new_events(&mut self, poke_seq: u64) {
        self.catch_up_to(poke_seq).await;
    }

    /// Feeds a batch of `(seq, Envelope)` pairs into the policy and acts on
    /// the resulting decision. Persists the cursor whenever it advances.
    async fn feed_and_act(&mut self, envelopes: Vec<(u64, Envelope)>) {
        if envelopes.is_empty() {
            return;
        }

        let decision = self.state.feed(&envelopes);

        // Persist the cursor after every batch — even if no wake is triggered,
        // the cursor advanced past non-relevant events.
        self.persist_cursor();

        match decision {
            WakeDecision::Wake {
                items,
                first_seq,
                last_seq,
            } => {
                self.trigger_wake(items, first_seq, last_seq).await;
            }
            WakeDecision::Pending => {}
        }
    }

    /// Triggers a wake via the action seam, storing the done future for
    /// multi-wake prevention.
    async fn trigger_wake(&mut self, items: Vec<u64>, first_seq: u64, last_seq: u64) {
        // If a keeper is already running, don't trigger another wake —
        // the events will be caught up after the keeper finishes.
        // (This should not happen because the policy returns Pending while
        // keeper_running is true, but this is a belt-and-braces guard.)
        if self.keeper_done.is_some() {
            return;
        }

        let info = WakeInfo {
            items,
            first_seq,
            last_seq,
        };
        let (author, done) = self.action.wake(info);
        self.state.keeper_started(author.clone());
        self.keeper_author = Some(author);
        self.keeper_done = Some(done);
    }

    /// Called when the in-flight keeper session has finished. Resets the
    /// running state and processes any events that accumulated while it was
    /// running (the policy's `keeper_finished` may return an immediate Wake).
    async fn handle_keeper_finished(&mut self) {
        self.keeper_done = None;
        self.keeper_author = None;
        let decision = self.state.keeper_finished();
        match decision {
            WakeDecision::Wake {
                items,
                first_seq,
                last_seq,
            } => {
                self.trigger_wake(items, first_seq, last_seq).await;
            }
            WakeDecision::Pending => {}
        }
    }

    /// Persists the current cursor to disk.
    fn persist_cursor(&self) {
        if let Some(path) = &self.cursor_file {
            cursor::save(path, self.state.cursor());
        }
    }
}

/// Parses the seq from a subscribe poke line (`{"log":"board","seq":N}`).
/// Returns `None` if the line is not a valid poke.
fn parse_poke_seq(line: &str) -> Option<u64> {
    let poke: horizon_board::wire::SubscribePoke = serde_json::from_str(line).ok()?;
    Some(poke.seq)
}

/// Reads the board event log file and returns `(seq, Envelope)` pairs for
/// every decodable event whose 1-based line number exceeds `since`.
///
/// The seq is the 1-based line number in the JSONL file (counting non-empty
/// lines, matching `event::read`'s `line_count` semantics). Lines that fail
/// to decode as an `Envelope` (corrupt, future event types, or foreign
/// format) are still counted for seq purposes but produce no envelope —
/// this mirrors the tolerant reader's behavior and ensures the cursor
/// advances past them.
fn read_events_since(path: &std::path::Path, since: u64) -> Vec<(u64, Envelope)> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };

    // Drop a torn trailing line (no trailing newline) — same as event::read.
    let text = if !text.is_empty() && !text.ends_with('\n') {
        &text[..text.rfind('\n').map(|i| i + 1).unwrap_or(0)]
    } else {
        &text
    };

    let mut result = Vec::new();
    let mut seq: u64 = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        seq += 1;
        if seq <= since {
            continue;
        }
        if let Ok(env) = serde_json::from_str::<Envelope>(line) {
            result.push((seq, env));
        }
        // Lines that fail to decode are skipped (cursor still advances past
        // them via the seq counter).
    }
    result
}

/// A helper future that polls the keeper-done future to completion. Used in
/// `select!` with a guard so it's only polled when a keeper is running.
async fn poll_keeper_done(done: &mut Option<DoneFuture>) {
    if let Some(fut) = done.as_mut() {
        fut.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_board::{BoardEvent, Envelope, SCHEMA, VERSION};

    fn env(at: u64, event: BoardEvent) -> Envelope {
        Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event,
        }
    }

    fn item_created(id: u64) -> BoardEvent {
        BoardEvent::ItemCreated {
            id,
            title: format!("Item {id}"),
            body: String::new(),
            rank: "n".to_string(),
        }
    }

    fn comment(id: u64, author: &str) -> BoardEvent {
        BoardEvent::CommentAdded {
            id,
            author: author.to_string(),
            text: "x".to_string(),
        }
    }

    fn write_events(path: &std::path::Path, events: &[BoardEvent]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut text = String::new();
        for (i, event) in events.iter().enumerate() {
            let env = env((i as u64 + 1) * 1000, event.clone());
            text.push_str(&serde_json::to_string(&env).unwrap());
            text.push('\n');
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn read_events_since_returns_only_new_events() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("events.jsonl");
        write_events(
            &path,
            &[item_created(1), comment(1, "owner"), item_created(2)],
        );

        let result = read_events_since(&path, 1);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, 2);
        assert_eq!(result[1].0, 3);
    }

    #[test]
    fn read_events_since_returns_empty_when_cursor_at_end() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("events.jsonl");
        write_events(&path, &[item_created(1), comment(1, "owner")]);

        let result = read_events_since(&path, 2);
        assert!(result.is_empty());
    }

    #[test]
    fn read_events_since_returns_empty_for_missing_file() {
        let result = read_events_since(std::path::Path::new("/nonexistent/events.jsonl"), 0);
        assert!(result.is_empty());
    }

    #[test]
    fn read_events_since_skips_undecodable_lines_but_advances_seq() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-wake-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("events.jsonl");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // Line 1: valid, line 2: garbage, line 3: valid.
        let env1 = env(1000, item_created(1));
        let env3 = env(3000, comment(1, "owner"));
        let text = format!(
            "{}\nthis is not json\n{}\n",
            serde_json::to_string(&env1).unwrap(),
            serde_json::to_string(&env3).unwrap(),
        );
        std::fs::write(&path, text).unwrap();

        let result = read_events_since(&path, 0);
        // Only lines 1 and 3 decode; line 2 is skipped but seq still counts.
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, 1);
        assert_eq!(result[1].0, 3);
    }

    #[test]
    fn parse_poke_seq_extracts_seq() {
        let line = r#"{"log":"board","seq":42}"#;
        assert_eq!(parse_poke_seq(line), Some(42));
    }

    #[test]
    fn parse_poke_seq_returns_none_for_garbage() {
        assert_eq!(parse_poke_seq("not json"), None);
        assert_eq!(parse_poke_seq(r#"{"log":"board"}"#), None);
    }
}
