mod fetch;
mod search;
mod ssrf;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, OnceLock};

use crate::tools::output::Response;
use crossbeam_channel::Sender;
use futures_util::FutureExt;
use horizon_sandbox_proxy::Allowlist;
use reqwest::Url;

use crate::contract::{OccurrenceId, SessionId, ToolCallId, ToolCallIdentity};
use crate::tools::output::error as error_output;
use crate::tools::output::{annotate_auto_approval, annotate_domain_approval};
use crate::tools::state::ToolSessionState;
use crate::tools::ToolCompletion;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FetchGate {
    Invalid,
    Allowed { domain: String },
    NeedsApproval { domain: String },
}

#[derive(Clone, Debug)]
pub(crate) enum WebApprovalOrigin {
    Auto,
    ManualDomainGrant { domains: Vec<String> },
}

pub(crate) fn fetch_gate(
    tool_state: &ToolSessionState,
    input: &crate::tools::input::WebFetch,
) -> FetchGate {
    match fetch::domain_from_input(input) {
        Ok(domain) if tool_state.is_domain_allowed(&domain) => FetchGate::Allowed { domain },
        Ok(domain) => FetchGate::NeedsApproval { domain },
        Err(_) => FetchGate::Invalid,
    }
}

pub(crate) fn domain_grant_from_input(input: &crate::tools::input::WebFetch) -> Option<String> {
    fetch::domain_from_input(input).ok()
}

pub(crate) fn validate_domain_grant(domain: &str) -> Result<String, String> {
    let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
    if normalized.contains('/') || normalized.contains('@') {
        return Err("domain grant is not a canonical host".to_string());
    }
    let authority = if normalized.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{normalized}]")
    } else {
        if normalized.contains(':') {
            return Err("domain grant is not a canonical host".to_string());
        }
        normalized.clone()
    };
    let url = Url::parse(&format!("https://{authority}/"))
        .map_err(|_| "domain grant is not a valid host".to_string())?;
    let canonical = ssrf::validate_url(&url)?;
    if canonical != normalized {
        return Err("domain grant is not canonical".to_string());
    }
    Ok(canonical)
}

pub(crate) fn spawn(
    session_id: SessionId,
    request: &super::input::PreparedCall<'_>,
    domains: Arc<Allowlist>,
    origin: WebApprovalOrigin,
    result_tx: Sender<ToolCompletion>,
) {
    let identity = request.identity();
    let input = request.input.clone();
    let registration = super::background::Registration::new(
        session_id,
        super::background::Lifetime::Call(identity.clone()),
        super::background::WorkKind::Tool,
    );
    let token = registration.token();
    let tool_id = request.tool_id.clone();
    web_runtime().spawn(async move {
        let work = AssertUnwindSafe(run(
            identity.clone(),
            &tool_id,
            input,
            domains,
            &origin,
        ))
        .catch_unwind();
        let completion = tokio::select! {
            biased;
            _ = token.cancelled() => None,
            result = work => Some(match result {
                Ok(completion) => completion,
                Err(payload) => ToolCompletion::Finished(identity.finish(
                    error_output(format!("{tool_id} worker panicked: {}", panic_message(&*payload))),
                )),
            }),
        };
        let was_current = registration.finish();
        if was_current {
            if let Some(completion) = completion {
                if matches!(completion, ToolCompletion::Finished(_)) {
                    clear_approved_domains(session_id, &identity);
                }
                let _ = result_tx.send(completion);
            }
        }
    });
}

async fn run(
    identity: crate::contract::ToolCallIdentity,
    tool_id: &str,
    input: super::input::ToolInput,
    domains: Arc<Allowlist>,
    origin: &WebApprovalOrigin,
) -> ToolCompletion {
    let outcome = match input {
        super::input::ToolInput::WebSearch(input) => {
            WebOutcome::Finished(search::execute(input).await)
        }
        super::input::ToolInput::WebFetch(input) => match fetch::execute(input, domains).await {
            fetch::FetchOutcome::Finished(output) => WebOutcome::Finished(output),
            fetch::FetchOutcome::DomainGrantRequired(domains) => {
                WebOutcome::DomainGrantRequired(domains)
            }
        },
        _ => WebOutcome::Finished(error_output(format!(
            "unknown asynchronous web tool `{tool_id}`"
        ))),
    };
    with_identity(identity, outcome, tool_id, origin)
}

enum WebOutcome {
    Finished(Response),
    DomainGrantRequired(Vec<String>),
}

fn with_identity(
    identity: crate::contract::ToolCallIdentity,
    outcome: WebOutcome,
    tool_id: &str,
    origin: &WebApprovalOrigin,
) -> ToolCompletion {
    match outcome {
        WebOutcome::Finished(mut output) => {
            match origin {
                WebApprovalOrigin::Auto => annotate_auto_approval(
                    &mut output,
                    "boundary_crossing",
                    if tool_id == "web_search" {
                        "fixed Exa search endpoint"
                    } else {
                        "session host was already approved"
                    },
                ),
                WebApprovalOrigin::ManualDomainGrant { domains } => {
                    annotate_domain_approval(&mut output, domains)
                }
            }
            ToolCompletion::Finished(identity.finish(output))
        }
        WebOutcome::DomainGrantRequired(domains) => ToolCompletion::DomainGrantRequired {
            call_id: identity.call_id,
            occurrence_id: identity.occurrence_id,
            domains,
        },
    }
}

pub(crate) fn clear_session_approvals(session_id: SessionId) {
    approved_domains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(registered_session, _, _), _| *registered_session != session_id);
}

pub(crate) fn record_approved_domains(
    session_id: SessionId,
    identity: &ToolCallIdentity,
    newly_approved: &[String],
) -> Vec<String> {
    let mut approvals = approved_domains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let accumulated = approvals
        .entry((
            session_id,
            identity.call_id.clone(),
            identity.occurrence_id.clone(),
        ))
        .or_default();
    for domain in newly_approved {
        if !accumulated.contains(domain) {
            accumulated.push(domain.clone());
        }
    }
    accumulated.clone()
}

pub(crate) fn clear_approved_domains(session_id: SessionId, identity: &ToolCallIdentity) {
    approved_domains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(
            session_id,
            identity.call_id.clone(),
            identity.occurrence_id.clone(),
        ));
}

type TaskKey = (SessionId, ToolCallId, OccurrenceId);
type ApprovedDomainMap = HashMap<TaskKey, Vec<String>>;

fn approved_domains() -> &'static Mutex<ApprovedDomainMap> {
    static APPROVED_DOMAINS: OnceLock<Mutex<ApprovedDomainMap>> = OnceLock::new();
    APPROVED_DOMAINS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn web_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("horizon-agent-web")
            .build()
            .expect("web runtime")
    })
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ToolCallRequest;

    #[test]
    fn spawned_web_work_keeps_its_dispatch_occurrence_in_results_and_grant_requests() {
        for url in ["not a URL", "https://example.com/"] {
            let tools = ToolSessionState::without_root();
            let request = ToolCallRequest {
                call_id: ToolCallId("web-attempt".into()),
                occurrence_id: crate::contract::OccurrenceId::new(),
                tool_id: "web_fetch".into(),
                input: serde_json::json!({"url": url}).into(),
            };
            let (tx, rx) = crossbeam_channel::unbounded();
            spawn(
                SessionId::new(),
                &super::super::input::PreparedCall::new(&request).unwrap(),
                tools.domain_allowlist(),
                WebApprovalOrigin::Auto,
                tx,
            );
            let completion = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            match completion {
                ToolCompletion::Finished(result) if url == "not a URL" => {
                    assert!(result.is_error());
                    assert_eq!(result.occurrence_id, request.occurrence_id);
                }
                ToolCompletion::DomainGrantRequired { occurrence_id, .. } if url != "not a URL" => {
                    assert_eq!(occurrence_id, request.occurrence_id);
                }
                other => panic!("unexpected completion: {other:?}"),
            }
        }
    }

    #[test]
    fn domain_grants_are_canonical_hosts_not_urls_or_credentials() {
        assert_eq!(
            validate_domain_grant("Example.COM.").unwrap(),
            "example.com"
        );
        assert_eq!(
            validate_domain_grant("2606:4700:4700::1111").unwrap(),
            "2606:4700:4700::1111"
        );
        for invalid in [
            "https://example.com",
            "user@example.com",
            "example.com:443",
            "localhost",
            "127.0.0.1",
            "::1",
        ] {
            assert!(validate_domain_grant(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn human_domain_grants_accumulate_for_one_call_and_clear_on_cancel() {
        let session_id = SessionId::new();
        let call_id = ToolCallId("redirecting-fetch".to_string());
        let identity = crate::test_support::tool_identity(&call_id);
        assert_eq!(
            record_approved_domains(session_id, &identity, &["first.example".to_string()]),
            vec!["first.example"]
        );
        assert_eq!(
            record_approved_domains(
                session_id,
                &identity,
                &["first.example".to_string(), "second.example".to_string()]
            ),
            vec!["first.example", "second.example"]
        );
        super::super::cancel_tool_execution(session_id, &identity);
        assert_eq!(
            record_approved_domains(session_id, &identity, &["third.example".to_string()]),
            vec!["third.example"]
        );
        clear_approved_domains(session_id, &identity);
    }
    #[test]
    fn cancelling_an_old_occurrence_keeps_the_new_occurrences_domain_annotations() {
        let session = SessionId::new();
        let old = crate::test_support::tool_identity(&ToolCallId("reused".into()));
        let new = ToolCallIdentity {
            occurrence_id: OccurrenceId::new(),
            ..old.clone()
        };
        record_approved_domains(session, &old, &["old.example".into()]);
        record_approved_domains(session, &new, &["new.example".into()]);
        super::super::cancel_tool_execution(session, &old);
        assert_eq!(
            record_approved_domains(session, &new, &[]),
            vec!["new.example"]
        );
        clear_session_approvals(session);
    }
}
