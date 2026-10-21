//! The inline approval judge -- `docs/agent-approval-design.md`'s "Judge
//! design" section, run on a decision model as
//! `docs/agent-decision-model-design.md`'s "The judge" describes, or on the
//! `small_model` connection when no decision model is available: an
//! asynchronous enforcing gate for every approval candidate. A safe verdict
//! follows the ordinary approved execution/retry path without exposing a
//! transient prompt; every failure or escalation preserves the ordinary
//! human approval path.
//!
//! ## Shape
//!
//! - [`JudgeHandle`] is the per-session-installed bundle (model id, backend,
//!   rate limiter, event-log writer) `ToolSessionBuilder::with_judge`
//!   carries -- see `handle`'s module doc. The backend is chosen once, at
//!   session start: the decision connection when one is available,
//!   otherwise the `small_model` connection, otherwise no judge at all.
//! - [`run_judge`] dispatches one candidate to the session's backend.
//! - [`run_decision_judge`] places one decision call: a single `noul`
//!   question, `ask_human`, whose answer is the probability that a human
//!   must approve the call. At or below the ask threshold the call
//!   auto-approves; above it, it escalates. There is no second stage: the
//!   gray zone belongs to the human. It takes a `&dyn DecisionClient`, so
//!   tests drive it against a mock.
//! - [`chat::run_chat_judge`] is the fallback backend's two-stage cascade:
//!   stage 1 (single-token, err-toward-block, cheap) auto-approves or flags;
//!   a flagged call runs stage 2 (chain-of-thought in the visible reply,
//!   provider-side reasoning off) for a final, parseable verdict. It takes a
//!   `&dyn ModelClient`, so tests drive it against a mock.
//! - Either backend escalates on a client error, an unreadable answer, or
//!   the timeout (fail-closed).
//! - [`request`] assembles the decision question's fixed instructions and
//!   the JSON state (prior user messages plus the raw arguments); [`prompt`]
//!   assembles the cascade's prompts (fixed system text, a
//!   per-call-id-delimited untrusted args region); [`parse`] turns each
//!   cascade stage's raw text back into a [`JudgeDecision`] (lenient
//!   parsing, never `logit_bias`); [`client`] is the cascade's mockable
//!   seam over rig's OpenAI-completions client; [`ratelimit`] is the
//!   judge-call budget; [`record`] is the durable verdict record;
//!   [`runtime`] is the dedicated tokio runtime judge calls are spawned
//!   onto.
//!
//! ## Wiring at the seam
//!
//! [`start_approval_gate`] is called only after the approval kind and reason
//! have been derived. It returns immediately: accepted work completes on
//! the session's normal async-results channel, while unavailable or
//! rate-limited work falls back to the human prompt synchronously.

mod chat;
mod client;
mod handle;
mod parse;
mod prompt;
mod ratelimit;
mod record;
mod request;
mod runtime;

use std::sync::Arc;

use crate::contract::{
    ApprovalKind, ApprovalRequest, Message, MessageRole, SessionId, ToolCallRequest,
};
use crate::decision::{Answer, DecisionClient};
use crate::frame::{AgentFrame, AgentFrameItem};
use crate::tools::{git_prefilter, GitPrefilterVerdict, ToolSessionState};

pub use handle::JudgeHandle;

use client::ModelClient;

/// The model a session's judge runs on, chosen once at session start --
/// see [`JudgeHandle::new`].
enum JudgeBackend {
    /// One `ask_human` question on the `decision_model` connection;
    /// `threshold` is the ask probability at or below which a call
    /// auto-approves.
    Decision {
        client: Arc<dyn DecisionClient>,
        threshold: f64,
    },
    /// The two-stage chat cascade on the `small_model` connection.
    Chat { client: Arc<dyn ModelClient> },
}

impl JudgeBackend {
    fn kind(&self) -> JudgeBackendKind {
        match self {
            Self::Decision { .. } => JudgeBackendKind::Decision,
            Self::Chat { .. } => JudgeBackendKind::Chat,
        }
    }

    /// The ask threshold; only the decision backend has one.
    fn threshold(&self) -> Option<f64> {
        match self {
            Self::Decision { threshold, .. } => Some(*threshold),
            Self::Chat { .. } => None,
        }
    }
}

/// Which backend produced a verdict, recorded as `judge_backend`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JudgeBackendKind {
    Decision,
    Chat,
}

impl JudgeBackendKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Chat => "chat",
        }
    }
}

/// The judge's per-call verdict -- see the module doc. `stage` is the
/// cascade stage that decided (chat verdicts only). `ask_probability` is
/// the decision model's raw `noul` for `ask_human` (decision verdicts
/// only). `confidence` is, for a decision verdict, the probability of the
/// verdict taken (`1 - p` for an auto-approval, `p` for an escalation); for
/// a chat verdict, stage 1's logprob-derived value when stage 1 decided,
/// and `None` after stage 2. Both are `None` on a fallback.
/// `fallback_error` carries the failure text when the fallback came from an
/// error, bounded by [`FALLBACK_ERROR_MAX_CHARS`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct JudgeVerdict {
    pub decision: JudgeDecision,
    pub backend: JudgeBackendKind,
    pub stage: Option<u8>,
    pub ask_probability: Option<f64>,
    pub confidence: Option<f64>,
    pub fallback_reason: Option<JudgeFallbackReason>,
    pub fallback_error: Option<String>,
}

impl JudgeVerdict {
    /// A fail-closed escalation with no model answer behind it.
    fn fallback(
        backend: JudgeBackendKind,
        reason: JudgeFallbackReason,
        error: Option<String>,
    ) -> Self {
        Self {
            decision: JudgeDecision::Escalate,
            backend,
            stage: None,
            ask_probability: None,
            confidence: None,
            fallback_reason: Some(reason),
            fallback_error: error,
        }
    }
}

/// Upper bound, in chars, on [`JudgeVerdict::fallback_error`]; an error
/// carrying a whole HTML response body would otherwise land in the event
/// log verbatim.
pub(crate) const FALLBACK_ERROR_MAX_CHARS: usize = 2000;

/// Truncates `text` to at most [`FALLBACK_ERROR_MAX_CHARS`] chars, cutting
/// only on a char boundary.
pub(crate) fn bounded_fallback_error(text: &str) -> String {
    match text.char_indices().nth(FALLBACK_ERROR_MAX_CHARS) {
        Some((byte_index, _)) => text[..byte_index].to_string(),
        None => text.to_string(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JudgeDecision {
    AutoApprove,
    Escalate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JudgeFallbackReason {
    ClientError,
    Unparseable,
    Timeout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalCandidate {
    pub request: ToolCallRequest,
    pub approval: ApprovalRequest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalJudgment {
    pub candidate: ApprovalCandidate,
    pub decision: JudgeDecision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApprovalGate {
    Pending,
    Human(Box<ApprovalCandidate>),
}

/// What the judge is allowed to see for one call -- `docs/agent-approval-
/// design.md`'s "Input restriction (the injection defense)": prior user
/// messages plus the raw tool-call arguments only, never tool results or
/// assistant prose. `tool_description` is populated only for this crate's
/// own built-in tools (a short, Horizon-authored, static string -- trusted
/// framing text, not attacker-influenced); left `None` for anything else
/// (there are no MCP/external tools wired into this crate yet -- see
/// `docs/agent-approval-design.md`'s "Tool description / schema" open
/// question for why that case stays unbuilt here).
#[derive(Clone, Debug)]
pub(crate) struct JudgeInput {
    pub(crate) identity: crate::contract::ToolCallIdentity,
    pub(crate) tool_id: String,
    pub(crate) args: serde_json::Value,
    pub(crate) tool_description: Option<String>,
    pub(crate) prior_user_messages: Vec<String>,
    /// Horizon-authored mediation output, a state field of its own beside
    /// the untrusted arguments (outside the untrusted argument region in the
    /// cascade's prompt).
    pub(crate) requested_filesystem_grants: Vec<horizon_sandbox::FilesystemGrant>,
    /// Canonical Horizon-derived hosts, a state field of its own beside the
    /// untrusted arguments (outside the untrusted argument region in the
    /// cascade's prompt). Empty for calls that do not request or use a domain grant.
    pub(crate) requested_domains: Vec<String>,
    /// Approval reruns this one call with the host process's ordinary
    /// authority rather than adding only the displayed narrow grants.
    pub(crate) host_execution_requested: bool,
    /// This call is a Git metadata operation (commit, rebase, etc.). The
    /// decision question's instructions, and the cascade's prompt, gain
    /// git-specific guidance when set.
    pub(crate) git_metadata_operation: bool,
}

pub(crate) struct JudgeApprovalContext {
    requested_filesystem_grants: Vec<horizon_sandbox::FilesystemGrant>,
    requested_domains: Vec<String>,
    host_execution_requested: bool,
    git_metadata_operation: bool,
}

/// One judgment on the session's backend.
async fn run_judge(model: &str, backend: &JudgeBackend, input: &JudgeInput) -> JudgeVerdict {
    match backend {
        JudgeBackend::Decision { client, threshold } => {
            run_decision_judge(model, client.as_ref(), input, *threshold).await
        }
        JudgeBackend::Chat { client } => chat::run_chat_judge(model, client.as_ref(), input).await,
    }
}

/// One decision call for one candidate. Never panics or propagates a
/// network error to the caller: anything but a `noul` answer to
/// `ask_human` resolves to [`JudgeDecision::Escalate`], the fail-safe
/// direction (`docs/agent-approval-design.md`'s "Judge-unreachable is
/// fail-safe").
async fn run_decision_judge(
    model: &str,
    client: &dyn DecisionClient,
    input: &JudgeInput,
    threshold: f64,
) -> JudgeVerdict {
    let response = match client.decide(request::decision_request(model, input)).await {
        Ok(response) => response,
        Err(error) => {
            return JudgeVerdict::fallback(
                JudgeBackendKind::Decision,
                JudgeFallbackReason::ClientError,
                Some(bounded_fallback_error(&format!("{error:#}"))),
            );
        }
    };
    let Some(Answer::Noul { noul }) = response.answers.get(request::ASK_HUMAN) else {
        return JudgeVerdict::fallback(
            JudgeBackendKind::Decision,
            JudgeFallbackReason::Unparseable,
            None,
        );
    };
    let noul = *noul;
    let (decision, confidence) = if noul <= threshold {
        (JudgeDecision::AutoApprove, 1.0 - noul)
    } else {
        (JudgeDecision::Escalate, noul)
    };
    JudgeVerdict {
        decision,
        backend: JudgeBackendKind::Decision,
        stage: None,
        ask_probability: Some(noul),
        confidence: Some(confidence),
        fallback_reason: None,
        fallback_error: None,
    }
}

/// Starts one enforcing judgment without blocking the session loop.
/// Missing judge configuration and rate-limit exhaustion preserve the
/// human approval flow immediately.
pub(crate) fn start_approval_gate(
    tool_state: &ToolSessionState,
    session_id: SessionId,
    mut candidate: ApprovalCandidate,
    result_tx: crossbeam_channel::Sender<crate::tools::ToolCompletion>,
) -> ApprovalGate {
    // Deterministic prefilter for Git metadata operations (board #38): the
    // prefilter no longer blocks any construct —
    // every GitOperation-classified command passes to the judge in full.
    // The HumanDirect branch is retained as a seam but is currently
    // unreachable; the deterministic defense against host-escalation
    // vectors moved to the sandbox grant shape (hooks/config exclusion).
    if let ApprovalKind::GitOperation { .. } = &candidate.approval.kind {
        let command = candidate
            .request
            .input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let GitPrefilterVerdict::HumanDirect(reason) = git_prefilter(command) {
            candidate.approval.reason = format!(
                "{}\n\nDetected {} — this command must be approved by a human.",
                candidate.approval.reason, reason
            );
            return ApprovalGate::Human(Box::new(candidate));
        }
    }
    let Some(judge) = tool_state.judge_handle() else {
        return ApprovalGate::Human(Box::new(candidate));
    };
    let prior_user_messages = crate::tools::live_frame_for_session(session_id)
        .map(|frame| prior_user_messages_from_frame(&frame))
        .unwrap_or_default();
    let context = trusted_approval_context(&candidate.approval.kind, &candidate.request.tool_id);
    if judge.start(
        session_id,
        candidate.clone(),
        prior_user_messages,
        context,
        result_tx,
    ) {
        ApprovalGate::Pending
    } else {
        ApprovalGate::Human(Box::new(candidate))
    }
}

fn trusted_approval_context(kind: &ApprovalKind, tool_id: &str) -> JudgeApprovalContext {
    match kind {
        // The judge is asked about what approval actually buys -- the
        // shaped grants -- not about the raw attempts that triggered the
        // request, and no longer about host execution: this retry stays
        // sandboxed (`docs/containment-denial-narrow-grants-design.md`'s
        // 2026-07-26 decision).
        ApprovalKind::FilesystemDenialRetry { grants, .. } => JudgeApprovalContext {
            requested_filesystem_grants: grants.clone(),
            requested_domains: Vec::new(),
            host_execution_requested: false,
            git_metadata_operation: false,
        },
        ApprovalKind::GitOperation { writable_roots } => JudgeApprovalContext {
            requested_filesystem_grants: crate::tools::metadata_grants(writable_roots),
            requested_domains: Vec::new(),
            host_execution_requested: false,
            git_metadata_operation: true,
        },
        ApprovalKind::DomainGrant { domains } | ApprovalKind::DomainDenialRetry { domains, .. } => {
            JudgeApprovalContext {
                requested_filesystem_grants: Vec::new(),
                requested_domains: domains.clone(),
                host_execution_requested: false,
                git_metadata_operation: false,
            }
        }
        ApprovalKind::MachServiceGrant { .. } => JudgeApprovalContext {
            // The mach service grant's "what approval buys" (nono's
            // all-or-nothing security-service group) has no field in this
            // context; the request's reason text carries it, and the
            // structural facts the judge needs are the all-false ones: the
            // retry stays sandboxed, no filesystem grant, no domain grant.
            requested_filesystem_grants: Vec::new(),
            requested_domains: Vec::new(),
            host_execution_requested: false,
            git_metadata_operation: false,
        },
        ApprovalKind::Standard => JudgeApprovalContext {
            requested_filesystem_grants: Vec::new(),
            requested_domains: Vec::new(),
            host_execution_requested: tool_id == "bash",
            git_metadata_operation: false,
        },
    }
}

/// Prior user messages, oldest first, from a session's live frame -- the
/// `LiveState::frame()` read `docs/agent-approval-design.md`'s "Input
/// restriction" bullet pins as already available pre-fold at the seam.
fn prior_user_messages_from_frame(frame: &AgentFrame) -> Vec<String> {
    frame
        .items
        .iter()
        .filter_map(|item| match item {
            AgentFrameItem::Message(Message {
                role: MessageRole::User,
                text,
            }) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// This crate's own built-in tool description, if `tool_id` names one --
/// see [`JudgeInput::tool_description`]'s doc comment for the trust
/// reasoning.
fn builtin_tool_description(tool_id: &str) -> Option<String> {
    crate::tools::definitions()
        .into_iter()
        .find(|definition| definition.id == tool_id)
        .map(|definition| definition.description)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    use crate::decision::{DecisionRequest, DecisionResponse, Question};

    /// A fake [`DecisionClient`] replying from a queue in call order and
    /// recording every request it received, so a test drives the judge
    /// without ever touching the network.
    struct ScriptedClient {
        replies: Mutex<Vec<anyhow::Result<DecisionResponse>>>,
        requests: Mutex<Vec<DecisionRequest>>,
    }

    impl ScriptedClient {
        fn new(replies: Vec<anyhow::Result<DecisionResponse>>) -> Self {
            Self {
                replies: Mutex::new(replies),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<DecisionRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl DecisionClient for ScriptedClient {
        async fn decide(&self, request: DecisionRequest) -> anyhow::Result<DecisionResponse> {
            self.requests.lock().unwrap().push(request);
            let mut replies = self.replies.lock().unwrap();
            if replies.is_empty() {
                anyhow::bail!("ScriptedClient called more times than scripted")
            } else {
                replies.remove(0)
            }
        }
    }

    fn response(answers: Vec<(&str, Answer)>) -> DecisionResponse {
        DecisionResponse {
            model: "clef".to_string(),
            answers: answers
                .into_iter()
                .map(|(name, answer)| (name.to_string(), answer))
                .collect(),
            usage: Default::default(),
        }
    }

    /// A reply whose `ask_human` probability is `noul`.
    fn ask(noul: f64) -> anyhow::Result<DecisionResponse> {
        Ok(response(vec![(request::ASK_HUMAN, Answer::Noul { noul })]))
    }

    struct SlowClient;

    #[async_trait]
    impl DecisionClient for SlowClient {
        async fn decide(&self, _request: DecisionRequest) -> anyhow::Result<DecisionResponse> {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            ask(0.0)
        }
    }

    const DEFAULT_THRESHOLD: f64 = 0.2;

    fn input(call_id: &str) -> JudgeInput {
        JudgeInput {
            identity: crate::contract::ToolCallIdentity {
                call_id: crate::contract::ToolCallId(call_id.into()),
                occurrence_id: crate::contract::OccurrenceId("judge-occ".into()),
            },
            tool_id: "bash".to_string(),
            args: serde_json::json!({ "command": "echo hi" }),
            tool_description: Some("Run a shell command.".to_string()),
            prior_user_messages: vec!["please check the logs".to_string()],
            requested_filesystem_grants: Vec::new(),
            requested_domains: Vec::new(),
            host_execution_requested: false,
            git_metadata_operation: false,
        }
    }

    #[tokio::test]
    async fn the_judge_places_one_ask_human_noul_call() {
        let client = ScriptedClient::new(vec![ask(0.05)]);
        let input = input("call-shape");
        run_decision_judge("clef:cloud", &client, &input, DEFAULT_THRESHOLD).await;

        let requests = client.requests();
        assert_eq!(requests.len(), 1, "there is no second stage");
        let request = &requests[0];
        assert_eq!(request.model, "clef:cloud");
        assert_eq!(request.questions.len(), 1);
        let Question::Noul {
            instructions,
            criteria: Some(criteria),
        } = &request.questions[request::ASK_HUMAN]
        else {
            panic!("ask_human must be a noul question with criteria");
        };
        assert!(criteria.if_true.is_some() && criteria.if_false.is_some());
        assert!(!instructions.contains("echo hi"));
        assert_eq!(request.state["tool_call"]["args"], input.args);
        assert_eq!(
            request.state["user_messages"],
            serde_json::json!(["please check the logs"])
        );
    }

    #[tokio::test]
    async fn the_threshold_is_inclusive_at_the_default() {
        for (noul, expected) in [
            (0.19, JudgeDecision::AutoApprove),
            (0.2, JudgeDecision::AutoApprove),
            (0.21, JudgeDecision::Escalate),
        ] {
            let client = ScriptedClient::new(vec![ask(noul)]);
            let verdict =
                run_decision_judge("clef:cloud", &client, &input("call-t"), DEFAULT_THRESHOLD)
                    .await;
            assert_eq!(verdict.decision, expected, "noul {noul}");
            assert_eq!(verdict.ask_probability, Some(noul));
            assert_eq!(verdict.fallback_reason, None);
            assert_eq!(verdict.fallback_error, None);
            let confidence = verdict.confidence.expect("a real answer has a confidence");
            let expected_confidence = match expected {
                JudgeDecision::AutoApprove => 1.0 - noul,
                JudgeDecision::Escalate => noul,
            };
            assert!(
                (confidence - expected_confidence).abs() < 1e-12,
                "noul {noul}: confidence {confidence}"
            );
        }
    }

    #[tokio::test]
    async fn a_client_error_escalates_with_the_bounded_error_text() {
        let client = ScriptedClient::new(vec![Err(
            anyhow::anyhow!("connection refused").context("decision request failed")
        )]);
        let verdict =
            run_decision_judge("clef:cloud", &client, &input("call-err"), DEFAULT_THRESHOLD).await;
        assert_eq!(verdict.decision, JudgeDecision::Escalate);
        assert_eq!(
            verdict.fallback_reason,
            Some(JudgeFallbackReason::ClientError)
        );
        assert_eq!(
            verdict.fallback_error.as_deref(),
            Some("decision request failed: connection refused")
        );
        assert_eq!(verdict.ask_probability, None);
        assert_eq!(verdict.confidence, None);

        let long = "x".repeat(FALLBACK_ERROR_MAX_CHARS * 2);
        let client = ScriptedClient::new(vec![Err(anyhow::anyhow!(long))]);
        let verdict = run_decision_judge(
            "clef:cloud",
            &client,
            &input("call-err2"),
            DEFAULT_THRESHOLD,
        )
        .await;
        assert_eq!(
            verdict.fallback_error.unwrap().chars().count(),
            FALLBACK_ERROR_MAX_CHARS
        );
    }

    #[tokio::test]
    async fn a_missing_or_mistyped_answer_escalates_as_unparseable() {
        let replies = [
            response(vec![]),
            response(vec![("something_else", Answer::Noul { noul: 0.0 })]),
            response(vec![(request::ASK_HUMAN, Answer::Other)]),
            response(vec![(
                request::ASK_HUMAN,
                Answer::Choice {
                    choice: "false".to_string(),
                    probabilities: Default::default(),
                    confidence: 1.0,
                },
            )]),
        ];
        for reply in replies {
            let client = ScriptedClient::new(vec![Ok(reply.clone())]);
            let verdict = run_decision_judge(
                "clef:cloud",
                &client,
                &input("call-missing"),
                DEFAULT_THRESHOLD,
            )
            .await;
            assert_eq!(verdict.decision, JudgeDecision::Escalate, "{reply:?}");
            assert_eq!(
                verdict.fallback_reason,
                Some(JudgeFallbackReason::Unparseable),
                "{reply:?}"
            );
            assert_eq!(verdict.ask_probability, None);
            assert_eq!(verdict.confidence, None);
        }
    }

    #[test]
    fn fallback_error_is_truncated_on_a_char_boundary() {
        let long = "接続拒否".repeat(FALLBACK_ERROR_MAX_CHARS);
        let bounded = bounded_fallback_error(&long);
        assert_eq!(bounded.chars().count(), FALLBACK_ERROR_MAX_CHARS);
        assert!(long.starts_with(&bounded));

        let exact = "誤".repeat(FALLBACK_ERROR_MAX_CHARS);
        assert_eq!(bounded_fallback_error(&exact), exact);
    }

    #[test]
    fn prior_user_messages_from_frame_reads_only_user_messages_oldest_first() {
        let frame = AgentFrame {
            state: None,
            turn_end_reason: None,
            items: vec![
                AgentFrameItem::Message(Message {
                    role: MessageRole::User,
                    text: "first".to_string(),
                }),
                AgentFrameItem::Message(Message {
                    role: MessageRole::Assistant,
                    text: "assistant reply".to_string(),
                }),
                AgentFrameItem::Message(Message {
                    role: MessageRole::User,
                    text: "second".to_string(),
                }),
            ],
        };

        assert_eq!(
            prior_user_messages_from_frame(&frame),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn builtin_tool_description_finds_a_cataloged_tool_and_none_for_unknown() {
        assert!(builtin_tool_description("bash").is_some());
        assert_eq!(builtin_tool_description("not.a.real.tool"), None);
    }

    #[test]
    fn trusted_context_covers_git_filesystem_and_domain_candidates() {
        let root = std::env::temp_dir().join("judge-git-root");
        let context = trusted_approval_context(
            &ApprovalKind::GitOperation {
                writable_roots: vec![root.clone()],
            },
            "bash",
        );
        assert_eq!(context.requested_domains, Vec::<String>::new());
        assert!(!context.host_execution_requested);
        assert_eq!(context.requested_filesystem_grants.len(), 1);
        assert_eq!(context.requested_filesystem_grants[0].path, root);
        assert_eq!(
            context.requested_filesystem_grants[0].access,
            horizon_sandbox::FilesystemGrantAccess::ReadWrite
        );
        // The judge sees the grant exactly as the sandbox applies it,
        // including the hooks/config write exclusion.
        assert_eq!(
            context.requested_filesystem_grants[0].excluded_subpaths,
            vec![
                std::path::PathBuf::from("hooks"),
                std::path::PathBuf::from("config")
            ]
        );
        assert!(context.git_metadata_operation);

        let denial = horizon_sandbox::FilesystemDenial {
            attempted_path: std::env::temp_dir().join("attempted"),
            grant: horizon_sandbox::FilesystemGrant {
                path: std::env::temp_dir().join("approved"),
                access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
                scope: horizon_sandbox::FilesystemGrantScope::File,
                excluded_subpaths: Vec::new(),
            },
        };
        // The judge is asked about the shaped grant approval would actually
        // add -- deliberately not the per-attempt grant inside `denials`,
        // which is evidence only -- and no longer about host execution: a
        // filesystem retry stays sandboxed.
        let shaped = horizon_sandbox::FilesystemGrant {
            path: std::env::temp_dir(),
            access: horizon_sandbox::FilesystemGrantAccess::ReadWrite,
            scope: horizon_sandbox::FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: Vec::new(),
        };
        let context = trusted_approval_context(
            &ApprovalKind::FilesystemDenialRetry {
                denials: vec![denial.clone()],
                grants: vec![shaped.clone()],
                prior_result: crate::contract::ToolCallResult::new(
                    (crate::contract::ToolCallId("call-context".to_string())).clone(),
                    crate::contract::OccurrenceId(
                        (crate::contract::ToolCallId("call-context".to_string()))
                            .0
                            .clone(),
                    ),
                    serde_json::json!({}),
                ),
            },
            "bash",
        );
        assert_eq!(context.requested_filesystem_grants, vec![shaped]);
        assert_ne!(
            context.requested_filesystem_grants,
            vec![denial.grant.clone()]
        );
        assert!(context.requested_domains.is_empty());
        assert!(!context.host_execution_requested);
        assert!(!context.git_metadata_operation);

        let expected_domains = vec!["example.com".to_string()];
        let context = trusted_approval_context(
            &ApprovalKind::DomainDenialRetry {
                domains: expected_domains.clone(),
                prior_result: crate::contract::ToolCallResult::new(
                    (crate::contract::ToolCallId("call-domain".to_string())).clone(),
                    crate::contract::OccurrenceId(
                        (crate::contract::ToolCallId("call-domain".to_string()))
                            .0
                            .clone(),
                    ),
                    serde_json::json!({}),
                ),
            },
            "bash",
        );
        assert!(context.requested_filesystem_grants.is_empty());
        assert_eq!(context.requested_domains, expected_domains);
        assert!(!context.host_execution_requested);
        assert!(!context.git_metadata_operation);

        let context = trusted_approval_context(&ApprovalKind::Standard, "bash");
        assert!(context.host_execution_requested);
        let context = trusted_approval_context(&ApprovalKind::Standard, "web_fetch");
        assert!(!context.host_execution_requested);
    }

    fn temp_event_log(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-agent-judge-seam-{label}-{}.jsonl",
            uuid::Uuid::new_v4()
        ))
    }

    fn candidate(kind: ApprovalKind) -> ApprovalCandidate {
        let request = ToolCallRequest {
            call_id: (crate::contract::ToolCallId("call-1".to_string())).clone(),
            tool_id: "mock.approval_required".to_string(),
            input: serde_json::json!({}).into(),
            occurrence_id: crate::contract::OccurrenceId(
                (crate::contract::ToolCallId("call-1".to_string()))
                    .0
                    .clone(),
            ),
        };
        ApprovalCandidate {
            approval: ApprovalRequest {
                call_id: request.call_id.clone(),
                reason: "test approval".to_string(),
                kind,
                occurrence_id: crate::contract::OccurrenceId(request.call_id.0.clone()),
            },
            request,
        }
    }

    #[test]
    fn enforcing_gate_returns_pending_then_delivers_auto_approval() {
        let path = temp_event_log("auto");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let client: Arc<dyn DecisionClient> = Arc::new(ScriptedClient::new(vec![ask(0.05)]));
        let judge = JudgeHandle::for_test_decision("test-judge-model", client, writer);
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let session_id = SessionId::new();
        let expected = candidate(ApprovalKind::Standard);
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tool_state, session_id, expected.clone(), tx,),
            ApprovalGate::Pending
        );
        let completion = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("judge completion");
        let crate::tools::ToolCompletion::ApprovalJudged(judgment) = completion else {
            panic!("unexpected completion");
        };
        assert_eq!(
            judgment,
            ApprovalJudgment {
                candidate: expected,
                decision: JudgeDecision::AutoApprove,
            }
        );
        let _ = std::fs::remove_file(path);
    }

    /// A chat client that answers every stage-1 call with a clean `N`.
    struct ApprovingChatClient;

    #[async_trait]
    impl ModelClient for ApprovingChatClient {
        async fn complete(
            &self,
            _model: &str,
            _request: client::RawCompletionRequest,
        ) -> anyhow::Result<client::RawCompletionResponse> {
            Ok(client::RawCompletionResponse {
                text: "N".to_string(),
                logprobs: Some(
                    serde_json::json!({ "content": [{ "token": "N", "logprob": 0.0 }] }),
                ),
            })
        }
    }

    #[test]
    fn the_chat_backend_gate_delivers_a_stage1_auto_approval_and_records_it() {
        let path = temp_event_log("chat-auto");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let judge =
            JudgeHandle::for_test_chat("small-chat", Arc::new(ApprovingChatClient), writer.clone());
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(
                &tool_state,
                SessionId::new(),
                candidate(ApprovalKind::Standard),
                tx,
            ),
            ApprovalGate::Pending
        );
        let crate::tools::ToolCompletion::ApprovalJudged(judgment) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("judge completion")
        else {
            panic!("unexpected completion");
        };
        assert_eq!(judgment.decision, JudgeDecision::AutoApprove);
        writer.flush().expect("flush chat audit");
        let report = crate::persistence::event_log::read(&path).expect("read chat audit");
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("chat payload");
        assert_eq!(payload["judge_backend"], "chat");
        assert_eq!(payload["judge_model"], "small-chat");
        assert_eq!(payload["judge_stage"], 1);
        assert_eq!(payload["judge_threshold"], serde_json::Value::Null);
        assert_eq!(payload["judge_ask_probability"], serde_json::Value::Null);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn enforcing_gate_delivers_escalation_on_model_error() {
        let path = temp_event_log("error");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let client: Arc<dyn DecisionClient> = Arc::new(ScriptedClient::new(vec![Err(
            anyhow::anyhow!("connection refused"),
        )]));
        let judge = JudgeHandle::for_test_decision("test-judge-model", client, writer);
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(
                &tool_state,
                SessionId::new(),
                candidate(ApprovalKind::Standard),
                tx,
            ),
            ApprovalGate::Pending
        );
        let crate::tools::ToolCompletion::ApprovalJudged(judgment) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("judge completion")
        else {
            panic!("unexpected completion");
        };
        assert_eq!(judgment.decision, JudgeDecision::Escalate);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_judge_preserves_the_human_candidate_immediately() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let expected = candidate(ApprovalKind::Standard);
        let (tx, _rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tool_state, SessionId::new(), expected.clone(), tx,),
            ApprovalGate::Human(Box::new(expected))
        );
    }

    #[test]
    fn timeout_delivers_fail_closed_escalation() {
        let path = temp_event_log("timeout");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let judge = JudgeHandle::for_test_with_limits(
            "test-judge-model",
            JudgeBackend::Decision {
                client: Arc::new(SlowClient),
                threshold: DEFAULT_THRESHOLD,
            },
            writer.clone(),
            std::time::Duration::from_millis(5),
            10.0,
            5.0,
        );
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(
                &tool_state,
                SessionId::new(),
                candidate(ApprovalKind::DomainGrant {
                    domains: vec!["example.com".to_string()],
                }),
                tx,
            ),
            ApprovalGate::Pending
        );
        let crate::tools::ToolCompletion::ApprovalJudged(judgment) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("timeout completion")
        else {
            panic!("unexpected completion");
        };
        assert_eq!(judgment.decision, JudgeDecision::Escalate);
        writer.flush().expect("flush timeout audit");
        let report = crate::persistence::event_log::read(&path).expect("read timeout audit");
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("timeout payload");
        assert_eq!(payload["fallback_reason"], "timeout");
        assert_eq!(payload["judge_decision"], "escalate");
        assert_eq!(payload["judge_ask_probability"], serde_json::Value::Null);
        assert_eq!(payload["judge_confidence"], serde_json::Value::Null);
        assert_eq!(payload["judge_threshold"], DEFAULT_THRESHOLD);
        assert_eq!(payload["judge_backend"], "decision");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rate_limit_exhaustion_preserves_human_flow_immediately() {
        let path = temp_event_log("rate");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let judge = JudgeHandle::for_test_with_limits(
            "test-judge-model",
            JudgeBackend::Decision {
                client: Arc::new(SlowClient),
                threshold: DEFAULT_THRESHOLD,
            },
            writer.clone(),
            std::time::Duration::from_secs(1),
            0.0,
            0.0,
        );
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let expected = candidate(ApprovalKind::FilesystemDenialRetry {
            denials: Vec::new(),
            grants: Vec::new(),
            prior_result: crate::contract::ToolCallResult::new(
                (crate::contract::ToolCallId("call-1".to_string())).clone(),
                crate::contract::OccurrenceId(
                    (crate::contract::ToolCallId("call-1".to_string()))
                        .0
                        .clone(),
                ),
                serde_json::json!({}),
            ),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tool_state, SessionId::new(), expected.clone(), tx,),
            ApprovalGate::Human(Box::new(expected))
        );
        assert!(rx.try_recv().is_err());
        writer.flush().expect("flush rate-limit audit");
        let report = crate::persistence::event_log::read(&path).expect("read rate-limit audit");
        let payload = report.records[0]
            .provider_payload
            .as_ref()
            .expect("rate-limit payload");
        assert_eq!(payload["fallback_reason"], "rate_limited");
        assert_eq!(payload["judge_decision"], "escalate");
        let _ = std::fs::remove_file(path);
    }

    fn git_candidate(command: &str) -> ApprovalCandidate {
        let request = ToolCallRequest {
            call_id: (crate::contract::ToolCallId("call-1".to_string())).clone(),
            tool_id: "bash".to_string(),
            input: serde_json::json!({ "command": command }).into(),
            occurrence_id: crate::contract::OccurrenceId(
                (crate::contract::ToolCallId("call-1".to_string()))
                    .0
                    .clone(),
            ),
        };
        ApprovalCandidate {
            approval: ApprovalRequest {
                call_id: request.call_id.clone(),
                reason: "test git approval".to_string(),
                kind: ApprovalKind::GitOperation {
                    writable_roots: vec![std::env::temp_dir()],
                },
                occurrence_id: crate::contract::OccurrenceId(request.call_id.0.clone()),
            },
            request,
        }
    }

    #[test]
    fn git_prefilter_passes_dangerous_commands_to_the_judge() {
        // Board #38: the prefilter no longer
        // blocks dangerous constructs. A command carrying `-c` now reaches
        // the judge (Pending) instead of going straight to the human.
        let path = temp_event_log("git-dangerous");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let client: Arc<dyn DecisionClient> = Arc::new(ScriptedClient::new(vec![ask(0.05)]));
        let judge = JudgeHandle::for_test_decision("test-judge-model", client, writer);
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let candidate = git_candidate("git -c core.hooksPath=/dev/null commit -m test");
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tool_state, SessionId::new(), candidate, tx),
            ApprovalGate::Pending
        );
        let crate::tools::ToolCompletion::ApprovalJudged(judgment) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("judge completion")
        else {
            panic!("unexpected completion");
        };
        assert_eq!(judgment.decision, JudgeDecision::AutoApprove);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn git_prefilter_passes_plain_commands_to_the_judge() {
        let path = temp_event_log("git-plain");
        let (writer, _init_rx) = crate::persistence::event_log::WriterHandle::open(&path);
        let client: Arc<dyn DecisionClient> = Arc::new(ScriptedClient::new(vec![ask(0.05)]));
        let judge = JudgeHandle::for_test_decision("test-judge-model", client, writer);
        let tool_state = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let candidate = git_candidate("git commit -m test");
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tool_state, SessionId::new(), candidate.clone(), tx),
            ApprovalGate::Pending
        );
        let crate::tools::ToolCompletion::ApprovalJudged(judgment) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("judge completion")
        else {
            panic!("unexpected completion");
        };
        assert_eq!(judgment.decision, JudgeDecision::AutoApprove);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn git_operation_without_judge_falls_back_to_human() {
        let tool_state = ToolSessionState::new(std::env::temp_dir());
        let candidate = git_candidate("git commit -m test");
        let (tx, _rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tool_state, SessionId::new(), candidate.clone(), tx),
            ApprovalGate::Human(Box::new(candidate))
        );
    }
    struct PendingClient {
        started: crossbeam_channel::Sender<()>,
        dropped: crossbeam_channel::Sender<()>,
    }
    #[async_trait]
    impl DecisionClient for PendingClient {
        async fn decide(&self, _request: DecisionRequest) -> anyhow::Result<DecisionResponse> {
            struct OnDrop(crossbeam_channel::Sender<()>);
            impl Drop for OnDrop {
                fn drop(&mut self) {
                    let _ = self.0.send(());
                }
            }
            let _guard = OnDrop(self.dropped.clone());
            let _ = self.started.send(());
            std::future::pending().await
        }
    }

    #[test]
    fn cancelled_judgments_drop_the_request_without_a_verdict_or_completion() {
        use crate::tools::background::{self, SessionOwner};
        use std::time::Duration;
        for session_shutdown in [false, true] {
            let path = temp_event_log("cancel-judge");
            let (writer, _) = crate::persistence::event_log::WriterHandle::open(&path);
            let (started_tx, started) = crossbeam_channel::unbounded();
            let (dropped_tx, dropped) = crossbeam_channel::unbounded();
            let judge = JudgeHandle::for_test_decision(
                "test",
                Arc::new(PendingClient {
                    started: started_tx,
                    dropped: dropped_tx,
                }),
                writer.clone(),
            );
            let tools = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
                .with_judge(Some(judge))
                .build();
            let session = SessionId::new();
            let owner = SessionOwner::new(session);
            let candidate = candidate(ApprovalKind::Standard);
            let call = candidate.request.identity();
            let (tx, rx) = crossbeam_channel::unbounded();
            assert_eq!(
                start_approval_gate(&tools, session, candidate, tx),
                ApprovalGate::Pending
            );
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            if session_shutdown {
                background::close_session(session);
            } else {
                crate::tools::cancel_tool_execution(session, &call);
            }
            dropped
                .recv_timeout(Duration::from_secs(2))
                .expect("in-flight request must be dropped");
            assert!(background::drain_session_work(
                session,
                Duration::from_secs(2)
            ));
            assert!(rx.try_recv().is_err());
            writer.flush().unwrap();
            assert!(crate::persistence::event_log::read(&path)
                .unwrap()
                .records
                .is_empty());
            drop(owner);
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn a_replaced_judgment_is_cancelled_without_stopping_its_replacement() {
        use std::time::Duration;
        let path = temp_event_log("replace-judge");
        let (writer, _) = crate::persistence::event_log::WriterHandle::open(&path);
        let (started_tx, started) = crossbeam_channel::unbounded();
        let (dropped_tx, dropped) = crossbeam_channel::unbounded();
        let judge = JudgeHandle::for_test_decision(
            "test",
            Arc::new(PendingClient {
                started: started_tx,
                dropped: dropped_tx,
            }),
            writer.clone(),
        );
        let tools = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let session = SessionId::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let first = candidate(ApprovalKind::Standard);
        let mut second = first.clone();
        second.request.occurrence_id = crate::contract::OccurrenceId::new();
        second.approval.occurrence_id = second.request.occurrence_id.clone();
        assert_eq!(
            start_approval_gate(&tools, session, first, tx.clone()),
            ApprovalGate::Pending
        );
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(
            start_approval_gate(&tools, session, second.clone(), tx),
            ApprovalGate::Pending
        );
        dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            dropped.try_recv().is_err(),
            "replacement still owns its request"
        );
        crate::tools::cancel_tool_execution(session, &second.request.identity());
        dropped.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(crate::tools::drain_session_work(
            session,
            Duration::from_secs(2)
        ));
        assert!(rx.try_recv().is_err());
        let _ = std::fs::remove_file(path);
    }
    struct PanickingClient;
    #[async_trait]
    impl DecisionClient for PanickingClient {
        async fn decide(&self, _: DecisionRequest) -> anyhow::Result<DecisionResponse> {
            panic!("judge transport panicked");
        }
    }
    #[test]
    fn a_panicking_judge_retires_its_work_and_escalates_to_the_human() {
        let path = temp_event_log("panic-judge");
        let (writer, _) = crate::persistence::event_log::WriterHandle::open(&path);
        let judge = JudgeHandle::for_test_decision("test", Arc::new(PanickingClient), writer);
        let tools = crate::tools::ToolSessionBuilder::new(std::env::temp_dir())
            .with_judge(Some(judge))
            .build();
        let session = SessionId::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        assert_eq!(
            start_approval_gate(&tools, session, candidate(ApprovalKind::Standard), tx),
            ApprovalGate::Pending
        );
        let result = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(matches!(
            result,
            crate::tools::ToolCompletion::ApprovalJudged(ApprovalJudgment {
                decision: JudgeDecision::Escalate,
                ..
            })
        ));
        assert!(crate::tools::drain_session_work(
            session,
            std::time::Duration::from_secs(2)
        ));
        let _ = std::fs::remove_file(path);
    }
}
