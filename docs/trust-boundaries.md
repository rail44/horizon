# Trust Boundaries and Runtime Placement

Decision record, 2026-07. Context: the product direction (agents and
terminals as equal first-class objects; the agent mechanism as a reusable
asset; price-model ownership) plus a stack/runtime/principles audit of the
codebase. This file records where code runs and why, and the stances taken
on the major dependencies. Update it when a decision here is revisited.

## Three tiers, chosen by trust — not by one technology

**1. Untrusted code (agent-authored and third-party plugins) → wasm.**
Agent-written plugins are untrusted by construction: even a well-behaved
agent writes bugs, and a prompt-injected one writes malice. Wasm provides
memory isolation plus a capability-based boundary — a module cannot call
what the host does not hand it — and one artifact runs on both Linux and
macOS. Instantiate/drop also gives safe hot reload. Consequences:

- `wasmtime` stays in the tree as a strategic placeholder even while only
  validation is wired. Do not swap it for a validate-only parser.
- When Phase 7 starts executing plugins, move the pin to a wasmtime LTS
  line (the current pin is a non-LTS release outside its patch window).

**2. Trusted but fast-churning code (the agent mechanism) → keep it behind
the provider contract seam; in-process today, process boundary decided as
the next architecture milestone (2026-07-04).** The agent mechanism
(contract, providers, tools, persistence) is intended as a reusable asset
that does not require Horizon as its frontend. The contract is already
message-shaped and the event log is the source of truth, so moving the
runtime to a child process is a placement change, not a redesign — restart
the child and replay from the log, and agent-code hot reload falls out for
free (the daily-driver dogfooding requirement). An ACP adapter would let
any frontend drive the same runtime. Near-term discipline: no
Horizon-specific types leak into the agent core (Horizon-coupled tools like
`workspace.snapshot` stay pluggable catalog entries). Future enforcement:
split the mechanism into its own crate when convenient.

Dylib-based hot reload was considered and rejected for this tier: Rust has
no stable ABI, unloading is unsound in practice, and a crash is not
isolated — the worst fit for the fastest-changing component.

**3. Trusted hot-path code (terminal emulation) → native, in one process.**
PTY output interpretation and grid rendering are latency- and
bandwidth-sensitive; a wasm boundary inside that path buys little and costs
a protocol. Revised 2026-07-04: an earlier version of this entry also
dismissed a *process* boundary — that dismissal does not survive the
daily-driver requirement. Sessions must outlive UI restarts, which points
at a tmux-style split: a session daemon owns PTYs (and agent runtimes), a
freely-restartable UI client renders. That is a session-survival argument,
not a hot-reload one, and it is the long-term shape to design together
with the delegation milestone (the daemon is also the natural home for
delegated agent sessions). Until then: UI iteration happens in a dev
instance nested inside the stable one.

## Dependency stances (from the 2026-07 audit)

- **rig-core** — pinned at 0.39, used as a thin typed-payload/streaming
  adapter behind the contract; its agent loop and tool system are
  deliberately bypassed. Decision point: when the second provider is wired,
  choose between rig's provider breadth and a direct OpenAI-compatible
  client. Until then, do not chase rig releases.
- **duckdb** — kept, as the agent-knowledge-base bet. JSONL is the durable
  log; the DuckDB projection is rebuildable and opt-in, so the exit cost
  stays small. Note: `DUCKDB_DOWNLOAD_LIB` is a no-op under the `bundled`
  feature (verified against libduckdb-sys 1.10504.0); relieving build cost
  would require the non-bundled path and is not currently worth it.
- **alacritty_terminal + termwiz + portable-pty** — a deliberate three-way
  split: output interpretation / keyboard input encoding (incl. the kitty
  protocol; alacritty_terminal exposes no input-encoding API) / PTY and
  process I/O. Not redundant; keep.
- **floem** — moved (2026-07-04) from crates.io 0.2.0 to Lapce's git pin
  (`31fa8f44`), the only rev battle-tested by a production app; strategy is
  to bump when Lapce bumps and never track `main`. Compile-side cost was
  small (71 errors, two mechanical categories); glyph rendering is
  pixel-identical (that rev still uses cosmic-text, not Parley). **Known
  accepted regression:** for ~0.3-0.5s after the window appears, all input
  is silently dropped (absent on 0.2.0; likely the windowing-layer switch
  from the floem-winit fork to a direct winit rev). Accepted by the owner
  for a solo daily driver; the headless verification scripts compensate
  with a post-window settle delay. Re-check at each Lapce bump whether the
  window disappears; a reproducible 5/5 bisection exists if we choose to
  report it upstream.
