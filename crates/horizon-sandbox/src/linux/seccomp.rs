//! Network-syscall cut (`docs/agent-approval-design.md`: "seccompiler for
//! the network-syscall cut"). Layered *underneath* bwrap's own
//! `--unshare-all` (`linux::bwrap`, the primary/more robust cut: with no
//! network namespace there is no interface to reach at all) as
//! defense-in-depth against any path that doesn't go through the
//! namespace unshare.
//!
//! `seccompiler::apply_filter` installs a filter on the *calling thread*
//! (inherited by that thread's future `fork`/`exec` descendants only, per
//! `seccomp(2)`), so building the filter here is safe anywhere; actually
//! applying it must happen on the dedicated thread that goes on to spawn
//! bwrap (see `linux::spawn`), never on a thread horizon-sessiond reuses
//! for anything else.

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule,
};
use std::collections::BTreeMap;
use std::convert::TryInto;

/// Address families denied at `socket(2)` -- the syscall every network
/// operation starts with. AF_UNIX and other local-IPC families stay open
/// (syslog, D-Bus, abstract sockets some tools rely on).
const DENIED_FAMILIES: [i32; 3] = [libc::AF_INET, libc::AF_INET6, libc::AF_PACKET];

/// Builds a BPF program that returns `EPERM` for `socket(2)` calls
/// requesting a denied address family, and allows everything else.
pub(crate) fn build_network_cut_filter() -> Result<BpfProgram, String> {
    let rules = DENIED_FAMILIES
        .iter()
        .map(|&family| {
            let condition =
                SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, family as u64)
                    .map_err(|e| e.to_string())?;
            SeccompRule::new(vec![condition]).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut rules_by_syscall = BTreeMap::new();
    rules_by_syscall.insert(libc::SYS_socket, rules);

    let filter = SeccompFilter::new(
        rules_by_syscall,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        std::env::consts::ARCH
            .try_into()
            .map_err(|e| format!("unsupported target arch {}: {e}", std::env::consts::ARCH))?,
    )
    .map_err(|e| e.to_string())?;

    let program: BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| e.to_string())?;
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_non_empty_program() {
        let program = build_network_cut_filter().expect("filter should build on this host arch");
        assert!(!program.is_empty());
    }
}
