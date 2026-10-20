//! The turn-continuation check (`docs/agent-decision-model-design.md`, "The
//! turn-continuation check"): after a round the model ended with a plain
//! reply, the decision model is asked whether the model or the user should
//! act next, and the turn continues when the answer is clearly `model`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::completion::CompletionStop;
use super::super::conversation::Prompt;
use super::state::SessionLoopState;
use super::{deterministic_rig_response, TurnCompletion};
use crate::config::RigAgentConfig;
use crate::contract::{
    Command, ContinuationVerdict, ConversationInputKind, Event, Message as AgentMessage,
    MessageRole, SessionState,
};
use crate::decision::{Answer, DecisionClient, DecisionRequest, HttpDecisionClient, Question};
use crate::roles::RoleDefinition;

/// Moves the continuation threshold for a measurement run.
pub(crate) const CONTINUATION_THRESHOLD_VAR: &str = "HORIZON_AGENT_CONTINUATION_THRESHOLD";
const DEFAULT_CONTINUATION_THRESHOLD: f64 = 0.8;
/// Harness continuations one turn may run.
const MAX_CONTINUATIONS_PER_TURN: u32 = 3;
const DECISION_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-text character bound on what `state` carries.
const STATE_TEXT_LIMIT: usize = 16_000;
const FALLBACK_ERROR_LIMIT: usize = 2_000;

const QUESTION: &str = "next_speaker";
const USER: &str = "user";
const MODEL: &str = "model";

const USER_CRITERION: &str = "The user should act next: the reply ends with a direct question, \
     needs a decision, access, or information only the user can give, or reports the request \
     complete.";
const MODEL_CRITERION: &str = "The assistant should act next: the reply states an immediate \
     next action the assistant itself will take (including a tool call it describes but did not \
     make), or is clearly incomplete against the request.";
const INSTRUCTIONS: &str = "Decide who should act next, given the user's request and the \
     assistant's latest reply. Answer user when the reply ends with a direct question, needs a \
     decision, access, or information only the user can give, or reports the request complete. \
     Answer model when the reply states an immediate next action the assistant itself will take \
     (including a tool call it describes but did not make), or is clearly incomplete against the \
     request. When in doubt, answer user.";

/// The message a continued turn runs on.
pub(crate) const CONTINUATION_PROMPT: &str = "Your previous reply announced a next step without \
     taking it. Continue the work. End the turn only when the request is complete or you need \
     something only the user can give (a decision, access, or information).";

/// The installed check: a decision connection and the threshold read when
/// the session started.
#[derive(Clone)]
pub(crate) struct ContinuationCheck {
    pub(crate) client: Arc<dyn DecisionClient>,
    pub(crate) model: String,
    pub(crate) threshold: f64,
    pub(crate) timeout: Duration,
}

impl ContinuationCheck {
    /// `Some` when the session has a decision model and its role is not an
    /// exploration role: an exploration child's end of turn is consumed by
    /// its parent.
    pub(crate) fn for_session(
        config: &RigAgentConfig,
        role: Option<&'static RoleDefinition>,
    ) -> Option<Self> {
        let decision = config.decision.as_ref()?;
        if role.is_some_and(|role| {
            crate::roles::is_exploration(&crate::roles::RoleId(role.id.to_string()))
        }) {
            return None;
        }
        Some(Self {
            model: decision.model.clone(),
            client: Arc::new(HttpDecisionClient::new(decision.clone())),
            threshold: resolve_continuation_threshold(
                std::env::var(CONTINUATION_THRESHOLD_VAR).ok(),
            ),
            timeout: DECISION_TIMEOUT,
        })
    }
}

/// Per-turn inputs to the check, reset when an interaction starts.
#[derive(Clone, Default)]
pub(crate) struct ContinuationState {
    pub(crate) check: Option<ContinuationCheck>,
    /// The text of the input that started the current turn.
    pub(crate) request: String,
    /// Tool calls the provider rounds of the current turn requested.
    pub(crate) tool_calls: usize,
    /// Harness continuations the current turn has run.
    pub(crate) count: u32,
}

impl ContinuationState {
    pub(crate) fn begin_turn(&mut self, request: &str) {
        self.request = request.to_string();
        self.tool_calls = 0;
        self.count = 0;
    }
}

/// A parseable value clamped into `0..=1` wins; unset keeps the default; an
/// unparseable value keeps the default and says so once.
pub(crate) fn resolve_continuation_threshold(raw: Option<String>) -> f64 {
    let Some(raw) = raw else {
        return DEFAULT_CONTINUATION_THRESHOLD;
    };
    match raw.trim().parse::<f64>() {
        Ok(value) if !value.is_nan() => value.clamp(0.0, 1.0),
        _ => {
            eprintln!(
                "horizon-agent: {CONTINUATION_THRESHOLD_VAR}={raw:?} is not a number; using \
                 {DEFAULT_CONTINUATION_THRESHOLD}"
            );
            DEFAULT_CONTINUATION_THRESHOLD
        }
    }
}

fn head_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

fn tail_chars(text: &str, limit: usize) -> &str {
    let count = text.chars().count();
    if count <= limit {
        return text;
    }
    let (index, _) = text
        .char_indices()
        .nth(count - limit)
        .expect("count exceeds limit");
    &text[index..]
}

pub(crate) fn continuation_request(
    model: &str,
    user_request: &str,
    assistant_reply: &str,
    tool_calls_in_turn: usize,
    continuations_so_far: u32,
) -> DecisionRequest {
    let criteria = BTreeMap::from([
        (USER.to_string(), Some(USER_CRITERION.to_string())),
        (MODEL.to_string(), Some(MODEL_CRITERION.to_string())),
    ]);
    DecisionRequest {
        model: model.to_string(),
        state: json!({
            "user_request": head_chars(user_request, STATE_TEXT_LIMIT),
            "assistant_reply": tail_chars(assistant_reply, STATE_TEXT_LIMIT),
            "tool_calls_in_turn": tool_calls_in_turn,
            "harness_continuations_so_far": continuations_so_far,
        }),
        questions: BTreeMap::from([(
            QUESTION.to_string(),
            Question::Choice {
                instructions: INSTRUCTIONS.to_string(),
                criteria,
            },
        )]),
    }
}

impl SessionLoopState {
    /// Runs the turn-continuation check on `outcome` and, while the decision
    /// model clearly answers `model`, continues the turn with
    /// [`CONTINUATION_PROMPT`] (no `TurnEnded` in between). Returns `None`
    /// only when the check is installed and a shutdown is queued, as
    /// truncation recovery does; otherwise the outcome to carry on
    /// with, which may be a re-run's (completing, with tool calls, or
    /// truncated).
    pub(crate) async fn handle_continuation_check(
        &mut self,
        mut outcome: TurnCompletion,
    ) -> Option<TurnCompletion> {
        loop {
            let Some(check) = self.continuation.check.clone() else {
                return Some(outcome);
            };
            if !outcome.is_completing() || !self.config.model_in_play() {
                return Some(outcome);
            }
            self.collect_inputs();
            if self.shutdown_queued() {
                return None;
            }
            if self.has_pending_stop() || self.inputs.has_additions() {
                return Some(outcome);
            }
            let reply = match &outcome.stop {
                CompletionStop::Finished { text } if !text.trim().is_empty() => text.clone(),
                _ => return Some(outcome),
            };
            let so_far = self.continuation.count;
            if so_far >= MAX_CONTINUATIONS_PER_TURN {
                self.emit_verdict(ContinuationVerdict {
                    next_speaker: None,
                    model_probability: None,
                    user_probability: None,
                    threshold: check.threshold,
                    continued: false,
                    continuations_so_far: so_far,
                    latency_ms: 0,
                    fallback_reason: Some("cap_reached".into()),
                    fallback_error: None,
                });
                return Some(outcome);
            }

            let request = continuation_request(
                &check.model,
                &self.continuation.request,
                &reply,
                self.continuation.tool_calls,
                so_far,
            );
            let started = Instant::now();
            let answered = tokio::time::timeout(check.timeout, check.client.decide(request)).await;
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let mut verdict = ContinuationVerdict {
                next_speaker: None,
                model_probability: None,
                user_probability: None,
                threshold: check.threshold,
                continued: false,
                continuations_so_far: so_far,
                latency_ms,
                fallback_reason: None,
                fallback_error: None,
            };
            match answered {
                Err(_) => verdict.fallback_reason = Some("timeout".into()),
                Ok(Err(error)) => {
                    verdict.fallback_reason = Some("client_error".into());
                    verdict.fallback_error =
                        Some(head_chars(&format!("{error:#}"), FALLBACK_ERROR_LIMIT).to_string());
                }
                Ok(Ok(response)) => match response.answers.get(QUESTION) {
                    Some(Answer::Choice {
                        choice,
                        probabilities,
                        ..
                    }) if choice == USER || choice == MODEL => {
                        verdict.next_speaker = Some(choice.clone());
                        verdict.model_probability = probabilities.get(MODEL).copied();
                        verdict.user_probability = probabilities.get(USER).copied();
                        verdict.continued = choice == MODEL
                            && verdict
                                .model_probability
                                .is_some_and(|probability| probability >= check.threshold);
                    }
                    _ => verdict.fallback_reason = Some("unparseable".into()),
                },
            }
            // Input that arrived during the call takes precedence over a
            // continuation.
            if verdict.continued {
                self.collect_inputs();
                if self.shutdown_queued() {
                    verdict.continued = false;
                    self.emit_verdict(verdict);
                    return None;
                }
                if self.has_pending_stop() || self.inputs.has_additions() {
                    verdict.continued = false;
                }
            }
            let continued = verdict.continued;
            self.emit_verdict(verdict);
            if !continued {
                return Some(outcome);
            }

            self.continuation.count += 1;
            let _ = self
                .events_tx
                .send(Event::StateChanged(SessionState::Running).into());
            let _ = self.events_tx.send(
                Event::MessageCommitted(AgentMessage {
                    role: MessageRole::AutoContinue,
                    text: CONTINUATION_PROMPT.to_string(),
                })
                .into(),
            );
            outcome = self
                .run_cancellable_turn(
                    Prompt::input(ConversationInputKind::Continuation, CONTINUATION_PROMPT),
                    || deterministic_rig_response("next-speaker continuation"),
                )
                .await;
            if outcome.stop.truncation().is_some() {
                return Some(outcome);
            }
        }
    }

    fn shutdown_queued(&self) -> bool {
        self.inbox
            .iter()
            .any(|command| matches!(command, Command::Shutdown))
    }

    fn emit_verdict(&self, verdict: ContinuationVerdict) {
        let _ = self
            .events_tx
            .send(Event::ContinuationVerdict(verdict).into());
    }
}

#[cfg(test)]
mod tests;
