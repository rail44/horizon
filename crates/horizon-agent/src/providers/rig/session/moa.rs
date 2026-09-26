//! The aggregator half of a Mixture-of-Agents pass
//! (`docs/agent-moa-design.md`): the conversation proposers are given, the
//! barrier that waits for all of them, and the block their answers are
//! injected as.
//!
//! One owner message opens one pass. Tool-result rounds, continue-turn
//! rounds, and task-notification rounds do not open another.
//!
//! The proposals are projected into the provider-facing message list at a
//! position fixed for the whole turn (see [`MoaTurn::index`] and
//! `clearing::history_for_provider_request`) and are never appended to
//! `rig_history`. The provider's prompt cache keys on the request prefix, so
//! the position has to keep everything ahead of the block byte-identical
//! across the turn's rounds and across turns.

use rig_core::completion::Message;

use crate::contract::{Command, Error, Event, MessageRole, MoaPassStarted, MoaProposer};
use crate::tools::moa::Proposal;

use super::state::SessionLoopState;

/// A turn's proposals, and where the request builder puts them.
#[derive(Clone, Debug)]
pub(crate) struct MoaTurn {
    /// The rendered block, injected verbatim as one user-role message.
    pub(crate) block: String,
    /// Index in `rig_history` the block is inserted at, resolved on the
    /// turn's first provider round and reused by every later round of the
    /// same turn. It is the length `rig_history` had before the turn's
    /// opening message was pushed, so the block always lands immediately
    /// ahead of that message and never between a tool call and its result.
    pub(crate) index: Option<usize>,
}

/// One side of the plain-text conversation proposers receive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MoaSpeaker {
    Owner,
    Assistant,
}

/// The owner messages and the answers written back to them, in order.
/// Tool calls, tool results, task notifications, and auto-continue prompts
/// are not part of it.
#[derive(Clone, Debug, Default)]
pub(crate) struct MoaConversation {
    entries: Vec<(MoaSpeaker, String)>,
}

impl MoaConversation {
    /// Rebuilds the conversation from a session's persisted events, so a
    /// resumed session's proposers see what a continuously-running one's
    /// would. Assistant messages before the first owner message belong to
    /// session startup; consecutive assistant messages collapse to the last,
    /// which is the answer that turn ended on.
    pub(crate) fn from_events(events: &[Event]) -> Self {
        let mut conversation = Self::default();
        for event in events {
            let Event::MessageCommitted(message) = event else {
                continue;
            };
            match message.role {
                MessageRole::User => conversation.record_owner(message.text.clone()),
                MessageRole::Assistant => conversation.record_answer(message.text.clone()),
                MessageRole::TaskNotification | MessageRole::AutoContinue => {}
            }
        }
        conversation
    }

    pub(crate) fn record_owner(&mut self, text: String) {
        self.entries.push((MoaSpeaker::Owner, text));
    }

    pub(crate) fn record_answer(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        match self.entries.last_mut() {
            Some((MoaSpeaker::Assistant, previous)) => *previous = text,
            Some(_) => self.entries.push((MoaSpeaker::Assistant, text)),
            // Nothing has been asked yet: this is the session's own startup
            // notice, not an answer.
            None => {}
        }
    }

    fn render(&self) -> String {
        let mut rendered = String::new();
        for (speaker, text) in &self.entries {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(match speaker {
                MoaSpeaker::Owner => "User:\n",
                MoaSpeaker::Assistant => "Assistant:\n",
            });
            rendered.push_str(text.trim());
        }
        rendered
    }
}

/// What one proposer is asked. The conversation so far is plain text rather
/// than a reconstructed message list: several model families answer one
/// pass, and a chat template that rejects a reshaped history would kill the
/// proposer outright.
pub(crate) fn proposer_prompt(conversation: &MoaConversation, message: &str) -> String {
    let mut prompt = String::from(
        "You are one of several assistants answering the user's message below, independently. \
         Another model reads every answer and writes the reply the user sees, so make yours \
         complete on its own. Name the files you relied on.\n\n\
         Reply with the answer only, as your own words: the conversation below is context, not \
         something to continue.\n\n",
    );
    let history = conversation.render();
    if !history.is_empty() {
        prompt.push_str("Conversation so far:\n\n");
        prompt.push_str(&history);
        prompt.push_str("\n\n");
    }
    prompt.push_str("The message to answer:\n\n");
    prompt.push_str(message.trim());
    prompt
}

/// Trims a proposal that continued the transcript it was shown instead of
/// answering: a leading `User:`/`Assistant:` header is dropped, and anything
/// from a later `User:` header onward is a fabricated next turn. `None` when
/// nothing usable is left.
pub(crate) fn sanitize_proposal(text: &str) -> Option<String> {
    let mut body = text.trim();
    for header in ["Assistant:", "User:"] {
        if let Some(rest) = body.strip_prefix(header) {
            body = rest.trim_start();
            break;
        }
    }
    let cut = body
        .match_indices("\nUser:")
        .next()
        .map(|(index, _)| index)
        .unwrap_or(body.len());
    let body = body[..cut].trim();
    (!body.is_empty()).then(|| body.to_string())
}

/// The block the aggregator's request carries: the proposals, plus what to
/// do with them and how to look further into any of them.
pub(crate) fn proposal_block(proposals: &[(usize, String, String)]) -> String {
    let mut block = String::from(
        "Several assistants answered the user's latest message independently; their answers \
         follow. Synthesize them into a single reply. Evaluate them critically — some may be \
         biased or incorrect — and write a refined, accurate answer rather than replicating any \
         one of them.\n\n\
         Each answer names its session; `recall.search` / `recall.read` with that session_id \
         reach the tool calls and results behind it.\n\n",
    );
    for (position, session_id, text) in proposals {
        block.push_str(&format!(
            "\n--- Answer {position} (session_id {session_id}) ---\n{text}\n"
        ));
    }
    block
}

/// How a pass ended.
pub(crate) enum PassOutcome {
    /// The aggregator's turn should run. The proposals (possibly none) are
    /// already installed on the session state.
    Proceed,
    /// The turn was cancelled while proposers were running; the caller must
    /// not start the aggregator's turn.
    Cancelled,
}

impl SessionLoopState {
    /// Runs one pass for `message`: launch a proposer per configured member,
    /// wait for all of them, and install the block the aggregator's rounds
    /// will carry. Returns [`PassOutcome::Cancelled`] if the turn was
    /// cancelled or shut down while waiting.
    pub(crate) async fn run_moa_pass(&mut self, message: &str) -> PassOutcome {
        self.moa_turn = None;
        let Some(pass) = self.config.moa.clone() else {
            return PassOutcome::Proceed;
        };
        if pass.proposers.is_empty() {
            return PassOutcome::Proceed;
        }
        let Some(host) = crate::tools::moa::exploration_host(self.session_id) else {
            self.report_pass_failure(format!(
                "cannot spawn proposer sessions; the `{}` aggregator answers alone",
                pass.name
            ));
            return PassOutcome::Proceed;
        };

        let prompt = proposer_prompt(&self.moa_conversation, message);
        let mut launch = crate::tools::moa::launch(self.session_id, host, &pass.proposers, &prompt);
        let _ = self.events_tx.send(
            Event::MoaPassStarted(MoaPassStarted {
                entry: pass.name.clone(),
                proposers: launch
                    .launched
                    .iter()
                    .map(|proposer| MoaProposer {
                        session_id: proposer.session_id,
                        provider: proposer.provider.clone(),
                        model: proposer.model.clone(),
                    })
                    .collect(),
            })
            .into(),
        );

        let mut collected: Vec<Proposal> = Vec::new();
        let mut remaining = launch.launched.len();
        while remaining > 0 {
            tokio::select! {
                received = launch.results.recv() => match received {
                    Some(proposal) => {
                        collected.push(proposal);
                        remaining -= 1;
                    }
                    // Every watcher is gone without reporting: nothing more
                    // can arrive, so stop waiting.
                    None => break,
                },
                maybe_command = self.commands.recv() => match maybe_command {
                    Some(Command::Cancel { .. }) => {
                        launch.abort();
                        self.pause_inputs(true);
                        self.emit_cancelled_turn();
                        return PassOutcome::Cancelled;
                    }
                    Some(Command::Shutdown) => {
                        launch.abort();
                        self.pause_inputs(true);
                        self.inbox.push_front(Command::Shutdown);
                        self.emit_cancelled_turn();
                        return PassOutcome::Cancelled;
                    }
                    Some(other) => self.inbox.push_back(other),
                    None => break,
                },
            }
        }

        collected.extend(launch.unavailable.iter().cloned());
        self.install_proposals(&pass.name, collected);
        PassOutcome::Proceed
    }

    /// Renders whatever the pass collected into the turn's injected block.
    /// With nothing usable the turn runs as an ordinary single-model turn.
    /// A proposer runs in no pane, so each one that contributed nothing is
    /// reported through [`Self::report_pass_failure`].
    fn install_proposals(&mut self, entry: &str, proposals: Vec<Proposal>) {
        let mut usable = Vec::new();
        for proposal in &proposals {
            match proposal.text.as_deref().and_then(sanitize_proposal) {
                Some(text) => usable.push((
                    usable.len() + 1,
                    proposal.member.session_id.as_uuid().to_string(),
                    text,
                )),
                None => self.report_pass_failure(format!(
                    "moa `{entry}` proposer {}/{} contributed nothing ({})",
                    proposal.member.provider,
                    proposal.member.model,
                    proposal
                        .failure
                        .as_deref()
                        .unwrap_or("its answer was not usable")
                )),
            }
        }
        if usable.is_empty() {
            self.report_pass_failure(format!(
                "moa `{entry}` got no usable proposals; the aggregator answers alone"
            ));
            return;
        }
        self.moa_turn = Some(MoaTurn {
            block: proposal_block(&usable),
            index: None,
        });
    }

    /// Records a pass failure as an error item in the aggregator's pane: a
    /// proposer session is never attached to a pane, so nothing else the
    /// user can see says the pass lost one.
    fn report_pass_failure(&self, message: String) {
        let _ = self.events_tx.send(Event::Error(Error { message }).into());
    }

    /// The message the block is injected as, plus the index to insert it at,
    /// resolving the index against the current history on first use.
    pub(crate) fn moa_injection(&mut self) -> Option<(usize, Message)> {
        let history_len = self.rig_history.len();
        let turn = self.moa_turn.as_mut()?;
        let index = *turn.index.get_or_insert(history_len);
        Some((index, Message::user(turn.block.clone())))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crossbeam_channel::Sender as CrossbeamSender;

    use crate::config::{MoaMember, MoaPass, RigAgentConfig};
    use crate::contract::{SessionId, SessionState, TurnEndReason};
    use crate::tools::{ExplorationHost, ExplorationRequest, StartedExploration};

    use super::*;

    /// A spawn capability that hands back a channel the test drives, so a
    /// whole pass runs without a provider, a daemon, or a network.
    #[derive(Default)]
    struct ScriptedHost {
        started: Mutex<Vec<(SessionId, String, String, String)>>,
        events: Mutex<Vec<(SessionId, CrossbeamSender<Event>)>>,
        terminated: Mutex<Vec<SessionId>>,
        refuse: bool,
    }

    impl ExplorationHost for ScriptedHost {
        fn start(&self, request: ExplorationRequest) -> Result<StartedExploration, String> {
            if self.refuse {
                return Err("no provider is configured".to_string());
            }
            let session_id = SessionId::new();
            self.started.lock().unwrap().push((
                session_id,
                request.provider.unwrap_or_default(),
                request.model.unwrap_or_default(),
                request.prompt,
            ));
            let (tx, events) = crossbeam_channel::unbounded();
            self.events.lock().unwrap().push((session_id, tx));
            Ok(StartedExploration { session_id, events })
        }

        fn terminate(&self, session_id: SessionId) {
            self.terminated.lock().unwrap().push(session_id);
        }
    }

    impl ScriptedHost {
        /// Plays one proposer session's whole answer, the shape the explore
        /// role's fold recognizes as a finished report.
        fn answer(&self, index: usize, text: &str) {
            let events = self.events.lock().unwrap();
            let (_, tx) = &events[index];
            let _ = tx.send(Event::MessageCommitted(crate::contract::Message {
                role: MessageRole::User,
                text: "the question".to_string(),
            }));
            let _ = tx.send(Event::MessageCommitted(crate::contract::Message {
                role: MessageRole::Assistant,
                text: text.to_string(),
            }));
            let _ = tx.send(Event::TurnEnded(TurnEndReason::Completed));
        }

        /// A proposer that strayed outside its workspace root: the read is
        /// refused with an error result — never an approval prompt, which
        /// this fold would treat as a dead end — and the session carries on
        /// to its answer.
        fn answer_after_a_refused_read(&self, index: usize, text: &str) {
            let events = self.events.lock().unwrap();
            let (_, tx) = &events[index];
            let call_id = crate::contract::ToolCallId("proposer-read".to_string());
            let _ = tx.send(Event::MessageCommitted(crate::contract::Message {
                role: MessageRole::User,
                text: "the question".to_string(),
            }));
            let _ = tx.send(Event::ToolCallRequested(crate::contract::ToolCallRequest {
                call_id: call_id.clone(),
                tool_id: "fs.read".to_string(),
                input: serde_json::json!({ "path": "/elsewhere/lib.rs" }).into(),
                occurrence_id: crate::contract::OccurrenceId(call_id.0.clone()),
            }));
            let _ = tx.send(Event::ToolCallFinished(
                crate::contract::ToolCallResult::new(
                    call_id.clone(),
                    crate::contract::OccurrenceId(call_id.0.clone()),
                    serde_json::json!({
                        "is_error": true,
                        "message": "`fs.read` cannot read `/elsewhere/lib.rs`: it is outside \
                                    this session's workspace root.",
                    }),
                ),
            ));
            let _ = tx.send(Event::MessageCommitted(crate::contract::Message {
                role: MessageRole::Assistant,
                text: text.to_string(),
            }));
            let _ = tx.send(Event::TurnEnded(TurnEndReason::Completed));
        }

        /// A proposer that dies without producing anything.
        fn fail(&self, index: usize) {
            let events = self.events.lock().unwrap();
            let (_, tx) = &events[index];
            let _ = tx.send(Event::StateChanged(SessionState::Terminated));
        }

        fn started_members(&self) -> Vec<(String, String)> {
            self.started
                .lock()
                .unwrap()
                .iter()
                .map(|(_, provider, model, _)| (provider.clone(), model.clone()))
                .collect()
        }
    }

    /// A member whose entry had its key, so the pass launches it.
    fn member(model: &str) -> MoaMember {
        MoaMember {
            api_key_present: true,
            api_key_env: "HORIZON_TEST_MOA_KEY".to_string(),
            ..MoaMember::new("synthetic".to_string(), model.to_string())
        }
    }

    fn members() -> Vec<MoaMember> {
        vec![member("hf:a/A"), member("hf:b/B")]
    }

    /// A loop state selected onto a `[[moa]]` entry, with a live command
    /// sender kept by the caller so the barrier's cancellation branch does
    /// not see a closed channel.
    fn moa_state(
        host: Arc<ScriptedHost>,
    ) -> (
        SessionLoopState,
        CrossbeamSender<Command>,
        crossbeam_channel::Receiver<crate::contract::ProviderEvent>,
    ) {
        let (commands_tx, commands_rx) = crossbeam_channel::unbounded::<Command>();
        let (events_tx, events_rx) = crossbeam_channel::unbounded();
        let session_id = SessionId::new();
        crate::tools::register_exploration_host(session_id, Some(host));
        let state = SessionLoopState {
            session_id,
            events_tx,
            commands: super::super::bridge_commands(commands_rx),
            config: RigAgentConfig {
                moa: Some(MoaPass {
                    name: "mix".to_string(),
                    proposers: members(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        (state, commands_tx, events_rx)
    }

    fn pass_record(
        events: &crossbeam_channel::Receiver<crate::contract::ProviderEvent>,
    ) -> Option<MoaPassStarted> {
        events.try_iter().find_map(|event| {
            match event.clone().into_event().expect("conversation event") {
                Event::MoaPassStarted(record) => Some(record),
                _ => None,
            }
        })
    }

    /// Every error the pass put on the session's own event channel, in
    /// order -- what the aggregator's pane renders as error items.
    fn errors(events: &crossbeam_channel::Receiver<crate::contract::ProviderEvent>) -> Vec<String> {
        events
            .try_iter()
            .filter_map(
                |event| match event.clone().into_event().expect("conversation event") {
                    Event::Error(error) => Some(error.message),
                    _ => None,
                },
            )
            .collect()
    }

    /// The message being answered is carried once, as the message, and the
    /// conversation the prompt renders is what came before it.
    #[tokio::test]
    async fn the_proposer_prompt_carries_the_new_message_exactly_once() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, _events) = moa_state(host.clone());
        state
            .moa_conversation
            .record_owner("an earlier question".to_string());
        state
            .moa_conversation
            .record_answer("an earlier answer".to_string());

        let driver = tokio::spawn({
            let host = host.clone();
            async move {
                loop {
                    if host.events.lock().unwrap().len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                host.answer(0, "a");
                host.answer(1, "b");
            }
        });
        state.run_moa_pass("the new question").await;
        driver.await.unwrap();

        let prompt = host.started.lock().unwrap()[0].3.clone();
        assert_eq!(prompt.matches("the new question").count(), 1, "{prompt}");
        assert!(prompt.contains("an earlier answer"), "{prompt}");
        crate::tools::unregister_exploration_host(state.session_id);
    }

    #[tokio::test]
    async fn a_pass_launches_one_proposer_per_member_and_injects_their_answers() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, events) = moa_state(host.clone());

        let pass = tokio::spawn({
            // The pass blocks until every proposer answers, so the answers
            // are played from here while it waits.
            let host = host.clone();
            async move {
                tokio::task::yield_now().await;
                loop {
                    if host.events.lock().unwrap().len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                host.answer(0, "first answer");
                host.answer(1, "second answer");
            }
        });
        let outcome = state.run_moa_pass("the question").await;
        pass.await.unwrap();
        assert!(matches!(outcome, PassOutcome::Proceed));

        assert_eq!(
            host.started_members(),
            vec![
                ("synthetic".to_string(), "hf:a/A".to_string()),
                ("synthetic".to_string(), "hf:b/B".to_string()),
            ],
            "each member runs on its own provider and model"
        );

        let record = pass_record(&events).expect("the pass is recorded in the event log");
        assert_eq!(record.entry, "mix");
        assert_eq!(record.proposers.len(), 2);
        assert_eq!(record.proposers[0].model, "hf:a/A");

        let block = state.moa_turn.as_ref().expect("proposals installed");
        assert!(block.block.contains("first answer"), "{}", block.block);
        assert!(block.block.contains("second answer"), "{}", block.block);
        assert!(
            block
                .block
                .contains(&record.proposers[0].session_id.as_uuid().to_string()),
            "each answer names the session that produced it"
        );

        // The block is a provider-view projection, never canonical history.
        let (index, message) = state.moa_injection().expect("an injection");
        assert_eq!(index, 0, "it lands ahead of the turn's opening message");
        assert!(state.rig_history.is_empty());
        let projected = super::super::super::clearing::history_for_provider_request(
            &state.rig_history,
            &Default::default(),
            None,
            Some(&(index, message)),
        );
        assert_eq!(projected.len(), 1);
        assert!(
            errors(&events).is_empty(),
            "a pass every proposer contributed to reports no failure"
        );
        crate::tools::unregister_exploration_host(state.session_id);
    }

    /// A proposer that reaches outside the workspace root is refused the
    /// read and keeps going, so its answer still reaches the aggregator —
    /// the pass loses nothing to a stray path.
    #[tokio::test]
    async fn a_proposer_whose_read_was_refused_still_contributes_its_proposal() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, _events) = moa_state(host.clone());

        let driver = tokio::spawn({
            let host = host.clone();
            async move {
                loop {
                    if host.events.lock().unwrap().len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                host.answer_after_a_refused_read(0, "answer despite the refusal");
                host.answer(1, "second answer");
            }
        });
        let outcome = state.run_moa_pass("the question").await;
        driver.await.unwrap();
        assert!(matches!(outcome, PassOutcome::Proceed));

        let block = &state.moa_turn.as_ref().expect("proposals installed").block;
        assert!(block.contains("answer despite the refusal"), "{block}");
        assert!(block.contains("second answer"), "{block}");
        crate::tools::unregister_exploration_host(state.session_id);
    }

    #[tokio::test]
    async fn a_proposer_that_produces_nothing_does_not_fail_the_pass() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, events) = moa_state(host.clone());

        let driver = tokio::spawn({
            let host = host.clone();
            async move {
                loop {
                    if host.events.lock().unwrap().len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                host.answer(0, "the surviving answer");
                host.fail(1);
            }
        });
        let outcome = state.run_moa_pass("the question").await;
        driver.await.unwrap();
        assert!(matches!(outcome, PassOutcome::Proceed));

        let block = state.moa_turn.as_ref().expect("the pass still proceeds");
        assert!(block.block.contains("the surviving answer"));
        assert!(
            !block.block.contains("Answer 2"),
            "only usable answers are numbered: {}",
            block.block
        );
        assert_eq!(
            errors(&events),
            vec![
                "moa `mix` proposer synthetic/hf:b/B contributed nothing (the task session \
                 terminated before finishing)"
                    .to_string()
            ],
            "the proposer that contributed nothing is reported in the aggregator's pane"
        );
        crate::tools::unregister_exploration_host(state.session_id);
    }

    #[tokio::test]
    async fn no_usable_proposal_leaves_the_aggregator_to_answer_alone() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, events) = moa_state(host.clone());

        let driver = tokio::spawn({
            let host = host.clone();
            async move {
                loop {
                    if host.events.lock().unwrap().len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                host.fail(0);
                host.fail(1);
            }
        });
        let outcome = state.run_moa_pass("the question").await;
        driver.await.unwrap();

        assert!(matches!(outcome, PassOutcome::Proceed));
        assert!(state.moa_turn.is_none(), "nothing is injected");

        let reported = errors(&events);
        assert_eq!(reported.len(), 3, "{reported:?}");
        assert_eq!(
            reported
                .iter()
                .filter(|message| message
                    .contains("contributed nothing (the task session terminated before finishing)"))
                .count(),
            2,
            "each proposer that contributed nothing is reported: {reported:?}"
        );
        assert_eq!(
            reported.last().map(String::as_str),
            Some("moa `mix` got no usable proposals; the aggregator answers alone"),
            "and the pass says the aggregator is answering alone: {reported:?}"
        );
        crate::tools::unregister_exploration_host(state.session_id);
    }

    /// A member on an entry whose key variable is unset is never launched:
    /// such a session answers from the deterministic fallback responder,
    /// and that text is indistinguishable from a model's answer once the
    /// event fold has it.
    #[tokio::test]
    async fn a_member_on_a_key_less_entry_is_never_launched() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, events) = moa_state(host.clone());
        state.config.moa = Some(MoaPass {
            name: "mix".to_string(),
            proposers: vec![
                member("hf:a/A"),
                MoaMember {
                    api_key_present: false,
                    api_key_env: "HORIZON_TEST_MOA_MISSING_KEY".to_string(),
                    ..MoaMember::new("keyless".to_string(), "hf:b/B".to_string())
                },
            ],
        });

        let driver = tokio::spawn({
            let host = host.clone();
            async move {
                loop {
                    if !host.events.lock().unwrap().is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                host.answer(0, "the available member's answer");
            }
        });
        let outcome = state.run_moa_pass("the question").await;
        driver.await.unwrap();
        assert!(matches!(outcome, PassOutcome::Proceed));

        assert_eq!(
            host.started_members(),
            vec![("synthetic".to_string(), "hf:a/A".to_string())],
            "only the available member was asked"
        );
        let record = pass_record(&events).expect("the pass is recorded");
        assert_eq!(
            record.proposers.len(),
            1,
            "a member that was never launched is not a proposer session"
        );
        let block = state.moa_turn.as_ref().expect("the pass still proceeds");
        assert!(block.block.contains("the available member's answer"));
        assert!(
            !block.block.contains("Answer 2"),
            "the key-less member contributes nothing: {}",
            block.block
        );
        crate::tools::unregister_exploration_host(state.session_id);
    }

    /// With every member on a key-less entry nothing is launched at all and
    /// the aggregator answers alone.
    #[tokio::test]
    async fn every_member_key_less_launches_nothing() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, _events) = moa_state(host.clone());
        state.config.moa = Some(MoaPass {
            name: "mix".to_string(),
            proposers: vec![MoaMember {
                api_key_present: false,
                api_key_env: "HORIZON_TEST_MOA_MISSING_KEY".to_string(),
                ..MoaMember::new("keyless".to_string(), "hf:b/B".to_string())
            }],
        });

        let outcome = state.run_moa_pass("the question").await;
        assert!(matches!(outcome, PassOutcome::Proceed));
        assert!(host.started_members().is_empty());
        assert!(state.moa_turn.is_none());
        crate::tools::unregister_exploration_host(state.session_id);
    }

    #[tokio::test]
    async fn a_member_whose_session_cannot_start_is_skipped() {
        let host = Arc::new(ScriptedHost {
            refuse: true,
            ..Default::default()
        });
        let (mut state, _commands, events) = moa_state(host.clone());

        let outcome = state.run_moa_pass("the question").await;
        assert!(matches!(outcome, PassOutcome::Proceed));
        assert!(state.moa_turn.is_none());
        let record = pass_record(&events).expect("the pass is still recorded");
        assert!(
            record.proposers.is_empty(),
            "a member that never started is not recorded as a proposer session"
        );
        crate::tools::unregister_exploration_host(state.session_id);
    }

    #[tokio::test]
    async fn cancelling_the_turn_stops_the_proposers() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, commands, _events) = moa_state(host.clone());

        let driver = tokio::spawn({
            let host = host.clone();
            async move {
                loop {
                    if host.events.lock().unwrap().len() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let _ = commands.send(Command::Cancel { request_id: None });
            }
        });
        let outcome = state.run_moa_pass("the question").await;
        driver.await.unwrap();

        assert!(matches!(outcome, PassOutcome::Cancelled));
        assert!(state.moa_turn.is_none());
        let terminated = host.terminated.lock().unwrap().clone();
        assert_eq!(
            terminated.len(),
            2,
            "every launched proposer session is terminated"
        );
        crate::tools::unregister_exploration_host(state.session_id);
    }

    #[test]
    fn the_conversation_keeps_owner_messages_and_the_last_answer_per_turn() {
        let mut conversation = MoaConversation::default();
        conversation.record_answer("provider initialized".to_string());
        conversation.record_owner("first question".to_string());
        conversation.record_answer("partial".to_string());
        conversation.record_answer("final answer".to_string());
        conversation.record_owner("second question".to_string());
        let rendered = conversation.render();
        assert_eq!(
            rendered, "User:\nfirst question\n\nAssistant:\nfinal answer\n\nUser:\nsecond question",
            "{rendered}"
        );
    }

    #[test]
    fn the_conversation_rebuilt_from_events_matches_the_live_one() {
        let message = |role, text: &str| {
            Event::MessageCommitted(crate::contract::Message {
                role,
                text: text.to_string(),
            })
        };
        let events = vec![
            message(MessageRole::Assistant, "provider initialized"),
            message(MessageRole::User, "first question"),
            message(MessageRole::TaskNotification, "a task finished"),
            message(MessageRole::AutoContinue, "keep going"),
            message(MessageRole::Assistant, "final answer"),
        ];
        let rebuilt = MoaConversation::from_events(&events);
        assert_eq!(
            rebuilt.render(),
            "User:\nfirst question\n\nAssistant:\nfinal answer",
            "notifications and auto-continue prompts are not part of it"
        );
    }

    #[test]
    fn a_proposal_that_continued_the_transcript_is_trimmed_to_its_answer() {
        assert_eq!(
            sanitize_proposal("Assistant:\nthe answer\n\nUser:\nwhat about x?"),
            Some("the answer".to_string())
        );
        assert_eq!(
            sanitize_proposal("  plain answer "),
            Some("plain answer".to_string())
        );
        assert_eq!(sanitize_proposal("Assistant:\n   "), None);
    }

    #[test]
    fn the_proposer_prompt_carries_the_conversation_and_the_new_message() {
        let mut conversation = MoaConversation::default();
        conversation.record_owner("earlier".to_string());
        conversation.record_answer("earlier answer".to_string());
        let prompt = proposer_prompt(&conversation, "the new question");
        assert!(prompt.contains("User:\nearlier"), "{prompt}");
        assert!(prompt.contains("Assistant:\nearlier answer"), "{prompt}");
        assert!(prompt.ends_with("the new question"), "{prompt}");
    }
    #[test]
    fn dropping_a_pass_stops_all_proposers_without_an_explicit_abort() {
        let host = Arc::new(ScriptedHost::default());
        let session = SessionId::new();
        let pass = crate::tools::moa::launch(
            session,
            crate::tools::moa::PreparedPass::new(session, host.clone()),
            &members(),
            "question",
        );
        assert_eq!(pass.launched.len(), 2);
        drop(pass);
        assert!(crate::tools::drain_session_work(
            session,
            std::time::Duration::from_secs(2)
        ));
        assert_eq!(host.terminated.lock().unwrap().len(), 2);
    }

    #[test]
    fn unwinding_a_pass_stops_already_launched_proposers() {
        let host = Arc::new(ScriptedHost::default());
        let session = SessionId::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pass = crate::tools::moa::launch(
                session,
                crate::tools::moa::PreparedPass::new(session, host.clone()),
                &members(),
                "question",
            );
            panic!("aggregator failed");
        }));
        assert!(result.is_err());
        assert!(crate::tools::drain_session_work(
            session,
            std::time::Duration::from_secs(2)
        ));
        assert_eq!(host.terminated.lock().unwrap().len(), 2);
    }
    #[test]
    fn teardown_between_acquiring_the_host_and_launch_does_not_start_children() {
        let host = Arc::new(ScriptedHost::default());
        let session = SessionId::new();
        crate::tools::register_exploration_host(session, Some(host.clone()));
        let prepared = crate::tools::moa::exploration_host(session).unwrap();
        crate::tools::unregister_session_runtime(session);
        let pass = crate::tools::moa::launch(session, prepared, &members(), "question");
        assert!(pass.launched.is_empty());
        assert!(host.started_members().is_empty());
        drop(pass);
        assert!(crate::tools::drain_session_work(
            session,
            std::time::Duration::ZERO
        ));
    }
    #[tokio::test]
    async fn dropping_the_waiting_pass_future_stops_its_proposers() {
        let host = Arc::new(ScriptedHost::default());
        let (mut state, _commands, _events) = moa_state(host.clone());
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(10),
            state.run_moa_pass("question")
        )
        .await
        .is_err());
        assert!(crate::tools::drain_session_work(
            state.session_id,
            std::time::Duration::from_secs(2)
        ));
        assert_eq!(host.started_members().len(), 2);
        assert_eq!(host.terminated.lock().unwrap().len(), 2);
        crate::tools::unregister_exploration_host(state.session_id);
    }
}
