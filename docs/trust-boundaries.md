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
the provider contract seam; in-process today, process boundary available
later.** The agent mechanism (contract, providers, tools, persistence) is
intended as a reusable asset that does not require Horizon as its frontend.
The contract is already message-shaped and the event log is the source of
truth, so moving the runtime out of process later (restart + replay) is a
placement change, not a redesign — and an ACP adapter would let any
frontend drive the same runtime. Near-term discipline: no Horizon-specific
types leak into the agent core (Horizon-coupled tools like
`workspace.snapshot` stay pluggable catalog entries). Future enforcement:
split the mechanism into its own crate when convenient.

Dylib-based hot reload was considered and rejected for this tier: Rust has
no stable ABI, unloading is unsound in practice, and a crash is not
isolated — the worst fit for the fastest-changing component.

**3. Trusted hot-path code (terminal emulation) → in-process native.**
PTY output interpretation and grid rendering are latency- and
bandwidth-sensitive; pushing frames across a wasm or process boundary buys
little and costs a protocol. Iterate via normal rebuilds.

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
- **floem** — currently pinned to crates.io 0.2.0. A migration spike to
  Lapce's git pin measured the compile-level cost as small (72 errors in
  three mechanical categories). The move decision is pending the runtime
  findings of the evaluation spike; record it here when made.
