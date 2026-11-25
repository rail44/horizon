# Sandbox runtime: an external runtime behind a seam

Status: proposal, not implemented. Board item #109 holds the decisions
this design follows; #103 and #107 are the failures that led to it.

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
  `-m` Fence logs `[fence:http] ... ✗ CONNECT 403 example.com`.
  `--fence-log-file <path>` writes that log on the host; the sandboxed side
  also tries to open the path and prints a warning when it cannot — to be
  handled (below).
- Killing Fence's process group ends the whole tree (`bwrap
  --die-with-parent`).
- `/tmp` inside is a private tmpfs; `TMPDIR` is `/tmp` (macOS:
  `/tmp/fence`).
- About 136 ms of overhead per command (`true`), proxies included.

## The seam

`crates/horizon-sandbox` is rewritten around one trait; nothing of its
current contents remains.

```rust
/// What one call may do, independent of how a runtime enforces it.
pub struct Containment {
    pub writable: Vec<PathBuf>,       // trees the command may write
    pub read_only: Vec<PathBuf>,      // paths inside them kept read-only
    pub domains: Vec<String>,         // hosts the proxy lets through
    pub loopback_ports: Vec<u16>,     // host 127.0.0.1 ports it may connect to
    pub mach_services: Vec<String>,   // macOS only
}

pub trait SandboxRuntime: Send + Sync {
    /// Whether this runtime can contain a command on this host now.
    fn probe(&self) -> Result<(), Unavailable>;
    /// The command that runs `script` in `cwd` under `containment`.
    /// `scratch` is a host directory the runtime may use for its own
    /// files (settings, logs); the contained command cannot write it.
    fn contain(&self, script: &str, cwd: &Path, containment: &Containment,
               scratch: &Path) -> Result<Contained, Error>;
}

pub struct Contained {
    pub command: std::process::Command,
    /// Read after the command ends: hosts the proxy refused.
    pub refused_hosts: Box<dyn FnOnce() -> Vec<String> + Send>,
}
```

`FenceRuntime { binary: PathBuf }` implements it: it writes the settings
JSON into `scratch`, runs `fence --settings <file> -m --fence-log-file
<scratch>/fence.log -c <script>`, and parses the `[fence:http]` /
`[fence:socks]` lines of that log for `refused_hosts`. Fence's own command
policy is turned off (`command.useDefaults: false`): what a command may do
is decided by Horizon's gate, not by a second list in the runtime.

The `bash` tool keeps one execution path (`exec/plain.rs`'s process group,
niceness, timeout, process-tree kill, bounded drain, output spill). A
contained call runs `Contained::command` through it; an approved
outside-the-sandbox call runs `bash -c` directly. The Git environment scrub
(`GIT_*` routing variables, `GIT_OPTIONAL_LOCKS=0`) and the shared-cache
`cargo clean` guard move from the sandboxed path to that shared path, so
they apply to both.

Configuration: a `[sandbox]` table names the runtime (`runtime =
"fence"`, the only value) and optionally the binary (`fence = "<path>"`,
default: `fence` on `PATH`). Fence is installed by the person (Homebrew on
macOS, the release binary or `go install` on Linux, which also needs
`bwrap` and `socat`); Horizon does not ship it.

## Per-call containment

| Session | `writable` | `read_only` |
|---|---|---|
| In its own worktree | the worktree, and the metadata `agentd`'s `own_metadata_grants` grants today: the worktree's gitdir, the common dir's `objects`, the session branch's ref and reflog | the worktree gitdir's `config.worktree`, `commondir` and `gitdir` (the common dir's `hooks` and `config` are outside `writable`) |
| In a primary checkout | nothing of the checkout (see Decision 3) | — |
| Read-only (exploration children, consultation stage) | nothing | — |
| Any of the above | plus `[[grants.project]] trees` | — |

`domains` is `[[grants.project]] network` names plus the hosts this call
carries (below). `loopback_ports` comes from `network` `127.0.0.1:<port>`
entries; `mach_services` from `mach_services`. The over-broad tree rule
(`is_overbroad_tree`, refusing `$HOME`, `/`, system roots) moves with the
`[grants]` validation into `horizon-config`.

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
- `domains: [...]`: hosts this call needs beyond the allowlist. A call
  carrying them is reviewed by the judge with the hosts, and they open for
  that call only; nothing is added to the session.

A contained call's result says it ran contained and lists
`refused_hosts`; the `bash` description tells the model what a block looks
like and the two fields above. Nothing infers a block from exit codes or
output.

When `probe` fails (Fence or `bwrap` missing), every `bash` call is
uncontained and asks, as a missing sandbox does today.

## What goes, what changes, what stays

Removed:

- `horizon-sandbox`'s current contents, `horizon-sandbox-runtime`, the
  helper and network-probe binaries, the hudsucker proxy (and with them
  the `nono` and `hudsucker` dependencies).
- Approval kinds `FilesystemDenialRetry`, `DomainDenialRetry`,
  `MachServiceGrant`, `SandboxAreaGrant`, `PrimaryCheckoutRetry`, their
  completion variants, and agentd's `session/completion/retry.rs` folds
  for them; the `request_sandbox_area` tool; the per-session network
  proxy; the denial-detail evidence fields (`denied_filesystem_paths`,
  `ungrantable_filesystem_paths`, `denied_network_routes`, ...); the
  `.horizon-sandbox-tmp` scratch directory.

Changed:

- `exec/sandboxed/*` shrinks to building `Containment` and reading
  `refused_hosts`.
- The recall `denial_count` SQL counts `refused_hosts`.
- Prompts: the `bash` description, the `$TMPDIR` line in `prompt.rs`, the
  exploration role's sandbox sentence.
- `scripts/gate.sh` recognises a contained session by an environment
  variable Horizon sets on contained calls (`HORIZON_SANDBOX=fence`)
  instead of the `TMPDIR` suffix, and the `sandboxed` nextest profile is
  measured again under Fence.

Stays: the gate and judge path, `ApprovalKind::Standard` and
`DomainGrant` (`web_fetch`), the retry machinery used by other tools, the
domain `Allowlist` type (moved out of the proxy crate; `web_fetch` reads
it), `metadata_writable_roots` (fs tools), the plain execution path.

Compatibility:

- Event log: `PersistedSessionContext::filesystem_grants` keeps its serde
  shape (the type moves into `horizon-agent`), so no version bump; lines
  with a removed approval kind decode as `SkippedEvent`.
- ACP: removing variants is not additive, so `HORIZON_ACP_EXT_VERSION`
  goes 10 → 11 with a regenerated artifact, and the change needs a full
  restart.

Design docs this replaces, to be marked superseded when it lands:
`agent-approval-design.md` ("Sandbox architecture"),
`containment-denial-narrow-grants-design.md`,
`macos-containment-denial-reporting-design.md`,
`sandbox-area-grants-design.md`, the perimeter sections of
`sandbox-perimeter-design.md`; partly `agent-tools-design.md` and
`agent-subagent-design.md` (decisions 7, 10, 12).

## Decisions to confirm

1. **Network beyond the allowlist**: per call (`domains` on the call,
   judged with it, Claude Code's auto-mode form) rather than per session.
   Recommended: per call.
2. **Git metadata**: a session's own worktree metadata writable as above,
   so commits on its branch stay inside the sandbox, rather than all of
   `.git` read-only and every commit going outside the sandbox (Codex).
   Recommended: the former. A missing `config.worktree` cannot be made
   read-only by a bind; Horizon creates it empty when the worktree is
   created.
3. **Primary checkout**: contained calls in a primary checkout write
   nothing of it; a write there is an outside-the-sandbox call
   (judge, then person). The session-wide "make the checkout writable"
   approval goes. Recommended: as described; `fs_write`/`fs_edit` keep
   their own primary-checkout gate.
4. **Unix sockets** (`SSH_AUTH_SOCK`, gpg-agent): Fence allows named
   sockets on macOS only. Recommended: drop the grants; `git push` and
   signing need the network or the agent anyway, which is an
   outside-the-sandbox call.

## Verification

- `horizon-sandbox`: `FenceRuntime` against the real binary (skipped where
  Fence is not installed, listed in the `sandboxed` nextest profile):
  writes inside and outside, `read_only` inside `writable`, a commit in a
  linked worktree, an allowed and a refused host with `refused_hosts`,
  cancellation ending the tree, and that a `fence.json` in the workspace is
  not read when `--settings` is given.
- `horizon-agent`: the gate for `outside_sandbox` and `domains`, read-only
  refusal, the unavailable-runtime path.
- The fence log warning: find whether `--fence-log-file` can be kept
  from the sandboxed side, or read the log lines from stderr instead.
- Dogfooding on Linux, and the same scenarios on the owner's Mac from a
  short procedure (install Fence with Homebrew, run the agent's
  `cargo test`, a commit, a refused host).
