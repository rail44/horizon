//! Non-cancelling input routing. A pending request can inform the current
//! answer without acquiring that answer's destination.
use std::collections::{HashSet, VecDeque};

use super::state::SessionLoopState;
use crate::contract::{Command, Event, InputResult, SessionInput, SessionInputOutcome};

#[derive(Default)]
pub(crate) struct Inputs {
    seen: HashSet<String>,
    active: Option<ActiveInputs>,
    paused: bool,
    queued: VecDeque<SessionInput>,
    additions: Vec<String>,
}

/// The first request owns the reply destination and receipt identity. Extra
/// passive inputs may join the answer but cannot change either of them.
struct ActiveInputs {
    first: SessionInput,
    rest: Vec<SessionInput>,
}

impl ActiveInputs {
    fn new(first: SessionInput) -> Self {
        Self {
            first,
            rest: Vec::new(),
        }
    }

    fn ids(&self) -> Vec<String> {
        std::iter::once(&self.first)
            .chain(&self.rest)
            .map(|input| input.id.clone())
            .collect()
    }

    fn finish(self, outcome: InputResult) -> SessionInputOutcome {
        SessionInputOutcome {
            delivery_id: format!("input-result:{}", self.first.id),
            reply_to: self.first.reply_to.clone(),
            input_ids: self.ids(),
            outcome,
        }
    }
}

fn present_input(input: &SessionInput) -> String {
    format!(
        "[Input from {}]\n[Final answer destination: {}]\n{}",
        input.origin,
        input
            .reply_to
            .as_deref()
            .unwrap_or("no automatic reply requested"),
        input.text,
    )
}

impl Inputs {
    pub(super) fn restore(events: &[Event]) -> Self {
        let completed: HashSet<&str> = events
            .iter()
            .filter_map(|event| {
                if let Event::InputOutcome(outcome) = event {
                    Some(outcome.input_ids.iter().map(String::as_str))
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        let mut inputs = Self::default();
        for event in events {
            match event {
                Event::InputAccepted(input) => {
                    if input.resume_work {
                        inputs.paused = false;
                    }
                    if inputs.seen.insert(input.id.clone())
                        && !completed.contains(input.id.as_str())
                    {
                        inputs.queued.push_back(input.clone());
                    }
                }
                Event::InputQueuePaused(paused) => inputs.paused = *paused,
                Event::InputStarted(_) => inputs.paused = false,
                _ => {}
            }
        }
        inputs
    }

    pub(super) fn accept(&mut self, input: SessionInput, busy: bool) {
        if !self.seen.insert(input.id.clone()) {
            return;
        }
        if busy {
            self.additions.push(present_input(&input));
            match &mut self.active {
                Some(active)
                    if input.reply_to.is_none() || input.reply_to == active.first.reply_to =>
                {
                    active.rest.push(input);
                    return;
                }
                None if input.reply_to.is_none() => {
                    self.active = Some(ActiveInputs::new(input));
                    return;
                }
                _ => {}
            }
        }
        self.queued.push_back(input);
    }

    pub(super) fn active_ids(&self) -> Vec<String> {
        self.active
            .as_ref()
            .map_or_else(Vec::new, ActiveInputs::ids)
    }

    pub(super) fn is_paused(&self) -> bool {
        self.paused
    }

    /// Admission state and its durable marker change together, including when
    /// a provider future borrows the rest of SessionLoopState.
    pub(super) fn set_paused(&mut self, paused: bool) -> Option<Event> {
        let changed = self.paused != paused;
        self.paused = paused;
        changed.then_some(Event::InputQueuePaused(paused))
    }

    pub(super) fn start_next(&mut self) -> Option<String> {
        if self.paused || self.active.is_some() {
            return None;
        }
        let first = self.queued.pop_front()?;
        let mut text = present_input(&first);
        let mut active = ActiveInputs::new(first);
        while self
            .queued
            .front()
            .is_some_and(|input| input.reply_to == active.first.reply_to)
        {
            let input = self.queued.pop_front().unwrap();
            text.push_str("\n\n");
            text.push_str(&present_input(&input));
            active.rest.push(input);
        }
        self.active = Some(active);
        Some(text)
    }

    pub(super) fn note_environment_failure(&mut self, message: String) {
        self.additions.push(format!(
            "Environment activation failed; consultation remains available: {message}"
        ));
    }

    pub(super) fn take_additions(&mut self) -> Option<String> {
        (!self.additions.is_empty()).then(|| std::mem::take(&mut self.additions).join("\n\n"))
    }

    pub(super) fn finish(&mut self, outcome: InputResult) -> Option<SessionInputOutcome> {
        self.active.take().map(|active| active.finish(outcome))
    }
}

impl SessionLoopState {
    /// Collect commands and select queued work only after lifecycle controls
    /// and any environment handoff have had a chance to stop it.
    pub(super) async fn prepare_next_input(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            self.inbox.push_back(command);
        }
        self.prioritize_lifecycle_control();
        if self.inbox.is_empty() && !self.execution.has_pending_tools() {
            self.activate_environment().await;
            if !self.inputs.is_paused() && !self.has_pending_stop() {
                if let Some(text) = self.inputs.start_next() {
                    self.record_active_input();
                    self.inbox.push_front(Command::UserMessage { text });
                }
            }
        }
    }

    fn prioritize_lifecycle_control(&mut self) {
        // Lifecycle controls must run before starting queued work, including
        // controls observed while the provider awaited an environment swap.
        if let Some(index) = self
            .inbox
            .iter()
            .position(|command| matches!(command, Command::Shutdown | Command::Cancel { .. }))
        {
            let mut preceding = VecDeque::new();
            for _ in 0..index {
                match self.inbox.pop_front().unwrap() {
                    Command::SessionInput(input) => self
                        .inputs
                        .accept(input, self.execution.has_pending_tools()),
                    Command::ToolCallReissued(identity) => self.note_tool_call_reissued(identity),
                    command => preceding.push_back(command),
                }
            }
            let control = self.inbox.pop_front().unwrap();
            preceding.append(&mut self.inbox);
            self.inbox = preceding;
            self.record_active_input();
            self.inbox.push_front(control);
        }
    }

    pub(super) fn pause_inputs(&mut self, paused: bool) {
        if let Some(event) = self.inputs.set_paused(paused) {
            let _ = self.events_tx.send(event.into());
        }
    }

    pub(super) fn has_pending_stop(&self) -> bool {
        self.inbox
            .iter()
            .any(|command| matches!(command, Command::Cancel { .. } | Command::Shutdown))
    }

    pub(super) fn collect_inputs(&mut self) {
        // The bridge can deliver inputs between receiving a tool result and
        // building the next request. Preserve non-input command ordering.
        while let Ok(command) = self.commands.try_recv() {
            self.inbox.push_back(command);
        }
        let mut remaining = VecDeque::new();
        while let Some(command) = self.inbox.pop_front() {
            match command {
                Command::SessionInput(input) => {
                    self.inputs.accept(input, true);
                    self.record_active_input();
                }
                Command::ActivateWorktree { base } => self.activation.push_back(base),
                other => remaining.push_back(other),
            }
        }
        self.inbox = remaining;
    }

    pub(super) fn record_active_input(&self) {
        let ids = self.inputs.active_ids();
        if !ids.is_empty() {
            let _ = self.events_tx.send(Event::InputStarted(ids).into());
        }
    }

    pub(super) fn finish_input(&mut self, outcome: InputResult) {
        if let Some(outcome) = self.inputs.finish(outcome) {
            let _ = self.events_tx.send(Event::InputOutcome(outcome).into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(id: &str, destination: Option<&str>) -> SessionInput {
        SessionInput {
            resume_work: false,
            id: id.into(),
            origin: "owner".into(),
            text: id.into(),
            reply_to: destination.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn lifecycle_control_accepts_preceding_inputs_without_starting_queued_work() {
        for control in [Command::Cancel { request_id: None }, Command::Shutdown] {
            let (send, receive) = tokio::sync::mpsc::unbounded_channel();
            let mut state = SessionLoopState {
                commands: receive,
                ..Default::default()
            };
            state.inbox.push_back(Command::ContinueTurn);
            send.send(Command::SessionInput(input("before", Some("first"))))
                .unwrap();
            send.send(control.clone()).unwrap();
            let later = Command::SessionInput(input("after", Some("second")));
            send.send(later.clone()).unwrap();

            state.prepare_next_input().await;

            assert_eq!(state.inbox, [control, Command::ContinueTurn, later]);
            assert!(state.inputs.active_ids().is_empty());
            assert_eq!(
                state.inputs.start_next(),
                Some(present_input(&input("before", Some("first"))))
            );
            state.inputs.finish(InputResult::Interrupted);
            assert!(state.inputs.start_next().is_none());
        }
    }

    #[test]
    fn additions_merge_but_other_destinations_queue_and_passive_input_never_retargets() {
        let mut inputs = Inputs::default();
        inputs.accept(input("a", Some("task:1")), false);
        assert_eq!(
            inputs.start_next(),
            Some(present_input(&input("a", Some("task:1"))))
        );
        inputs.accept(input("b", Some("task:1")), true);
        inputs.accept(input("c", Some("task:2")), true);
        inputs.accept(input("d", None), true);
        inputs.accept(input("b", Some("task:1")), true);
        let additions = inputs.take_additions().unwrap();
        assert!(additions.contains('c'));
        let result = inputs
            .finish(InputResult::Success {
                text: "final".into(),
            })
            .unwrap();
        assert_eq!(result.reply_to.as_deref(), Some("task:1"));
        assert_eq!(result.input_ids, ["a", "b", "d"]);
        assert_eq!(
            inputs.start_next(),
            Some(present_input(&input("c", Some("task:2"))))
        );
        assert_eq!(
            inputs
                .finish(InputResult::Interrupted)
                .unwrap()
                .reply_to
                .as_deref(),
            Some("task:2")
        );
    }
    #[test]
    fn replay_preserves_receipt_deduplication_and_outbox_acknowledgements() {
        let a = input("a", Some("destination"));
        let b = input("b", None);
        let result = SessionInputOutcome {
            input_ids: vec![a.id.clone()],
            delivery_id: "delivery".into(),
            reply_to: a.reply_to.clone(),
            outcome: InputResult::Interrupted,
        };
        let mut events = vec![
            Event::InputAccepted(a.clone()),
            Event::InputAccepted(b),
            Event::InputOutcome(result.clone()),
        ];
        let mut restored = Inputs::restore(&events);
        restored.accept(a, false);
        assert_eq!(
            restored.start_next(),
            Some(present_input(&input("b", None)))
        );
        assert_eq!(crate::contract::pending_input_outcomes(&events), [result]);
        events.push(Event::DeliveryAcknowledged("delivery".into()));
        assert!(crate::contract::pending_input_outcomes(&events).is_empty());
    }

    #[test]
    fn initial_and_busy_inputs_show_origin_and_opaque_answer_destination() {
        let destination = r#"{"session":"third-session"}"#;
        let mut inputs = Inputs::default();
        inputs.accept(input("initial", Some(destination)), false);
        assert_eq!(
            inputs.start_next().unwrap(),
            "[Input from owner]\n[Final answer destination: {\"session\":\"third-session\"}]\ninitial",
        );
        inputs.accept(input("addition", Some(destination)), true);
        inputs.accept(input("notification", None), true);
        assert_eq!(
            inputs.take_additions().unwrap(),
            "[Input from owner]\n[Final answer destination: {\"session\":\"third-session\"}]\naddition\n\n[Input from owner]\n[Final answer destination: no automatic reply requested]\nnotification",
        );
        assert_eq!(
            inputs
                .finish(InputResult::Interrupted)
                .unwrap()
                .reply_to
                .as_deref(),
            Some(destination)
        );
    }
}

#[cfg(test)]
mod outcome_tests {
    use super::*;
    use crate::providers::rig::completion::TurnCompletion;

    #[test]
    fn failed_and_interrupted_partial_text_never_become_successful_results() {
        for cancelled in [false, true] {
            let (sender, receiver) = crossbeam_channel::unbounded();
            let mut state = SessionLoopState {
                events_tx: sender,
                ..Default::default()
            };
            state.inputs.accept(
                SessionInput {
                    resume_work: false,
                    id: "request".into(),
                    origin: "owner".into(),
                    text: "work".into(),
                    reply_to: Some("opaque".into()),
                },
                false,
            );
            state.inputs.start_next();
            state.apply_turn_outcome(TurnCompletion {
                stop: if cancelled {
                    crate::providers::rig::completion::CompletionStop::Cancelled
                } else {
                    crate::providers::rig::completion::CompletionStop::Failed
                },
                ..Default::default()
            });
            let outcomes: Vec<_> = receiver
                .try_iter()
                .filter_map(
                    |event| match event.clone().into_event().expect("conversation event") {
                        Event::InputOutcome(outcome) => Some(outcome),
                        _ => None,
                    },
                )
                .collect();
            assert_eq!(outcomes.len(), 1);
            assert!(!matches!(outcomes[0].outcome, InputResult::Success { .. }));
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn request(id: &str) -> SessionInput {
        SessionInput {
            id: id.into(),
            origin: "test".into(),
            text: id.into(),
            reply_to: Some(id.into()),
            resume_work: false,
        }
    }

    #[test]
    fn an_empty_queue_stop_survives_later_passive_input_and_replay() {
        let mut inputs = Inputs::default();
        let pause = inputs
            .set_paused(true)
            .expect("empty queues also persist their stop");
        let pending = request("later");
        inputs.accept(pending.clone(), false);
        assert!(inputs.start_next().is_none());
        let mut restored = Inputs::restore(&[pause, Event::InputAccepted(pending)]);
        assert!(
            restored.start_next().is_none(),
            "replay must preserve admission"
        );
        restored.set_paused(false);
        assert!(restored.start_next().unwrap().ends_with("later"));
    }

    #[test]
    fn replay_preserves_control_order_and_never_requeues_completed_inputs() {
        let done = request("done");
        let pending = request("pending");
        let history = vec![
            Event::InputAccepted(done.clone()),
            Event::InputStarted(vec![done.id.clone()]),
            Event::InputAccepted(pending.clone()),
            Event::InputQueuePaused(true),
            Event::InputOutcome(ActiveInputs::new(done.clone()).finish(InputResult::Interrupted)),
        ];
        for resume in [
            None,
            Some(Event::InputQueuePaused(false)),
            Some(Event::InputAccepted(SessionInput {
                resume_work: true,
                ..pending.clone()
            })),
        ] {
            let mut events = history.clone();
            if let Some(event) = resume.clone() {
                events.push(event);
            }
            let mut inputs = Inputs::restore(&events);
            inputs.accept(done.clone(), false);
            assert_eq!(inputs.start_next().is_some(), resume.is_some());
            inputs.set_paused(false);
            if resume.is_none() {
                inputs.start_next().unwrap();
            }
            assert_eq!(
                inputs.finish(InputResult::Interrupted).unwrap().input_ids,
                ["pending"]
            );
            assert!(inputs.start_next().is_none());
        }
    }

    #[tokio::test]
    async fn a_stale_continue_cannot_restart_a_restored_paused_queue() {
        let (events_tx, events) = crossbeam_channel::unbounded();
        let mut state = SessionLoopState {
            inputs: Inputs::restore(&[
                Event::InputQueuePaused(true),
                Event::InputAccepted(request("pending")),
            ]),
            events_tx,
            ..Default::default()
        };
        state.continue_halted_turn().await;
        state.prepare_next_input().await;
        assert!(state.inbox.is_empty());
        assert!(state.inputs.is_paused());
        assert!(events.try_recv().is_err());
        state.pause_inputs(false);
        state.prepare_next_input().await;
        assert!(
            matches!(state.inbox.pop_front(), Some(Command::UserMessage { text }) if text.ends_with("pending"))
        );
        assert_eq!(state.inputs.active_ids(), ["pending"]);
    }
}
