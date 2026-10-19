//! Path-mutating syscall handler for write exclusions (board #68).
//!
//! Horizon-local addition with no upstream counterpart (see `UPSTREAM.md`).
//! Landlock allows a writable root as a whole, so a write exclusion beneath
//! it (`writable_root_exclusions`, a grant's `excluded_subpaths`) is enforced
//! only here and in the open handler. Every path operand the syscall mutates
//! is read from the tracee, resolved the way the tracee's kernel would
//! resolve it, and treated as a write at that path. The call is denied when
//! an operand is at, beneath, or above an excluded path; otherwise it is
//! continued and Landlock decides it as before. Operands outside the
//! writable roots are left to Landlock.

use super::network::parse_status_tgid;
use super::open::{record_denial, request_denial, AT_FDCWD_ARG};
use super::{InitialCapability, OpenNotificationContext};
use nono::sandbox::{
    continue_notif, deny_notif, notif_id_valid, respond_notif_errno, SeccompNotif,
};
use nono::{AccessMode, DenialReason};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const PATH_MAX: usize = 4096;
const MAX_SYMLINK_HOPS: usize = 40;
const PROC_SUPER_MAGIC: i64 = 0x9fa0;

/// Notified syscalls (number, name) beyond `openat`/`openat2`. Legacy
/// `open`/`creat` are dispatched to the open handler; the rest come here.
#[cfg(target_arch = "x86_64")]
const PATH_MUTATIONS: &[(libc::c_long, &str)] = &[
    (libc::SYS_rename, "rename"),
    (libc::SYS_renameat, "renameat"),
    (libc::SYS_renameat2, "renameat2"),
    (libc::SYS_link, "link"),
    (libc::SYS_linkat, "linkat"),
    (libc::SYS_symlink, "symlink"),
    (libc::SYS_symlinkat, "symlinkat"),
    (libc::SYS_unlink, "unlink"),
    (libc::SYS_unlinkat, "unlinkat"),
    (libc::SYS_rmdir, "rmdir"),
    (libc::SYS_mkdir, "mkdir"),
    (libc::SYS_mkdirat, "mkdirat"),
    (libc::SYS_mknod, "mknod"),
    (libc::SYS_mknodat, "mknodat"),
    (libc::SYS_truncate, "truncate"),
    (libc::SYS_open, "open"),
    (libc::SYS_creat, "creat"),
];

#[cfg(target_arch = "aarch64")]
const PATH_MUTATIONS: &[(libc::c_long, &str)] = &[
    (libc::SYS_renameat, "renameat"),
    (libc::SYS_renameat2, "renameat2"),
    (libc::SYS_linkat, "linkat"),
    (libc::SYS_symlinkat, "symlinkat"),
    (libc::SYS_unlinkat, "unlinkat"),
    (libc::SYS_mkdirat, "mkdirat"),
    (libc::SYS_mknodat, "mknodat"),
    (libc::SYS_truncate, "truncate"),
];

#[cfg(target_arch = "riscv64")]
const PATH_MUTATIONS: &[(libc::c_long, &str)] = &[
    (libc::SYS_renameat2, "renameat2"),
    (libc::SYS_linkat, "linkat"),
    (libc::SYS_symlinkat, "symlinkat"),
    (libc::SYS_unlinkat, "unlinkat"),
    (libc::SYS_mkdirat, "mkdirat"),
    (libc::SYS_mknodat, "mknodat"),
    (libc::SYS_truncate, "truncate"),
];

/// Syscall numbers the filter notifies when path mutation mediation is on.
pub(crate) fn path_mutation_syscalls() -> Vec<libc::c_long> {
    PATH_MUTATIONS.iter().map(|(nr, _)| *nr).collect()
}

/// Whether `nr` is legacy `open`/`creat`, handled like `openat`.
pub(crate) fn is_legacy_open(nr: i32) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        nr == libc::SYS_open as i32 || nr == libc::SYS_creat as i32
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = nr;
        false
    }
}

/// Whether `nr` belongs to [`handle_path_mutation_notification`].
pub(crate) fn is_path_mutation(nr: i32) -> bool {
    !is_legacy_open(nr) && syscall_name(nr).is_some()
}

fn syscall_name(nr: i32) -> Option<&'static str> {
    PATH_MUTATIONS
        .iter()
        .find(|(number, _)| *number as i32 == nr)
        .map(|(_, name)| *name)
}

/// One path operand the syscall mutates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Operand {
    dirfd: u64,
    path_addr: u64,
    /// Whether the kernel follows a symlink in the final component.
    follow_final: bool,
    /// `linkat(..., AT_EMPTY_PATH)`: an empty path names `dirfd` itself.
    empty_names_dirfd: bool,
}

impl Operand {
    fn at(dirfd: u64, path_addr: u64) -> Self {
        Self {
            dirfd,
            path_addr,
            follow_final: false,
            empty_names_dirfd: false,
        }
    }

    fn cwd(path_addr: u64) -> Self {
        Self::at(AT_FDCWD_ARG, path_addr)
    }
}

/// The operands a syscall mutates, each treated as a write at its path.
/// `rename*` mutates both names; `link*` writes the new name and, since a
/// hard link shares the inode, the old one too; `symlink*` writes only the
/// link path (the target string is never resolved by the call).
fn mutated_operands(nr: i32, args: &[u64; 6]) -> Option<Vec<Operand>> {
    let name = syscall_name(nr)?;
    Some(match name {
        "rename" | "link" => vec![Operand::cwd(args[0]), Operand::cwd(args[1])],
        "renameat" | "renameat2" => {
            vec![Operand::at(args[0], args[1]), Operand::at(args[2], args[3])]
        }
        "linkat" => {
            let flags = args[4] as i32;
            vec![
                Operand {
                    dirfd: args[0],
                    path_addr: args[1],
                    follow_final: flags & libc::AT_SYMLINK_FOLLOW != 0,
                    empty_names_dirfd: flags & libc::AT_EMPTY_PATH != 0,
                },
                Operand::at(args[2], args[3]),
            ]
        }
        "symlink" => vec![Operand::cwd(args[1])],
        "symlinkat" => vec![Operand::at(args[1], args[2])],
        "unlink" | "rmdir" | "mkdir" | "mknod" => vec![Operand::cwd(args[0])],
        "unlinkat" | "mkdirat" | "mknodat" => vec![Operand::at(args[0], args[1])],
        "truncate" => vec![Operand {
            follow_final: true,
            ..Operand::cwd(args[0])
        }],
        _ => return None,
    })
}

pub(crate) fn handle_path_mutation_notification(
    notify_fd: RawFd,
    notification: SeccompNotif,
    mut context: OpenNotificationContext<'_>,
) -> nono::Result<()> {
    let nr = notification.data.nr;
    let Some(operands) = mutated_operands(nr, &notification.data.args) else {
        let _ = deny_notif(notify_fd, notification.id);
        return Ok(());
    };

    let mut raw_paths = Vec::with_capacity(operands.len());
    for operand in &operands {
        match read_path_operand(notification.pid, operand.path_addr) {
            Ok(raw) => raw_paths.push(raw),
            Err(errno) => return respond_errno(notify_fd, notification.id, errno),
        }
    }
    // The operands came from tracee memory: act only on a notification that
    // is still pending, as the open handler does.
    if !notif_id_valid(notify_fd, notification.id)? {
        return Ok(());
    }

    let mut excluded = Vec::new();
    for (operand, raw) in operands.iter().zip(raw_paths) {
        let target = match resolve_operand(notification.pid, operand, raw) {
            Ok(target) => target,
            // The kernel would fail the same lookup. Answering here rather
            // than continuing keeps a concurrently created component from
            // redirecting the call after the check.
            Err(errno) => return respond_errno(notify_fd, notification.id, errno),
        };
        if let Some(path) = excluded_target(&target, context.initial_caps) {
            if !excluded.contains(&path) {
                excluded.push(path);
            }
        }
    }

    if excluded.is_empty() {
        // Not an exclusion: Landlock decides the syscall as it did before.
        if notif_id_valid(notify_fd, notification.id)? {
            continue_notif(notify_fd, notification.id)?;
        }
        return Ok(());
    }

    let reason = format!(
        "sandbox refused {} of a write-excluded path",
        syscall_name(nr).unwrap_or("a path mutation")
    );
    for path in excluded {
        record_denial(
            context.denials,
            path.clone(),
            AccessMode::Write,
            DenialReason::InsufficientAccess,
        );
        if !request_denial(path, AccessMode::Write, &reason, &mut context)? {
            break;
        }
    }
    if notif_id_valid(notify_fd, notification.id)? {
        deny_notif(notify_fd, notification.id)?;
    }
    Ok(())
}

fn respond_errno(notify_fd: RawFd, id: u64, errno: i32) -> nono::Result<()> {
    if notif_id_valid(notify_fd, id)? {
        respond_notif_errno(notify_fd, id, errno)?;
    }
    Ok(())
}

/// The path recorded for an operand that touches an exclusion: the operand
/// itself when it is at or beneath an excluded path, or the excluded path
/// when the operand is one of its ancestors inside the same capability
/// (renaming or replacing an ancestor replaces the excluded path's content).
fn excluded_target(path: &Path, initial_caps: &[InitialCapability]) -> Option<PathBuf> {
    for capability in initial_caps {
        if capability.is_file || !path.starts_with(&capability.path) {
            continue;
        }
        for excluded in &capability.excluded_paths {
            if path.starts_with(excluded) {
                return Some(path.to_path_buf());
            }
            if excluded.starts_with(path) {
                return Some(excluded.clone());
            }
        }
    }
    None
}

/// Reads a NUL-terminated path operand from tracee memory. Unlike
/// `nono::sandbox::read_notif_path`, an empty or non-UTF-8 path is returned
/// as-is: the kernel accepts both, so refusing them would break ordinary
/// file names rather than protect anything.
fn read_path_operand(pid: u32, addr: u64) -> Result<Vec<u8>, i32> {
    use std::os::unix::fs::FileExt;

    let mem = std::fs::File::open(format!("/proc/{pid}/mem")).map_err(|_| libc::EFAULT)?;
    let mut buf = vec![0_u8; PATH_MAX];
    let mut filled = 0;
    while filled < PATH_MAX {
        let offset = addr.checked_add(filled as u64).ok_or(libc::EFAULT)?;
        match mem.read_at(&mut buf[filled..], offset) {
            Ok(0) => break,
            Ok(read) => {
                if let Some(end) = buf[filled..filled + read]
                    .iter()
                    .position(|byte| *byte == 0)
                {
                    buf.truncate(filled + end);
                    return Ok(buf);
                }
                filled += read;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    Err(if filled >= PATH_MAX {
        libc::ENAMETOOLONG
    } else {
        libc::EFAULT
    })
}

/// Turns a raw operand into the absolute path its syscall would act on.
fn resolve_operand(tid: u32, operand: &Operand, raw: Vec<u8>) -> Result<PathBuf, i32> {
    let raw = PathBuf::from(OsString::from_vec(raw));
    if raw.as_os_str().is_empty() && !operand.empty_names_dirfd {
        return Err(libc::ENOENT);
    }
    let full = if raw.is_absolute() {
        raw
    } else {
        dirfd_path(tid, operand.dirfd)?.join(raw)
    };
    let follow_final = operand.follow_final || operand.empty_names_dirfd;
    resolve_in_tracee(tid, &full, follow_final)
}

/// The `/proc` link naming the directory a relative operand starts from.
fn dirfd_path(tid: u32, dirfd: u64) -> Result<PathBuf, i32> {
    // The kernel reads dirfd as an `int`: only the low 32 bits count.
    let fd = dirfd as u32 as i32;
    if fd == libc::AT_FDCWD {
        return Ok(PathBuf::from(format!("/proc/{tid}/cwd")));
    }
    if fd < 0 {
        return Err(libc::EBADF);
    }
    Ok(PathBuf::from(format!("/proc/{tid}/fd/{fd}")))
}

/// Resolves `path` component by component as the tracee's kernel would.
///
/// `std::fs::canonicalize` would resolve `/proc/self` (directly, or through
/// a symlink such as `/dev/fd`) as the supervisor rather than the tracee,
/// and always follows a final symlink although `rename`/`unlink`/`mkdir`/
/// `link`/`symlink` act on the link itself. A missing component fails with
/// the errno the kernel would return; with `follow_final` the final
/// component must exist too.
fn resolve_in_tracee(tid: u32, path: &Path, follow_final: bool) -> Result<PathBuf, i32> {
    let mut pending = split_components(path.as_os_str());
    let mut resolved = PathBuf::from("/");
    let mut hops = 0;
    while let Some(component) = pending.pop_front() {
        if component == "." {
            continue;
        }
        if component == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&component);
        let last = pending.is_empty();
        if last && !follow_final {
            return Ok(candidate);
        }
        let metadata = std::fs::symlink_metadata(&candidate).map_err(io_errno)?;
        if metadata.file_type().is_symlink() {
            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return Err(libc::ELOOP);
            }
            let target = match proc_self_target(&resolved, &component, tid)? {
                Some(target) => target,
                None => std::fs::read_link(&candidate).map_err(io_errno)?,
            };
            if target.is_absolute() {
                resolved = PathBuf::from("/");
            }
            for part in split_components(target.as_os_str()).into_iter().rev() {
                pending.push_front(part);
            }
            continue;
        }
        if !last && !metadata.is_dir() {
            return Err(libc::ENOTDIR);
        }
        resolved = candidate;
    }
    Ok(resolved)
}

/// The tracee's meaning of procfs `self`/`thread-self`, relative to the
/// procfs root `parent`; `None` for any other component.
fn proc_self_target(parent: &Path, component: &OsStr, tid: u32) -> Result<Option<PathBuf>, i32> {
    if component != "self" && component != "thread-self" {
        return Ok(None);
    }
    if !is_procfs(parent) {
        return Ok(None);
    }
    let tgid = read_tgid(tid)?;
    Ok(Some(if component == "self" {
        PathBuf::from(tgid.to_string())
    } else {
        PathBuf::from(format!("{tgid}/task/{tid}"))
    }))
}

fn is_procfs(path: &Path) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: c_path is NUL-terminated and stat points to writable storage.
    if unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statfs returned success, so the struct is initialized.
    #[allow(clippy::unnecessary_cast)]
    let f_type = unsafe { stat.assume_init() }.f_type as i64;
    f_type == PROC_SUPER_MAGIC
}

fn read_tgid(tid: u32) -> Result<u32, i32> {
    let status = std::fs::read_to_string(format!("/proc/{tid}/status")).map_err(io_errno)?;
    parse_status_tgid(&status).ok_or(libc::ESRCH)
}

fn split_components(path: &OsStr) -> VecDeque<OsString> {
    path.as_bytes()
        .split(|byte| *byte == b'/')
        .filter(|part| !part.is_empty())
        .map(|part| OsStr::from_bytes(part).to_os_string())
        .collect()
}

fn io_errno(error: std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(libc::EACCES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(path: &str, excluded: &[&str]) -> Vec<InitialCapability> {
        vec![InitialCapability {
            path: PathBuf::from(path),
            access: AccessMode::ReadWrite,
            is_file: false,
            excluded_paths: excluded.iter().map(PathBuf::from).collect(),
        }]
    }

    #[test]
    fn operands_at_beneath_or_above_an_exclusion_are_refused() {
        let caps = caps("/repo/.git", &["/repo/.git/hooks", "/repo/.git/config"]);
        assert_eq!(
            excluded_target(Path::new("/repo/.git/hooks/pre-commit"), &caps),
            Some(PathBuf::from("/repo/.git/hooks/pre-commit"))
        );
        assert_eq!(
            excluded_target(Path::new("/repo/.git/config"), &caps),
            Some(PathBuf::from("/repo/.git/config"))
        );
        // Replacing the granted gitdir itself would replace `hooks/`.
        assert_eq!(
            excluded_target(Path::new("/repo/.git"), &caps),
            Some(PathBuf::from("/repo/.git/hooks"))
        );
        assert_eq!(
            excluded_target(Path::new("/repo/.git/config.bak"), &caps),
            None
        );
        assert_eq!(
            excluded_target(Path::new("/repo/.git/objects/ab"), &caps),
            None
        );
        // Outside the capability is Landlock's business.
        assert_eq!(excluded_target(Path::new("/repo"), &caps), None);
        assert_eq!(excluded_target(Path::new("/elsewhere/x"), &caps), None);
    }

    #[test]
    fn rename_and_link_mutate_both_names_and_symlink_only_the_link() {
        let args = [1, 2, 3, 4, 0, 0];
        let renameat2 = mutated_operands(libc::SYS_renameat2 as i32, &args).unwrap();
        assert_eq!(
            renameat2,
            vec![Operand::at(1, 2), Operand::at(3, 4)],
            "renameat2 mutates old and new"
        );
        let linkat = mutated_operands(libc::SYS_linkat as i32, &args).unwrap();
        assert_eq!(linkat.len(), 2);
        let symlinkat = mutated_operands(libc::SYS_symlinkat as i32, &args).unwrap();
        assert_eq!(symlinkat, vec![Operand::at(2, 3)]);
        let truncate = mutated_operands(libc::SYS_truncate as i32, &args).unwrap();
        assert!(truncate[0].follow_final);
        assert!(mutated_operands(libc::SYS_openat as i32, &args).is_none());
    }

    #[test]
    fn linkat_flags_select_following_and_empty_path() {
        let args = [
            5,
            6,
            7,
            8,
            (libc::AT_SYMLINK_FOLLOW | libc::AT_EMPTY_PATH) as u64,
            0,
        ];
        let operands = mutated_operands(libc::SYS_linkat as i32, &args).unwrap();
        assert!(operands[0].follow_final);
        assert!(operands[0].empty_names_dirfd);
        assert!(!operands[1].follow_final);
    }

    #[test]
    fn resolution_keeps_a_final_symlink_but_follows_intermediate_ones() {
        let dir = std::env::temp_dir().join(format!(
            "horizon-mutation-resolve-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let dir = dir.canonicalize().unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let tid = std::process::id();

        assert_eq!(
            resolve_in_tracee(tid, &dir.join("link"), false),
            Ok(dir.join("link"))
        );
        assert_eq!(
            resolve_in_tracee(tid, &dir.join("link"), true),
            Ok(dir.join("real"))
        );
        assert_eq!(
            resolve_in_tracee(tid, &dir.join("link/new"), false),
            Ok(dir.join("real/new"))
        );
        assert_eq!(
            resolve_in_tracee(tid, &dir.join("missing/new"), false),
            Err(libc::ENOENT)
        );
        assert_eq!(
            resolve_in_tracee(tid, &dir.join("real/../link/x"), false),
            Ok(dir.join("real/x"))
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn proc_self_resolves_as_the_tracee() {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let tid = std::process::id();
        assert_eq!(
            resolve_in_tracee(tid, Path::new("/proc/self/cwd/new-entry"), false),
            Ok(cwd.join("new-entry"))
        );
    }
}
