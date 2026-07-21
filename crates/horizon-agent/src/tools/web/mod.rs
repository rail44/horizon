mod fetch;
mod search;
mod ssrf;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crossbeam_channel::Sender;
use futures_util::FutureExt;
use horizon_sandbox_proxy::Allowlist;
use reqwest::Url;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::contract::{SessionId, ToolCallId, ToolCallResult};
use crate::policy::{annotate_auto_approval, annotate_domain_approval};
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

pub(crate) fn fetch_gate(tool_state: &ToolSessionState, input: &Value) -> FetchGate {
    match fetch::domain_from_input(input) {
        Ok(domain) if tool_state.is_domain_allowed(&domain) => FetchGate::Allowed { domain },
        Ok(domain) => FetchGate::NeedsApproval { domain },
        Err(_) => FetchGate::Invalid,
    }
}

pub(crate) fn domain_grant_from_input(input: &Value) -> Option<String> {
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
    call_id: ToolCallId,
    tool_id: &str,
    input: Value,
    domains: Arc<Allowlist>,
    origin: WebApprovalOrigin,
    result_tx: Sender<ToolCompletion>,
) {
    let token = CancellationToken::new();
    let generation = next_generation();
    if let Some(replaced) = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            (session_id, call_id.clone()),
            RegisteredTask {
                generation,
                token: token.clone(),
            },
        )
    {
        replaced.token.cancel();
    }
    let tool_id = tool_id.to_string();
    web_runtime().spawn(async move {
        let work = AssertUnwindSafe(run(
            call_id.clone(),
            &tool_id,
            input,
            domains,
            &origin,
        ))
        .catch_unwind();
        let completion = tokio::select! {
            _ = token.cancelled() => None,
            result = work => Some(match result {
                Ok(completion) => completion,
                Err(payload) => ToolCompletion::Finished(ToolCallResult::new(
                    call_id.clone(),
                    json!({
                        "is_error": true,
                        "message": format!("{tool_id} worker panicked: {}", panic_message(&*payload)),
                    }),
                )),
            }),
        };
        let was_current = finish_registration(session_id, &call_id, generation);
        if was_current {
            if let Some(completion) = completion {
                if matches!(completion, ToolCompletion::Finished(_)) {
                    clear_approved_domains(session_id, &call_id);
                }
                let _ = result_tx.send(completion);
            }
        }
    });
}

async fn run(
    call_id: ToolCallId,
    tool_id: &str,
    input: Value,
    domains: Arc<Allowlist>,
    origin: &WebApprovalOrigin,
) -> ToolCompletion {
    let outcome = match tool_id {
        "web_search" => WebOutcome::Finished(search::execute(input).await),
        "web_fetch" => match fetch::execute(input, domains).await {
            fetch::FetchOutcome::Finished(output) => WebOutcome::Finished(output),
            fetch::FetchOutcome::DomainGrantRequired(domains) => {
                WebOutcome::DomainGrantRequired(domains)
            }
        },
        _ => WebOutcome::Finished(json!({
            "is_error": true,
            "message": format!("unknown asynchronous web tool `{tool_id}`"),
        })),
    };
    with_call_id(call_id, outcome, tool_id, origin)
}

enum WebOutcome {
    Finished(Value),
    DomainGrantRequired(Vec<String>),
}

fn with_call_id(
    call_id: ToolCallId,
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
            ToolCompletion::Finished(ToolCallResult::new(call_id, output))
        }
        WebOutcome::DomainGrantRequired(domains) => {
            ToolCompletion::DomainGrantRequired { call_id, domains }
        }
    }
}

pub(crate) fn cancel_if_running(session_id: SessionId, call_id: &ToolCallId) {
    if let Some(task) = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(session_id, call_id.clone()))
    {
        task.token.cancel();
    }
    clear_approved_domains(session_id, call_id);
}

pub(crate) fn cancel_session(session_id: SessionId) {
    let mut tasks = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let keys = tasks
        .keys()
        .filter(|(registered_session, _)| *registered_session == session_id)
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        if let Some(task) = tasks.remove(&key) {
            task.token.cancel();
        }
    }
    approved_domains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(registered_session, _), _| *registered_session != session_id);
}

pub(crate) fn record_approved_domains(
    session_id: SessionId,
    call_id: &ToolCallId,
    newly_approved: &[String],
) -> Vec<String> {
    let mut approvals = approved_domains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let accumulated = approvals.entry((session_id, call_id.clone())).or_default();
    for domain in newly_approved {
        if !accumulated.contains(domain) {
            accumulated.push(domain.clone());
        }
    }
    accumulated.clone()
}

pub(crate) fn clear_approved_domains(session_id: SessionId, call_id: &ToolCallId) {
    approved_domains()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&(session_id, call_id.clone()));
}

type TaskKey = (SessionId, ToolCallId);
type ApprovedDomainMap = HashMap<TaskKey, Vec<String>>;

fn approved_domains() -> &'static Mutex<ApprovedDomainMap> {
    static APPROVED_DOMAINS: OnceLock<Mutex<ApprovedDomainMap>> = OnceLock::new();
    APPROVED_DOMAINS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct RegisteredTask {
    generation: u64,
    token: CancellationToken,
}

fn registry() -> &'static Mutex<HashMap<TaskKey, RegisteredTask>> {
    static REGISTRY: OnceLock<Mutex<HashMap<TaskKey, RegisteredTask>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_generation() -> u64 {
    static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn finish_registration(session_id: SessionId, call_id: &ToolCallId, generation: u64) -> bool {
    let key = (session_id, call_id.clone());
    let mut tasks = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if tasks
        .get(&key)
        .is_some_and(|registered| registered.generation == generation)
    {
        tasks.remove(&key);
        true
    } else {
        false
    }
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
    fn cancellation_is_session_scoped_and_old_tasks_cannot_remove_replacements() {
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let call_id = ToolCallId("same-provider-call-id".to_string());
        let token_a = CancellationToken::new();
        let token_b = CancellationToken::new();
        let generation_a = next_generation();
        let generation_b = next_generation();
        {
            let mut tasks = registry().lock().unwrap();
            tasks.insert(
                (session_a, call_id.clone()),
                RegisteredTask {
                    generation: generation_a,
                    token: token_a.clone(),
                },
            );
            tasks.insert(
                (session_b, call_id.clone()),
                RegisteredTask {
                    generation: generation_b,
                    token: token_b.clone(),
                },
            );
        }

        cancel_if_running(session_a, &call_id);
        assert!(token_a.is_cancelled());
        assert!(!token_b.is_cancelled());

        let replacement = CancellationToken::new();
        let replacement_generation = next_generation();
        registry().lock().unwrap().insert(
            (session_b, call_id.clone()),
            RegisteredTask {
                generation: replacement_generation,
                token: replacement.clone(),
            },
        );
        assert!(!finish_registration(session_b, &call_id, generation_b));
        assert!(registry()
            .lock()
            .unwrap()
            .contains_key(&(session_b, call_id.clone())));

        assert!(finish_registration(
            session_b,
            &call_id,
            replacement_generation
        ));
        registry().lock().unwrap().insert(
            (session_b, call_id.clone()),
            RegisteredTask {
                generation: next_generation(),
                token: replacement.clone(),
            },
        );
        cancel_session(session_b);
        assert!(replacement.is_cancelled());
        assert!(!registry()
            .lock()
            .unwrap()
            .contains_key(&(session_b, call_id)));
    }

    #[test]
    fn human_domain_grants_accumulate_for_one_call_and_clear_on_cancel() {
        let session_id = SessionId::new();
        let call_id = ToolCallId("redirecting-fetch".to_string());
        assert_eq!(
            record_approved_domains(session_id, &call_id, &["first.example".to_string()]),
            vec!["first.example"]
        );
        assert_eq!(
            record_approved_domains(
                session_id,
                &call_id,
                &["first.example".to_string(), "second.example".to_string()]
            ),
            vec!["first.example", "second.example"]
        );
        cancel_if_running(session_id, &call_id);
        assert_eq!(
            record_approved_domains(session_id, &call_id, &["third.example".to_string()]),
            vec!["third.example"]
        );
        clear_approved_domains(session_id, &call_id);
    }
}
