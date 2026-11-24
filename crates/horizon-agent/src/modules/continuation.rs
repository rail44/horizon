//! The turn-continuation check (`docs/agent-decision-model-design.md`, "The
//! turn-continuation check") as a module of the continue stage: after a
//! round the model ended with a plain reply, the decision model is asked
//! whether the model or the user should act next, and the answer is to
//! continue when it is clearly `model`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;

use super::{RoundContext, RoundDecision, SessionModule};
use crate::config::RigAgentConfig;
use crate::contract::ContinuationVerdict;
use crate::decision::{Answer, DecisionClient, DecisionRequest, HttpDecisionClient, Question};
use crate::roles::RoleDefinition;

pub(crate) const CONTINUATION_THRESHOLD_VAR: &str = "HORIZON_AGENT_CONTINUATION_THRESHOLD";
pub(crate) const DEFAULT_CONTINUATION_THRESHOLD: f64 = 0.8;
/// Harness continuations one turn may run.
const MAX_CONTINUATIONS_PER_TURN: u32 = 3;
pub(crate) const DECISION_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-text character bound on what `state` carries.
pub(crate) const STATE_TEXT_LIMIT: usize = 16_000;
const FALLBACK_ERROR_LIMIT: usize = 2_000;

pub(crate) const QUESTION: &str = "next_speaker";
pub(crate) const USER: &str = "user";
pub(crate) const MODEL: &str = "model";

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

/// The decision-model continuation as a session module.
pub(crate) struct DecisionContinuation {
    check: ContinuationCheck,
}

impl DecisionContinuation {
    pub(crate) fn new(check: ContinuationCheck) -> Self {
        Self { check }
    }
}

#[async_trait]
impl SessionModule for DecisionContinuation {
    async fn after_round(&self, cx: &RoundContext<'_>) -> Option<RoundDecision> {
        let check = &self.check;
        let so_far = cx.continuations_so_far;
        let mut verdict = ContinuationVerdict {
            next_speaker: None,
            model_probability: None,
            user_probability: None,
            threshold: check.threshold,
            continued: false,
            continuations_so_far: so_far,
            latency_ms: 0,
            fallback_reason: None,
            fallback_error: None,
        };
        if so_far >= MAX_CONTINUATIONS_PER_TURN {
            verdict.fallback_reason = Some("cap_reached".into());
            return Some(RoundDecision {
                prompt: None,
                verdict,
            });
        }
        let request =
            continuation_request(&check.model, cx.request, cx.reply, cx.tool_calls, so_far);
        let started = Instant::now();
        let answered = tokio::time::timeout(check.timeout, check.client.decide(request)).await;
        verdict.latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
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
        Some(RoundDecision {
            prompt: verdict.continued.then_some(CONTINUATION_PROMPT),
            verdict,
        })
    }
}
