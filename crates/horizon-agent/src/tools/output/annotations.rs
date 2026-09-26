//! Execution and approval evidence is assembled before serializing a result.
use super::{FilesystemGrant, NetworkDenial, Response, UngrantablePath};
use std::path::PathBuf;
pub(crate) fn annotate_auto_approval(output: &mut Response, tier: &str, reason: &str) {
    output.evidence_mut().auto_approved = Some(true);
    output.evidence_mut().policy_tier = Some(tier.into());
    output.evidence_mut().policy_reason = Some(reason.into());
}
pub(crate) fn annotate_sandboxed(output: &mut Response, sandboxed: bool) {
    output.evidence_mut().sandboxed = Some(sandboxed);
}
pub(crate) fn annotate_denied_domains(output: &mut Response, domains: &[String]) {
    output.evidence_mut().denied_domains = Some(domains.to_vec());
    output.mark_failed();
}
pub(crate) fn annotate_domain_approval(output: &mut Response, domains: &[String]) {
    output.evidence_mut().domain_approved = Some(true);
    output.evidence_mut().approved_domains = Some(domains.to_vec());
}
pub(crate) fn annotate_filesystem_denials(
    output: &mut Response,
    denials: &[horizon_sandbox::FilesystemDenial],
) {
    output.evidence_mut().denied_filesystem_paths = Some(
        denials
            .iter()
            .map(|d| d.attempted_path.display().to_string())
            .collect(),
    );
    output.mark_failed();
}
pub(crate) fn annotate_ungrantable_denials(
    output: &mut Response,
    denials: &[horizon_sandbox::UngrantableDenial],
) {
    output.evidence_mut().ungrantable_filesystem_paths = denials
        .iter()
        .map(|d| UngrantablePath {
            path: d.attempted_path.display().to_string(),
            guidance: d.guidance.clone(),
        })
        .collect();
}
pub(crate) fn annotate_network_denials(
    output: &mut Response,
    denials: &[horizon_sandbox::NetworkDenial],
) {
    output.evidence_mut().denied_network_routes = denials
        .iter()
        .map(|d| NetworkDenial {
            target: d.target.clone(),
            operation: d.operation.clone(),
            reason: d.reason.clone(),
        })
        .collect();
}
#[cfg(target_os = "macos")]
pub(crate) fn annotate_denied_mach_services(output: &mut Response, services: &[String]) {
    output.evidence_mut().denied_mach_services = Some(services.to_vec());
    output.mark_failed();
}
pub(crate) fn annotate_mach_service_grant_approval(output: &mut Response, services: &[String]) {
    output.evidence_mut().mach_service_grant_approved = Some(true);
    output.evidence_mut().approved_mach_services = Some(services.to_vec());
}
#[cfg(target_os = "macos")]
pub(crate) fn annotate_denial_collection_unavailable(output: &mut Response, error: &str) {
    output.evidence_mut().denial_collection_unavailable = Some(true);
    output.evidence_mut().denial_collection_error = Some(error.into());
}
pub(crate) fn annotate_host_execution_approval(output: &mut Response, source: &str) {
    output.evidence_mut().host_execution_approved = Some(true);
    output.evidence_mut().approval_scope = Some("host_execution_once".into());
    output.evidence_mut().approval_source = Some(source.into());
}
pub(crate) fn annotate_filesystem_grant_approval(
    output: &mut Response,
    source: &str,
    grants: &[horizon_sandbox::FilesystemGrant],
    trigger_paths: &[PathBuf],
) {
    output.evidence_mut().approval_scope = Some("filesystem_grant".into());
    output.evidence_mut().approval_source = Some(source.into());
    output.evidence_mut().approved_filesystem_grants = Some(
        grants
            .iter()
            .map(|g| FilesystemGrant {
                path: g.path.display().to_string(),
                access: format!("{:?}", g.access),
                scope: format!("{:?}", g.scope),
            })
            .collect(),
    );
    output.evidence_mut().approval_trigger_paths = Some(
        trigger_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
    );
}
pub(crate) fn annotate_git_operation_approval(output: &mut Response, writable_roots: &[PathBuf]) {
    output.evidence_mut().git_operation_approved = Some(true);
    output.evidence_mut().approved_git_metadata_roots = Some(
        writable_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
    );
}
