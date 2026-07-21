//! One seccomp user-notification listener for filesystem and network policy.
//!
//! Derived from nono v0.68.0 `crates/nono/src/sandbox/linux.rs` proxy/open
//! filters. Horizon combines the disjoint programs because Linux permits only
//! one useful NEW_LISTENER ownership boundary for this helper. Socket creation
//! is notified too, so rejected protocols remain structured evidence.

use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;

const SECCOMP_DATA_NR_OFFSET: u32 = 0;
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;

const SECCOMP_SET_MODE_FILTER: u32 = 1;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: u32 = 1 << 3;
const SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV: u32 = 1 << 5;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH_NATIVE: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH_NATIVE: u32 = 0xc000_00b7;
#[cfg(target_arch = "riscv64")]
const AUDIT_ARCH_NATIVE: u32 = 0xc000_00f3;

#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64"
)))]
compile_error!("horizon-sandbox-runtime needs a native Linux audit architecture constant");

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SockFilterInsn {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilterInsn,
}

enum PendingInsn {
    Label(&'static str),
    Stmt {
        code: u16,
        k: u32,
    },
    Jump {
        code: u16,
        k: u32,
        yes: Option<&'static str>,
        no: Option<&'static str>,
    },
}

/// Installs the helper's single listener after Landlock is active.
pub(crate) fn install(network_mediation: bool) -> nono::Result<OwnedFd> {
    let filter = build(network_mediation);
    let program = SockFprog {
        len: u16::try_from(filter.len()).map_err(|_| {
            nono::NonoError::SandboxInit("combined seccomp filter is too large".to_string())
        })?,
        filter: filter.as_ptr(),
    };

    // Landlock has already set no_new_privs; repeat it defensively.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(nono::NonoError::SandboxInit(format!(
            "prctl(PR_SET_NO_NEW_PRIVS) failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    let mut flags = SECCOMP_FILTER_FLAG_NEW_LISTENER | SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV;
    let mut fd = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SECCOMP_SET_MODE_FILTER,
            flags,
            &program as *const SockFprog,
        )
    };
    if fd < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
        flags = SECCOMP_FILTER_FLAG_NEW_LISTENER;
        fd = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                SECCOMP_SET_MODE_FILTER,
                flags,
                &program as *const SockFprog,
            )
        };
    }
    if fd < 0 {
        return Err(nono::NonoError::SandboxInit(format!(
            "combined seccomp listener install failed: {}",
            std::io::Error::last_os_error()
        )));
    }

    // SAFETY: seccomp returned a fresh listener descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

fn build(network_mediation: bool) -> Vec<SockFilterInsn> {
    let mut pending = vec![
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_ARCH_OFFSET),
        jump(AUDIT_ARCH_NATIVE, Some("syscall"), None),
        label("kill"),
        stmt(BPF_RET_K, SECCOMP_RET_KILL_PROCESS),
        label("syscall"),
        stmt(BPF_LD_W_ABS, SECCOMP_DATA_NR_OFFSET),
        jump(libc::SYS_openat as u32, Some("notify"), None),
        jump(libc::SYS_openat2 as u32, Some("notify"), None),
    ];

    if network_mediation {
        pending.extend([
            jump(libc::SYS_socket as u32, Some("notify"), None),
            jump(libc::SYS_socketpair as u32, Some("notify"), None),
            jump(libc::SYS_connect as u32, Some("notify"), None),
            jump(libc::SYS_bind as u32, Some("notify"), None),
            jump(libc::SYS_sendto as u32, Some("notify"), None),
            jump(libc::SYS_sendmsg as u32, Some("notify"), None),
            jump(libc::SYS_sendmmsg as u32, Some("notify"), None),
            jump(libc::SYS_io_uring_setup as u32, Some("notify"), None),
        ]);
    }
    pending.push(jump_always("allow"));

    pending.extend([
        label("notify"),
        stmt(BPF_RET_K, SECCOMP_RET_USER_NOTIF),
        label("allow"),
        stmt(BPF_RET_K, SECCOMP_RET_ALLOW),
    ]);
    assemble(&pending)
}

fn label(name: &'static str) -> PendingInsn {
    PendingInsn::Label(name)
}

fn stmt(code: u16, k: u32) -> PendingInsn {
    PendingInsn::Stmt { code, k }
}

fn jump(k: u32, yes: Option<&'static str>, no: Option<&'static str>) -> PendingInsn {
    PendingInsn::Jump {
        code: BPF_JMP_JEQ_K,
        k,
        yes,
        no,
    }
}

fn jump_always(target: &'static str) -> PendingInsn {
    jump(0, Some(target), Some(target))
}

fn assemble(pending: &[PendingInsn]) -> Vec<SockFilterInsn> {
    let mut labels = HashMap::new();
    let mut index = 0usize;
    for instruction in pending {
        match instruction {
            PendingInsn::Label(name) => {
                labels.insert(*name, index);
            }
            _ => index += 1,
        }
    }

    let mut result = Vec::with_capacity(index);
    for instruction in pending {
        match instruction {
            PendingInsn::Label(_) => {}
            PendingInsn::Stmt { code, k } => result.push(SockFilterInsn {
                code: *code,
                jt: 0,
                jf: 0,
                k: *k,
            }),
            PendingInsn::Jump { code, k, yes, no } => {
                let current = result.len();
                result.push(SockFilterInsn {
                    code: *code,
                    jt: jump_offset(current, *yes, &labels),
                    jf: jump_offset(current, *no, &labels),
                    k: *k,
                });
            }
        }
    }
    result
}

fn jump_offset(
    current: usize,
    target: Option<&'static str>,
    labels: &HashMap<&'static str, usize>,
) -> u8 {
    let Some(target) = target else {
        return 0;
    };
    let target = labels[target];
    let distance = target
        .checked_sub(current + 1)
        .expect("classic BPF program only uses forward jumps");
    u8::try_from(distance).expect("classic BPF jump fits in u8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_filter_contains_one_terminal_notification_action() {
        let filter = build(true);
        assert_eq!(
            filter
                .iter()
                .filter(|instruction| instruction.code == BPF_RET_K
                    && instruction.k == SECCOMP_RET_USER_NOTIF)
                .count(),
            1
        );
    }

    #[test]
    fn open_only_filter_omits_network_syscall_numbers() {
        let filter = build(false);
        assert!(!filter.iter().any(|instruction| {
            instruction.code == BPF_JMP_JEQ_K && instruction.k == libc::SYS_connect as u32
        }));
    }
}
