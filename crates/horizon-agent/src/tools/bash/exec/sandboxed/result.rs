//! Preserve completion precedence independently of process management.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::super::{
    domain_denied, failed_output, finished, note_undrained, status_output, timeout_output,
};
use super::capture::Captured;
use crate::config::BashToolConfig;
use crate::contract::{ToolCallId, ToolCallResult};
use crate::policy::{annotate_denied_domains, annotate_sandboxed};
use crate::tools::bash::BashCompletion;

pub(super) fn complete(
    call_id: &ToolCallId,
    captured: Captured,
    denied_domains: Vec<String>,
    timeout: Duration,
    cwd_handle: &Arc<Mutex<PathBuf>>,
    config: &BashToolConfig,
) -> BashCompletion {
    let Captured {
        status,
        killed,
        drained,
        raw_stdout,
        raw_stderr,
        denials,
        #[cfg(target_os = "macos")]
        denial_collection_error,
    } = captured;
    if killed {
        let mut value = timeout_output(timeout, raw_stdout, config);
        annotate_common(&mut value, &denials);
        if !drained {
            note_undrained(&mut value, Duration::from_secs(config.drain_grace_secs));
        }
        return finish_or_domain_denied(call_id, value, denied_domains);
    }
    let Some(status) = status else {
        let mut value = failed_output(
            "failed to wait for sandboxed bash",
            Some(raw_stdout),
            config,
        );
        annotate_common(&mut value, &denials);
        // Wait failures historically carry no drain note or filesystem retry.
        return finish_or_domain_denied(call_id, value, denied_domains);
    };

    let mut value = status_output(status, raw_stdout, raw_stderr, cwd_handle, config);
    annotate_common(&mut value, &denials);
    #[cfg(target_os = "macos")]
    if let Some(error) = &denial_collection_error {
        crate::policy::annotate_denial_collection_unavailable(&mut value, error);
    }
    if !drained {
        note_undrained(&mut value, Duration::from_secs(config.drain_grace_secs));
    }
    // Filesystem retry takes precedence over domain retry. Both kinds of
    // evidence remain in the output, even if the shell itself exited zero.
    if !denials.filesystem.is_empty() {
        crate::policy::annotate_filesystem_denials(&mut value, &denials.filesystem);
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
        }
        return BashCompletion::FilesystemDenied {
            call_id: call_id.clone(),
            denials: denials.filesystem,
            // The daemon's fold stamps the originating request's occurrence
            // identity, keeping reused call IDs and denial retries distinct.
            result: ToolCallResult::new(call_id.clone(), None, value),
        };
    }
    #[cfg(target_os = "macos")]
    if !denials.mach_services.is_empty() {
        crate::policy::annotate_denied_mach_services(&mut value, &denials.mach_services);
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
        }
        return BashCompletion::MachServiceDenied {
            call_id: call_id.clone(),
            services: denials.mach_services,
            result: ToolCallResult::new(call_id.clone(), None, value),
        };
    }
    finish_or_domain_denied(call_id, value, denied_domains)
}

fn annotate_common(value: &mut Value, denials: &horizon_sandbox::ContainmentDenials) {
    annotate_sandboxed(value, true);
    crate::policy::annotate_network_denials(value, &denials.network);
    crate::policy::annotate_ungrantable_denials(value, &denials.ungrantable);
}

fn finish_or_domain_denied(
    call_id: &ToolCallId,
    mut value: Value,
    domains: Vec<String>,
) -> BashCompletion {
    if domains.is_empty() {
        finished(call_id, value)
    } else {
        annotate_denied_domains(&mut value, &domains);
        domain_denied(call_id, domains, value)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use horizon_sandbox::{
        ContainmentDenials, FilesystemDenial, FilesystemGrant, FilesystemGrantAccess,
        FilesystemGrantScope, NetworkDenial, UngrantableDenial,
    };
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn captured(status: Option<i32>, killed: bool) -> Captured {
        Captured {
            status: status.map(ExitStatus::from_raw),
            killed,
            drained: false,
            raw_stdout: Vec::new(),
            raw_stderr: b"/workspace/next".to_vec(),
            denials: ContainmentDenials {
                filesystem: vec![FilesystemDenial {
                    attempted_path: "/outside/file".into(),
                    grant: FilesystemGrant {
                        path: "/outside/file".into(),
                        access: FilesystemGrantAccess::ReadWrite,
                        scope: FilesystemGrantScope::File,
                        excluded_subpaths: Vec::new(),
                    },
                }],
                network: vec![NetworkDenial {
                    target: "192.0.2.1:443".into(),
                    operation: "connect".into(),
                    reason: "direct egress denied".into(),
                }],
                ungrantable: vec![UngrantableDenial {
                    attempted_path: "/".into(),
                    guidance: "choose a narrower path".into(),
                }],
                ..Default::default()
            },
            #[cfg(target_os = "macos")]
            denial_collection_error: None,
        }
    }

    fn finish(captured: Captured, domains: Vec<String>) -> (BashCompletion, PathBuf) {
        let cwd = Arc::new(Mutex::new(PathBuf::from("/workspace")));
        let completion = complete(
            &ToolCallId("result-precedence".into()),
            captured,
            domains,
            Duration::from_secs(1),
            &cwd,
            &crate::config::AgentToolsConfig::default().bash,
        );
        let final_cwd = cwd.lock().unwrap().clone();
        (completion, final_cwd)
    }

    #[test]
    fn filesystem_retry_wins_over_domain_retry_even_after_successful_exit() {
        let capture = captured(Some(0), false);
        let expected_denials = capture.denials.filesystem.clone();
        let (completion, cwd) = finish(capture, vec!["example.test".into()]);
        let BashCompletion::FilesystemDenied {
            result, denials, ..
        } = completion
        else {
            panic!("filesystem denial must take precedence");
        };
        assert_eq!(denials, expected_denials);
        assert!(result.is_error);
        assert!(result.occurrence_id.is_none());
        assert_eq!(result.output["exit_code"], 0);
        assert_eq!(result.output["sandboxed"], true);
        assert_eq!(result.output["denied_domains"][0], "example.test");
        assert_eq!(result.output["denied_filesystem_paths"][0], "/outside/file");
        assert_eq!(
            result.output["denied_network_routes"][0]["target"],
            "192.0.2.1:443"
        );
        assert_eq!(
            result.output["ungrantable_filesystem_paths"][0]["path"],
            "/"
        );
        assert!(result.output["note"]
            .as_str()
            .unwrap()
            .contains("output capture stopped"));
        assert_eq!(cwd, PathBuf::from("/workspace/next"));
    }

    #[test]
    fn timeout_and_wait_failure_keep_domain_evidence_without_filesystem_retry() {
        for killed in [false, true] {
            let (completion, cwd) = finish(captured(None, killed), vec!["example.test".into()]);
            let BashCompletion::DomainDenied {
                result, domains, ..
            } = completion
            else {
                panic!("timeout and wait failure must not request filesystem retry");
            };
            assert_eq!(domains, ["example.test"]);
            assert!(result.is_error);
            assert_eq!(result.output["sandboxed"], true);
            assert_eq!(
                result.output["denied_network_routes"][0]["operation"],
                "connect"
            );
            assert_eq!(
                result.output["ungrantable_filesystem_paths"][0]["path"],
                "/"
            );
            assert!(result.output.get("denied_filesystem_paths").is_none());
            assert_eq!(result.output.get("note").is_some(), killed);
            assert!(result.output["message"]
                .as_str()
                .unwrap()
                .contains(if killed {
                    "timed out"
                } else {
                    "failed to wait"
                }));
            assert_eq!(cwd, PathBuf::from("/workspace"));
        }
    }

    #[test]
    fn domain_denial_overrides_success_while_plain_exit_and_signal_keep_their_meaning() {
        for (raw_status, domains) in [
            (0, vec!["example.test".into()]),
            (7 << 8, vec![]),
            (9, vec![]),
        ] {
            let mut capture = captured(Some(raw_status), false);
            capture.denials = ContainmentDenials::default();
            capture.drained = true;
            let (completion, cwd) = finish(capture, domains);
            let result = match completion {
                BashCompletion::DomainDenied { result, .. } if raw_status == 0 => result,
                BashCompletion::Finished(result) if raw_status != 0 => result,
                _ => panic!("unexpected completion"),
            };
            assert_eq!(result.is_error, raw_status != 7 << 8);
            assert_eq!(result.output["sandboxed"], true);
            assert!(result.output.get("note").is_none());
            if raw_status == 9 {
                assert_eq!(cwd, PathBuf::from("/workspace"));
                assert!(result.output["message"]
                    .as_str()
                    .unwrap()
                    .contains("signal 9"));
            } else {
                assert_eq!(result.output["exit_code"], raw_status >> 8);
                assert_eq!(cwd, PathBuf::from("/workspace/next"));
            }
        }
    }
}
