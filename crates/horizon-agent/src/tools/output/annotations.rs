//! Execution and approval evidence is assembled before serializing a result.
use super::Response;
pub(crate) fn annotate_auto_approval(output: &mut Response, tier: &str, reason: &str) {
    output.evidence_mut().auto_approved = Some(true);
    output.evidence_mut().policy_tier = Some(tier.into());
    output.evidence_mut().policy_reason = Some(reason.into());
}
pub(crate) fn annotate_sandboxed(output: &mut Response, sandboxed: bool) {
    output.evidence_mut().sandboxed = Some(sandboxed);
}
pub(crate) fn annotate_refused_hosts(output: &mut Response, hosts: Vec<String>) {
    output.evidence_mut().refused_hosts = hosts;
}
pub(crate) fn annotate_domain_approval(output: &mut Response, domains: &[String]) {
    output.evidence_mut().domain_approved = Some(true);
    output.evidence_mut().approved_domains = Some(domains.to_vec());
}
pub(crate) fn annotate_host_execution_approval(output: &mut Response, source: &str) {
    output.evidence_mut().host_execution_approved = Some(true);
    output.evidence_mut().approval_scope = Some("host_execution_once".into());
    output.evidence_mut().approval_source = Some(source.into());
}
/// Who decided the approval this result ran under. One place, because the
/// question is the same for every kind: an approval whose result cannot
/// say who gave it is not auditable after the fact.
pub(crate) fn annotate_approval_source(output: &mut Response, source: &str) {
    output.evidence_mut().approval_source = Some(source.into());
}
