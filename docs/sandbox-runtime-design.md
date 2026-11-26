# Sandbox runtime: an external runtime behind a seam

Status: implemented. Board item #109 holds the decisions this design
follows; #103 and #107 are the failures that led to it.

## Goal

The agent's `bash` tool runs contained by an off-the-shelf sandbox
runtime instead of the stack Horizon assembled itself (`horizon-sandbox`
over nono, the seccomp supervisor in `horizon-sandbox-runtime`, the
hudsucker proxy in `horizon-sandbox-proxy`). The runtime sits behind a
seam so another runtime can replace it. The first and only runtime is
[Fence](https://github.com/fencesandbox/fence) (Apache-2.0, a single Go
binary; bubblewrap on Linux, `sandbox-exec` on macOS, its own HTTP and
SOCKS5 proxies per run). The same settings drive both operating systems.

The behaviour around the sandbox follows the form agent tools have
converged on, not the one Horizon built:

| | Converged form (Claude Code, Codex, Cursor) | Horizon today |
|---|---|---|
| Inside the sandbox | runs without asking | same (tier 1) |
| Writable | the workspace and a temporary directory, plus a configured list | same (`[grants]`) |
| Protected inside the workspace | `.git/hooks`, `.git/config` (Codex: all of `.git`) | all of `.git` read-only |
| Network | proxy with a domain allowlist | same |
| A command the sandbox blocked | the model asks to run it again outside the sandbox; a model reviewer (classifier / auto-review) decides, and a person when the reviewer declines | the sandbox reports each denied path or host; Horizon offers a sandboxed retry with exactly those grants |
| Network beyond the allowlist, in auto mode | Claude Code: the model names the hosts on the command; the classifier reviews them with it; they open for that command only | a domain approved for the session |

Sources: [Claude Code sandboxing](https://code.claude.com/docs/en/sandboxing),
[Claude Code permission modes](https://code.claude.com/docs/en/permission-modes),
[Codex approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security),
[Cursor run modes](https://cursor.com/docs/agent/security/run-modes).

The converged failure path needs less from a sandbox: the model must be
able to tell that a command was blocked (the command's own error output,
"Read-only file system" or a proxy `403`, and the hosts the proxy refused),
not a trustworthy report of every denied path. That is what lets an
external runtime replace the supervisor.

## What Fence does, measured on this machine (2026-11-25, Fence 0.1.67, Linux)

- `cargo test` in a fresh worktree passes; `kache` writes `~/.cache/kache`
  when that tree is in `allowWrite`.
- A write outside `allowWrite` fails with `EROFS`; `denyWrite` makes a path
  inside a writable tree read-only.
- With the workspace in `allowWrite`, Fence itself keeps `.git/hooks` and
  `.git/config` beneath it read-only, and shell startup files.
- With a gitdir itself in `allowWrite` (a linked worktree's gitdir lives
  outside the workspace), Fence does **not** protect its `hooks` and
  `config`; Horizon must list them in `denyWrite`. `denyWrite` on a path
  that does not exist protects nothing (it is a read-only bind), so a
  missing protected file needs a placeholder or a different rule.
- With the gitdirs writable and `hooks`/`config` denied, `git commit` in a
  linked worktree succeeds inside the sandbox.
- An allowed host answers through the proxy; another gets `403`, and with
  `-m` Fence prints `[fence:http] ... ✗ CONNECT 403 example.com` (or
  `[fence:socks] ... ✗ CONNECT example.org:443 BLOCKED`) on its stderr.
  (`--fence-log-file` would move those lines to a file, but the sandboxed
  side then tries to open it too and warns when it cannot.)
- Killing Fence's process group ends the whole tree (`bwrap
  --die-with-parent`).
- `/tmp` inside is a private tmpfs; `TMPDIR` is `/tmp` (macOS:
  `/tmp/fence`). On Linux this also hides whatever of the host's `/tmp` is
  not among the writable paths: a write there lands in the private tmpfs,
  not on the host.
- About 136 ms of overhead per command (`true`), proxies included.

## The seam

`crates/horizon-sandbox` holds the seam (`runtime`) and the filesystem
grant types with their checks (`grant`); nothing else of its former
contents remains.

```rust
/// What one call may do, independent of how a runtime enforces it.
pub struct Containment {
    pub writable: Vec<PathBuf>,       // trees the command may write
    pub read_only: Vec<PathBuf>,      // existing paths inside them kept read-only
    pub domains: Vec<String>,         // hosts the proxy lets through
    pub loopback_ports: Vec<u16>,     // host 127.0.0.1 ports it may connect to
    pub mach_services: Vec<String>,   // macOS only
    pub temp_dir: Option<PathBuf>,    // writable, and the call's TMPDIR
}

pub trait SandboxRuntime: Send + Sync + Debug {
    fn name(&self) -> &'static str;
    /// Whether this runtime can contain a command on this host now.
    fn probe(&self) -> Result<(), Unavailable>;
    /// The command that runs `script` with `bash -c` under `containment`.
    /// `scratch` is a host directory for the runtime's own files, which the
    /// contained command cannot write.
    fn contain(&self, script: &str, containment: &Containment, scratch: &Path)
        -> Result<Contained, RuntimeError>;
    /// The hosts the runtime's proxy refused, read from the command's
    /// stderr. Informational: the command can print anything.
    fn refused_hosts(&self, stderr: &str) -> Vec<String>;
}

pub struct Contained {
    pub command: std::process::Command,
    pub keep_alive: Vec<tempfile::TempPath>, // dropped after the command ends
}
```

`FenceRuntime { binary }` writes the settings JSON into `scratch` (the
host's temp directory, `horizon-sandbox/`), runs `fence --settings <file>
-m -c <script>` with `HORIZON_SANDBOX=fence` set, and reads the
`[fence:http]` / `[fence:socks]` lines for `refused_hosts`. Those lines stay
in the output the model sees, as Claude Code names a refused host in a
command's result. Fence's own command policy is off (`command.useDefaults:
false`): what a command may do is decided by Horizon's gate. With
`--settings` given, Fence reads no `fence.json` of the workspace or the
user.

Fence gives each call a private `/tmp` of its own, which ends with the
call. Each session therefore has a temporary directory of its own on the
host (`horizon-sandbox/sessions/<session>` under the host's temp
directory), passed as `temp_dir`: Fence adds it to `allowWrite`, and the
script sets `TMPDIR` to it first, since Fence sets its own. A file a call
leaves there is there for the session's next call; the directory is removed
when the session ends.

`horizon-agent`'s `tools::sandbox` holds the runtime in use and whether it
can contain a call (probed once, and again at each turn's start).
`horizon-agent-acp` sets it at start from `[sandbox] fence` in `config.toml`,
else `fence` on `PATH`, and logs when it is unavailable.

The `bash` tool keeps one execution path (`exec/plain.rs`: process group,
niceness, timeout, process-tree kill, bounded drain, output spill). A
contained call runs `Contained::command` on it; an approved
outside-the-sandbox call runs `bash -c` directly. The wrapper reports the
directory a call ended in on a marked line of its stderr, so the runtime's
lines there are told apart from it. The Git environment scrub (`GIT_*`
routing variables, `GIT_OPTIONAL_LOCKS=0`) and the shared-cache `cargo
clean` guard apply to both.

Fence is installed by the person (Homebrew on macOS, the release binary or
`go install` on Linux, which also needs `bwrap` and `socat`); Horizon does
not ship it.

## Per-call containment

| Session | `writable` | `read_only` |
|---|---|---|
| In its own worktree | the worktree, and what `horizon-agent-host`'s `own_metadata_grants` grants: the worktree's gitdir, the common dir's `objects`, the session branch's ref and reflog | the worktree's `.git` file; the gitdir's `config.worktree` (created empty first), `commondir` and `gitdir` (the common dir's `hooks` and `config` are outside `writable`) |
| In a primary checkout | nothing of the checkout until the session's primary-checkout approval (the one `fs_write`/`fs_edit` ask for there) | — |
| Read-only (exploration children, consultation stage) | nothing | — |
| Any of the above | plus `[[grants.project]] trees` | — |

`domains` is the session's domains (`[[grants.project]] network` names,
plus what a `web_fetch` approval added) and the hosts this call was
approved for (below). `loopback_ports` comes from `network`
`127.0.0.1:<port>` entries; `mach_services` from `mach_services`. The
over-broad tree rule (`is_overbroad_tree`, refusing `$HOME`, `/`, system
roots) checks `[grants]` in `horizon-config` and every grant again before
a call.

## The failure path

`bash` gains two optional input fields, both reviewed by the gate before
the call runs:

- `outside_sandbox: true` with a `reason`: run this call once with the
  host process's authority. Tier policy treats it as uncontained, so the
  judge reviews it (the `Standard` approval for `bash` and the judge's
  host-execution instructions already mean exactly this, today used only
  when no sandbox is available) and a person decides when the judge
  escalates or no judge is configured. Read-only sessions are refused
  without an offer. The model sends it after a contained call failed on
  the sandbox; it is a new call, so no retry machinery is involved.
- `domains: [...]`: hosts this call needs beyond the session's. A call
  carrying ones the session lacks asks (`ApprovalKind::DomainGrant`, the
  judge first), and they open for that call only; nothing is added to the
  session.

A contained call's result says it ran contained and lists
`refused_hosts`; the `bash` description tells the model what a block looks
like and the two fields above. Nothing infers a block from exit codes or
output.

When `probe` fails (Fence or `bwrap` missing), every `bash` call is
uncontained and asks, as a missing sandbox does today.

## What goes, what changes, what stays

Removed:

- `horizon-sandbox`'s former contents (all but the grant types and their
  checks), `horizon-sandbox-runtime`, the helper and network-probe
  binaries, the hudsucker proxy (and with them the `nono` and `hudsucker`
  dependencies).
- Approval kinds `FilesystemDenialRetry`, `DomainDenialRetry`,
  `MachServiceGrant`, `SandboxAreaGrant`, `PrimaryCheckoutRetry`, their
  completion variants, and agentd's `session/completion/retry.rs` folds
  for them; the `request_sandbox_area` tool; the per-session network
  proxy; the denial-detail evidence fields (`denied_filesystem_paths`,
  `ungrantable_filesystem_paths`, `denied_network_routes`, ...); the
  `.horizon-sandbox-tmp` scratch directory.

Changed:

- `exec/sandboxed/*` is gone: `bash::job::containment` builds the
  `Containment`, and the one execution path reads `refused_hosts`.
- `ACP`'s `ApprovalKind` keeps `Standard` and `DomainGrant`.
- `[[grants.project]] mach_services` takes any service name (Fence's
  `macos.mach.lookup`) instead of nono's list.
- The recall `denial_count` SQL counts `refused_hosts`.
- Prompts: the `bash` description, the `$TMPDIR` line in `prompt.rs`, the
  exploration role's sandbox sentence.
- `scripts/gate.sh` recognises a contained session by an environment
  variable Horizon sets on contained calls (`HORIZON_SANDBOX=fence`)
  instead of the `TMPDIR` suffix.

Stays: the gate and judge path, `ApprovalKind::Standard` and
`DomainGrant`, the reissue machinery `web_fetch` uses, the domain
`Allowlist` type (moved into `horizon-agent`'s `tools::network`),
`metadata_writable_roots` (fs tools), the plain execution path.

Compatibility:

- Event log: `PersistedSessionContext::filesystem_grants` keeps its serde
  shape (the type stays in `horizon-sandbox`), so no version bump; lines
  with a removed approval kind decode as `SkippedEvent`.
- ACP: removing variants is not additive, so `HORIZON_ACP_EXT_VERSION`
  goes 10 → 11 with a regenerated artifact, and the change needs a full
  restart.

Design docs this replaces, each marked superseded at its top:
`agent-approval-design.md` ("Sandbox architecture"),
`containment-denial-narrow-grants-design.md`,
`macos-containment-denial-reporting-design.md`,
`sandbox-area-grants-design.md`, the perimeter sections of
`sandbox-perimeter-design.md`; partly `agent-tools-design.md` and
`agent-subagent-design.md` (decisions 7, 10, 12).

## Decisions

The four points this design left open were settled as recommended (board
#109):

1. Network beyond the session's domains is per call (`domains`, judged
   with the call), not per session.
2. A session's own worktree metadata is writable as above, so commits on
   its branch stay inside the sandbox.
3. A contained call in a primary checkout writes nothing of it until the
   session's primary-checkout approval; a host-execution approval does not
   open it.
4. The unix-socket grants (`SSH_AUTH_SOCK`, gpg-agent) are gone: `git push`
   and signing are calls outside the sandbox.

## Verification

- `horizon-sandbox`: `tests/fence.rs` against the real binary (writes
  inside and outside, `read_only` inside `writable`, a refused host named,
  a workspace `fence.json` ignored, the process group ending the command);
  listed in the `sandboxed` nextest profile, as are the tests below that
  start Fence.
- `horizon-agent`: the tier policy for `bash` (contained, `outside_sandbox`,
  `domains`), the read-only refusal, a contained call reporting its
  directory, tier-1 dispatch running contained.
- `horizon-agent-host` and `horizon-agent-acp`: a session committing in its own worktree under Fence
  and moving no other branch; the e2e approval of an outside-the-sandbox
  call.
- Dogfooding on Linux, and the same scenarios on the owner's Mac (install
  Fence with Homebrew, set `[sandbox] fence` if the app does not see it on
  `PATH`, run the agent's `cargo test`, a commit, a refused host).
