//! Landlock fs backstop (`docs/agent-approval-design.md`).
//!
//! ## Sharp edge found while building this: Landlock and bwrap can't share
//! ## a thread
//!
//! The obvious design -- `restrict_self()` on the same thread that then
//! spawns bwrap, so the restriction is inherited into bwrap and beyond --
//! **does not work**. Landlock has a documented, permanent kernel
//! limitation: "arbitrary mounts are always denied" for a restricted
//! thread (see the `landlock` crate's own top-level docs, "Current
//! limitations"). bwrap's *entire* mechanism is namespace/mount setup
//! (`unshare(CLONE_NEWUSER/NEWNS)`, then a sequence of `mount(2)` calls) --
//! so a landlocked thread cannot become bwrap at all. Reproduced directly:
//! restricting a thread, then spawning `bwrap` from it, fails with
//! `bwrap: Failed to make / slave: Operation not permitted` (an earlier,
//! narrower ruleset instead failed at `bwrap: setting up uid map:
//! Permission denied` -- writing `/proc/self/{uid,gid}_map` is itself a
//! plain file write Landlock can gate, but the mount-slave step below it
//! can't be un-blocked by any rule at all). bwrap has no equivalent of its
//! own `--seccomp FD` hook (which installs a filter *after* its setup,
//! scoped to only the final target) for Landlock, so genuinely layering
//! Landlock as a backstop *around the sandboxed command* -- not around
//! bwrap itself -- needs a companion process that bwrap execs *after* its
//! own mount setup (so it never calls `mount()` itself), which then
//! applies Landlock and execs the real target. That's a real, buildable
//! follow-up (worth a roadmap/backlog note), not attempted in this spike.
//!
//! What this module does today: builds the same ruleset the design calls
//! for and negotiates+reports the ABI/enforcement level on an isolated,
//! throwaway thread (see [`negotiate`]) that never spawns anything --
//! satisfying the "explicit ABI negotiation, visible on downgrade"
//! requirement as a diagnostic capability, without the false claim that
//! it currently protects the spawned command (`linux::spawn` does not
//! call this for its bwrap-spawning thread; see that module's doc).
//!
//! Landlock restrictions apply to the calling thread and are inherited
//! only by that thread's future `fork`/`exec` descendants (`landlock(2)`)
//! -- which is exactly why an isolated, disposable thread is the right
//! shape for a pure capability probe: whatever it restricts dies with it.

use landlock::{
    path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreated, RulesetCreatedAttr, RulesetStatus, ABI,
};

use crate::error::SandboxError;
use crate::policy::{ReadableScope, SandboxPolicy};

/// Newest Landlock ABI this crate codes against (landlock 0.4.5's
/// ceiling for filesystem access rights predates `Scope`/logging, which
/// this backstop doesn't need). Bump deliberately when a newer ABI adds fs
/// rights worth requesting.
const TARGET_ABI: ABI = ABI::V5;

/// The negotiation outcome: what this crate requested vs. what the
/// running kernel actually supports. See the module docs for why this is
/// currently a diagnostic capability, not live enforcement around a
/// spawned command.
#[derive(Debug, PartialEq, Eq)]
pub struct LandlockReport {
    /// The ABI this crate requested.
    pub target_abi: ABI,
    /// What the running kernel would enforce, were this ruleset actually
    /// applied to a real spawn.
    pub enforcement: RulesetStatus,
}

impl LandlockReport {
    /// True if the kernel would enforce less than requested (older ABI,
    /// or Landlock unavailable entirely) -- the case that must stay
    /// visible, never silently promoted to "fully protected"
    /// (`docs/research/agent-approval-prior-art-2026-07-19.md` cites
    /// Codex's own hardcoded-ABI bug as the cautionary tale here).
    pub fn is_downgraded(&self) -> bool {
        self.enforcement != RulesetStatus::FullyEnforced
    }
}

/// Builds (but does not yet apply) the ruleset for `policy`. Everything
/// fallible/allocating happens here, in normal (non-thread-restricted)
/// process context; only the final `restrict_self()` call (see
/// [`negotiate`]) is thread-scoped.
fn build(policy: &SandboxPolicy) -> Result<RulesetCreated, SandboxError> {
    let all_access = AccessFs::from_all(TARGET_ABI);
    let read_access = AccessFs::from_read(TARGET_ABI); // includes Execute; see fs.rs

    let map_err = |e: landlock::RulesetError| SandboxError::Landlock(e.to_string());

    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(all_access)
        .map_err(map_err)?
        .create()
        .map_err(map_err)?
        .add_rules(path_beneath_rules(&policy.writable_roots, all_access))
        .map_err(map_err)?;

    let ruleset = match &policy.readable_scope {
        // Mirrors bwrap's own `--ro-bind / /` for the `Full` scope: an
        // explicit root rule, not an omission -- omitting it would leave
        // Landlock's independent enforcement denying *everything* outside
        // `writable_roots`, including `Execute` on bwrap's own binary.
        ReadableScope::Full => {
            let root = PathFd::new("/").map_err(|e| SandboxError::Landlock(e.to_string()))?;
            ruleset
                .add_rule(PathBeneath::new(root, read_access))
                .map_err(map_err)?
        }
        ReadableScope::Roots(roots) => ruleset
            .add_rules(path_beneath_rules(roots, read_access))
            .map_err(map_err)?,
    };

    Ok(ruleset)
}

/// Negotiates the Landlock ABI/enforcement level for `policy` on an
/// isolated, throwaway thread: builds the ruleset, calls
/// `restrict_self()`, reports the result, and lets the thread (and its
/// now-moot restriction) end there. Safe to call from anywhere -- it
/// never touches the calling thread's own state and never spawns a
/// process of its own (see module docs for why that matters).
pub(crate) fn negotiate(policy: &SandboxPolicy) -> Result<LandlockReport, SandboxError> {
    let ruleset = build(policy)?;
    std::thread::spawn(move || -> Result<LandlockReport, SandboxError> {
        let status = ruleset
            .restrict_self()
            .map_err(|e| SandboxError::Landlock(e.to_string()))?;
        Ok(LandlockReport {
            target_abi: TARGET_ABI,
            enforcement: status.ruleset,
        })
    })
    .join()
    .map_err(|_| SandboxError::ThreadPanicked)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::NetworkPolicy;

    fn policy(readable_scope: ReadableScope) -> SandboxPolicy {
        SandboxPolicy {
            writable_roots: vec![std::env::temp_dir()],
            readable_scope,
            network: NetworkPolicy::Disabled,
        }
    }

    #[test]
    fn builds_for_full_scope_without_error() {
        build(&policy(ReadableScope::Full)).expect("Landlock ruleset should build for Full scope");
    }

    #[test]
    fn builds_for_roots_scope_without_error() {
        build(&policy(ReadableScope::Roots(vec![
            "/usr".into(),
            "/etc".into(),
        ])))
        .expect("Landlock ruleset should build for Roots scope");
    }

    #[test]
    fn negotiate_reports_the_target_abi() {
        let report = negotiate(&policy(ReadableScope::Full)).expect("negotiation should not error");
        assert_eq!(report.target_abi, TARGET_ABI);
        // Whether it's downgraded depends on the kernel this test happens
        // to run on -- skip-not-fail, per the spike's test requirements.
        println!(
            "Landlock: target={:?} enforcement={:?} downgraded={}",
            report.target_abi,
            report.enforcement,
            report.is_downgraded()
        );
    }
}
