//! Reduced openat/openat2 recording-deny handler.
//!
//! Derived from nono-cli v0.68.0's `exec_strategy/supervisor_linux.rs`.
//! Horizon removes live fd injection and trust/profile policy: an unmatched
//! syscall is recorded, denied immediately, and may only succeed in a fresh
//! sandbox after an external narrow-grant approval.

use super::RateLimiter;
use crate::RecordingDenyBackend;
use nono::sandbox::{
    classify_access_from_flags, continue_notif, deny_notif, notif_id_valid, read_notif_path,
    read_open_how, resolve_notif_path, validate_openat2_size, SeccompNotif, SYS_OPENAT,
    SYS_OPENAT2,
};
use nono::{
    try_canonicalize, AccessMode, ApprovalBackend, ApprovalRequest, DenialReason, DenialRecord,
};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_DENIALS: usize = 1_000;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitialCapability {
    pub(crate) path: PathBuf,
    pub(crate) access: AccessMode,
    pub(crate) is_file: bool,
}

enum InitialCapabilityMatch<'a> {
    Sufficient,
    Insufficient(&'a InitialCapability),
    None,
}

pub(crate) struct OpenNotificationContext<'a> {
    pub(crate) child_pid: u32,
    pub(crate) session_id: &'a str,
    pub(crate) initial_caps: &'a [InitialCapability],
    pub(crate) backend: &'a RecordingDenyBackend,
    pub(crate) rate_limiter: &'a mut RateLimiter,
    pub(crate) denials: &'a mut Vec<DenialRecord>,
}

pub(crate) fn handle_open_notification(
    notify_fd: RawFd,
    notification: SeccompNotif,
    context: OpenNotificationContext<'_>,
) -> nono::Result<()> {
    let path = match read_notif_path(notification.pid, notification.data.args[1])
        .and_then(|raw| resolve_notif_path(notification.pid, notification.data.args[0], &raw))
    {
        Ok(path) => path,
        Err(error) => {
            let _ = deny_notif(notify_fd, notification.id);
            return Err(error);
        }
    };
    if !notif_id_valid(notify_fd, notification.id)? {
        return Ok(());
    }

    let raw_flags = match notification.data.nr {
        SYS_OPENAT => notification.data.args[2] as i32,
        SYS_OPENAT2 => {
            let size = notification.data.args[3] as usize;
            if !validate_openat2_size(size) {
                let _ = deny_notif(notify_fd, notification.id);
                return Ok(());
            }
            match read_open_how(notification.pid, notification.data.args[2]) {
                Ok(how) => how.flags as i32,
                Err(error) => {
                    let _ = deny_notif(notify_fd, notification.id);
                    return Err(error);
                }
            }
        }
        _ => {
            let _ = deny_notif(notify_fd, notification.id);
            return Ok(());
        }
    };
    let access = classify_access_from_flags(raw_flags);
    let canonicalized = try_canonicalize(&path);

    match match_initial_capability(&canonicalized, access, context.initial_caps) {
        InitialCapabilityMatch::Sufficient => {
            // This is not a runtime expansion. Landlock remains authoritative
            // and re-checks the actual syscall arguments after continuation.
            if notif_id_valid(notify_fd, notification.id)? {
                continue_notif(notify_fd, notification.id)?;
            }
            return Ok(());
        }
        InitialCapabilityMatch::Insufficient(capability) => {
            let _ = &capability.path;
            record_denial(
                context.denials,
                canonicalized.clone(),
                access,
                DenialReason::InsufficientAccess,
            );
        }
        InitialCapabilityMatch::None => {}
    }

    // Preserve ordinary ENOENT behavior for harmless read probes. Unlike the
    // upstream CLI, do not continue missing write/create attempts: that would
    // let Landlock deny them opaquely and recreate Horizon's original bug.
    if is_missing_read_probe(&path, access, raw_flags) {
        if notif_id_valid(notify_fd, notification.id)? {
            continue_notif(notify_fd, notification.id)?;
        }
        return Ok(());
    }

    if !context.rate_limiter.try_acquire() {
        record_denial(
            context.denials,
            canonicalized,
            access,
            DenialReason::RateLimited,
        );
        let _ = deny_notif(notify_fd, notification.id);
        return Ok(());
    }

    let request = ApprovalRequest::Capability {
        request_id: format!(
            "horizon-seccomp-{}-{}",
            std::process::id(),
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        ),
        // Canonicalize through the longest existing ancestor. This path may
        // name a grant request, but grant construction must canonicalize again
        // after approval; it is not an inode identity proof.
        path: canonicalized.clone(),
        access,
        reason: Some("sandbox intercepted an openat/openat2 boundary crossing".to_string()),
        child_pid: context.child_pid,
        session_id: context.session_id.to_string(),
    };
    let decision = context.backend.request_approval(&request)?;
    record_denial(
        context.denials,
        canonicalized,
        access,
        if decision.is_denied() {
            DenialReason::UserDenied
        } else {
            DenialReason::BackendError
        },
    );

    if notif_id_valid(notify_fd, notification.id)? {
        deny_notif(notify_fd, notification.id)?;
    }
    Ok(())
}

fn is_missing_read_probe(path: &Path, access: AccessMode, flags: i32) -> bool {
    let changes_filesystem = flags & (libc::O_CREAT | libc::O_TRUNC | libc::O_TMPFILE) != 0;
    access == AccessMode::Read
        && !changes_filesystem
        && std::fs::symlink_metadata(path).is_err_and(|error| {
            error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ENOTDIR)
        })
}

fn record_denial(
    denials: &mut Vec<DenialRecord>,
    path: PathBuf,
    access: AccessMode,
    reason: DenialReason,
) {
    if denials.len() < MAX_DENIALS {
        denials.push(DenialRecord {
            path,
            access,
            reason,
        });
    }
}

fn match_initial_capability<'a>(
    path: &Path,
    requested: AccessMode,
    initial_caps: &'a [InitialCapability],
) -> InitialCapabilityMatch<'a> {
    let mut best_covering = None;
    let mut best_sufficient = None;
    let mut best_covering_score = 0;
    let mut best_sufficient_score = 0;

    for capability in initial_caps {
        let covers = if capability.is_file {
            path == capability.path
        } else {
            path.starts_with(&capability.path)
        };
        if !covers {
            continue;
        }
        let score = capability.path.as_os_str().len();
        if score >= best_covering_score {
            best_covering = Some(capability);
            best_covering_score = score;
        }
        if capability.access.contains(requested) && score >= best_sufficient_score {
            best_sufficient = Some(capability);
            best_sufficient_score = score;
        }
    }

    if best_sufficient.is_some() {
        InitialCapabilityMatch::Sufficient
    } else if let Some(capability) = best_covering {
        InitialCapabilityMatch::Insufficient(capability)
    } else {
        InitialCapabilityMatch::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_sufficient_capability_wins() {
        let caps = vec![
            InitialCapability {
                path: PathBuf::from("/workspace"),
                access: AccessMode::Read,
                is_file: false,
            },
            InitialCapability {
                path: PathBuf::from("/workspace/build"),
                access: AccessMode::ReadWrite,
                is_file: false,
            },
        ];
        assert!(matches!(
            match_initial_capability(
                Path::new("/workspace/build/output"),
                AccessMode::Write,
                &caps
            ),
            InitialCapabilityMatch::Sufficient
        ));
    }

    #[test]
    fn file_capability_does_not_cover_children() {
        let caps = vec![InitialCapability {
            path: PathBuf::from("/workspace/file"),
            access: AccessMode::ReadWrite,
            is_file: true,
        }];
        assert!(matches!(
            match_initial_capability(Path::new("/workspace/file/child"), AccessMode::Read, &caps),
            InitialCapabilityMatch::None
        ));
    }
}
