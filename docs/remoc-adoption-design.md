# Session Wire — remoc Adoption Design

Status: adoption decided 2026-07-20 (owner decision, following the IPC
survey and the measured spike); **migration complete 2026-07-21** — all
four staged PRs landed (§6) and the wire is remoc at protocol v11. This
document records the decision, the target architecture, and the migration
boundaries. Measurements and skew experiments live in
`docs/research/remoc-spike-2026-07-20.md` (spike code preserved on the
`remoc-spike` branch, PR #19, not for merge). Companions:
`docs/session-daemon-design.md` (what the daemon split decided) and
`docs/terminal-protocol-goals.md` (where the frame path is headed) — §8
lists exactly which of their decisions this document supersedes or
inherits.

One section required an explicit owner call before implementation:
**§5, frame delivery** (watch-of-full-frames vs mpsc-of-diffs) — resolved
2026-07-20 in favour of Option A (full-frame watch), shipped as protocol
v11. Everything else was settled by the adoption decision plus this
document.

## 1. Decision record

### Why remoc

Horizon's UI ⇄ `horizon-agentd` IPC is hand-rolled: a JSONL envelope
(`horizon-session-protocol`), string `kind` dispatch onto sister
vocabularies, request-id correlation maps for the few call-shaped
exchanges, and per-connection frame baselines. Every new exchange re-pays
the same wiring tax, and the exact-version handshake makes every wire
change a hard break (nine bumps in under two weeks).

The 2026-07-20 consult surveyed six IPC/RPC candidates, narrowed to
[remoc](https://github.com/ENQT-GmbH/remoc) (0.18.x) as the only one
offering typed RPC *plus* forwardable typed channels over one connection,
spiked it against real `TerminalFrame` payloads, and then swept nine
similar crates to confirm no better-maintained equivalent exists. The
spike's verdict (research doc, results transcribed from runs):

- **Performance is not a blocker, but remoc is not a speedup.** With its
  default Postbag (Full) codec, e2e throughput tops out at ~1/4 of raw
  JSONL (2,023 fps @ 80x24; 361 fps @ 200x50 full-styled frames) — still
  6x headroom over the 16 ms/60 fps requirement in the worst measured
  case, with codec CPU at ~19% of one core. Wire bytes are ~26% smaller
  than JSON; chmux multiplexing overhead is noise (tens of bytes/frame).
- **Skew tolerance matches or beats today's serde_json posture.** Added
  fields are ignored, missing fields take `#[serde(default)]`, unknown
  enum variants decode to a `#[serde(other)]` variant (verified working
  under Postbag), and a single undecodable item errors only that one
  `recv` — the channel survives.
- **remoc's own wire (chmux) is stable in practice**: one protocol change
  since 2021, and it was backward compatible. The real hazard is
  *default-value faults* — 0.18.0 silently changed the default codec
  (JSON → Postbag), which is a cross-version connection killer if either
  end relies on the default.
- **The ergonomic win is the point**: `#[rtc::remote]` traits returning
  structs that carry live `rch` channels collapse the attach/input/frames
  wiring and give uniform, immediately-detectable disconnect behavior on
  every channel.

### Adoption conditions (binding)

The adoption is conditional on three disciplines, each traceable to a
spike finding:

1. **The codec is explicitly pinned, never defaulted.** Server and client
   construction name the codec type (`codec::Postbag`, not
   `codec::Default`), and default-codec cargo features are disabled so a
   remoc upgrade that changes the default fails to compile instead of
   silently forking the wire. This is the direct countermeasure to the
   0.18.0 default-codec fault. v10 pins **Postbag (Full)** — the
   configuration the skew experiments validated; §5 records the cheaper
   codecs as frame-path fallbacks, not defaults.
2. **Every wire enum carries a `#[serde(other)]` `Unknown` variant**, and
   receive loops treat a deserialization error as "skip this item", never
   "tear down the channel". Unknown unit variants lose their payload;
   "an unknown command/update is ignored" is the intended semantic.
3. **`Connect::io` is polled on both ends concurrently.** The connect
   future must be spawned and driven alongside channel use (sequentially
   awaiting one side deadlocks and presents as a 60 s `ChMux(Timeout)`);
   in-process test harnesses hosting both endpoints must join both
   handshakes.

### remoc version policy and bus factor

remoc is pinned to an exact version (`=0.x.y`) once, workspace-wide; UI
and daemon are always built from the same workspace, so a remoc bump is
by construction a both-ends-at-once event — the same operational shape as
today's wire-version bump, minus the negotiation (§3) that now absorbs
it. A 0.x bump is reviewed against the remoc CHANGELOG for chmux/codec
changes before landing.

The bus-factor risk is accepted with eyes open: remoc has a small
maintainer set and no large ecosystem behind it. Mitigations: chmux has
been wire-stable since 2021; Horizon's usage surface is narrow (rtc
traits, `rch::mpsc`/`watch`, one codec); and in the worst case — 
abandonment plus a blocking defect — the owner accepts maintaining a
fork, the same posture already taken for `portable-pty`
(`docs/roadmap.md`, backlog 28/31). The exit cost is bounded by keeping
domain vocabularies serde-plain (they never depend on remoc types), so a
transport re-swap would strand only the hub trait and channel plumbing.

## 2. Architecture

### The hub trait

One `#[rtc::remote]` trait replaces the envelope protocol. Sketch (shapes
are illustrative; exact signatures are implementation latitude — the
*structure* is the decision):

```rust
#[rtc::remote]
pub trait SessionHub {
    /// Version negotiation, §3. The first call on every connection.
    async fn hello(&self, client: ClientHello) -> Result<HubHello, HubError>;

    // -- terminal domain --
    async fn list_terminals(&self) -> Result<Vec<TerminalSummary>, HubError>;
    async fn create_terminal(&mut self, spec: TerminalSpawnSpec)
        -> Result<(Uuid, TerminalAttachment), HubError>;
    async fn attach_terminal(&mut self, session_id: Uuid)
        -> Result<TerminalAttachment, AttachError>;

    // -- agent domain --
    async fn list_agents(&self) -> Result<Vec<SessionSummary>, HubError>;
    async fn new_agent(&mut self, new: SessionNew)
        -> Result<AgentAttachment, HubError>;
    async fn attach_agent(&mut self, session_id: SessionId)
        -> Result<AgentAttachment, HubError>;

    /// Flush-and-exit, replacing `SessionControl::Drain`.
    async fn drain(&mut self) -> Result<(), HubError>;
}

pub struct TerminalAttachment {
    /// §5. Option A: a snapshot-valued signal; the receiver's current
    /// value IS the resync anchor.
    pub frames: rch::watch::Receiver<TerminalFrame>,
    /// Title/Bell/Clipboard/Exited — the non-frame updates.
    pub events: rch::mpsc::Receiver<TerminalEvent>,
    pub commands: rch::mpsc::Sender<TerminalCommand>,
}

pub struct AgentAttachment {
    pub events: rch::mpsc::Receiver<AgentWireEvent>,
    pub commands: rch::mpsc::Sender<Command>,
}
```

Daemon→client exchanges that are connection-global today
(`HostToolRequest`/`HostToolResponse`, `SkippedLines`) ride channels
handed over in `HubHello`; session-scoped announcements
(`WorkspaceRootResolved`, `SessionModel`, `ToolCallProgress`) ride the
agent attachment's event channel. `TerminalUpdate::Error` disappears as a
variant: transport failure is now every channel's own error result
(spike §4 — watch, mpsc, and rtc calls all report mux termination
promptly and distinguishably).

### What maps where

| Today | After |
|---|---|
| `Envelope { v, session_id, kind, payload }` | gone — rtc method + channel identity |
| `kind` string dispatch (`routing.rs`, daemon `main.rs` hosting loop) | gone — trait method dispatch |
| `request_id` correlation maps (`pending_terminal_lists`/`attaches`, `pending_session_list`) | gone — rtc calls return futures |
| per-message `session_id` correlation | attach-call argument only; afterwards structural (channel identity) |
| JSONL framing (`read_envelope`/`write_envelope`, `TornLine`) | gone — chmux framing; a minimal legacy *encoder* survived inside the drain prober until that prober was retired on 2026-08-01 (§6) |
| exact-version `Hello` / `HandshakeRejected` | `hello()` range negotiation (§3) |
| `SessionControl::Ping/Pong` | gone — channel-level disconnect detection subsumes it; a liveness probe, if ever wanted, is an rtc call |
| connection-loss fan-out (`Routes::connection_failed`) | per-channel error results |
| sister vocabularies (`TerminalCommand`, agent `Command`/`Event`, …) | **kept as-is**, serde-plain, remoc-free |
| socket discovery, `connect_or_spawn_agentd_retrying`, drain semantics | kept |
| 16 ms coalescing, daemon-retained latest frame | kept (transport-independent) |

### Crate shape and dependency direction

`horizon-session-protocol` stays the neutral protocol crate but inverts
its dependency direction: today the domain crates depend on it (for
`Envelope` helpers); after the cutover it depends on *them* (the hub
trait's signatures name `TerminalSpawnSpec` and `SessionNew`) plus remoc,
and the domain crates lose their `encode_*`/`decode_*` wire helpers
entirely. Decision 3 of `docs/session-daemon-design.md` (sister
contracts, no union) is preserved: the vocabularies stay in their own
crates and never reference each other; the hub trait is the one place
that names both — exactly the "thin shared layer" that decision already
allowed.

`src/runtime/` keeps its public shape (`AgentdHandle`'s sync API,
eager non-blocking start, one dedicated runtime thread); internally the
envelope FIFO and `Routes` registry become rtc calls and per-attachment
channel bridges. The sync-world ⇄ tokio boundary does not move.

## 3. Version negotiation

The exact-match handshake is replaced by range negotiation, carried as
the first rtc call on top of the established remoc connection:

- `ClientHello { min_supported: u32, current: u32, binary_id: String }`.
- The daemon intersects the client's `[min, current]` with its own and
  replies `HubHello { negotiated: u32, binary_id, … }` at the highest
  mutually supported version, or an explicit rejection error naming both
  ranges (which feeds the same auto-drain recovery as today's
  `HandshakeRejected`, §6).

`SESSION_PROTOCOL_VERSION` survives with its meaning shifted, then
narrowed further by the standing **lockstep, no per-feature gates**
policy (owner, 2026-07-30; hardened 2026-08-03 — this project carries no
wire decode compatibility by default): `min_supported` always equals
`current` for both hubs now (`AGENT_PROTOCOL_VERSION`/
`MIN_SUPPORTED_AGENT_PROTOCOL_VERSION` in
`crates/horizon-agent/src/wire/hub.rs`, the terminal pair in
`crates/horizon-terminal-core/src/wire.rs`). A same-machine self-spawned
daemon and its UI are always the same build, so a mismatched `hello` — a
stale daemon that hasn't picked up a rebuild yet — is rejected and
recovered by auto-drain-and-respawn (§6) rather than bridged by a
tolerated range. The version still bumps only on a deliberate wire
reshape (§4's checker still tells additive from reshape, for exactly this
reason): a purely additive change needs no bump, so a stale-but-current
daemon can keep serving right through it without an unnecessary
drain — the property `horizon-terminald`'s rarely-restarted PTYs
particularly depend on.

This replaces a regime of nine bumps in two weeks (v1–v9, each a hard
"reload required") with one where a routine vocabulary addition ships
with no version event at all.

## 4. Skew discipline

Tolerant evolution only works if reshapes are actually impossible to land
by accident. Rules, then enforcement:

1. **Additive only.** New struct fields carry `#[serde(default)]`; new
   enum variants are appended. Renaming, reordering, retyping, or
   removing anything wire-visible is a semantic break requiring a version
   bump (§3) and an owner decision — the expectation is that this
   essentially never happens again (v5's color-vocabulary reshape was the
   kind of change that now lands as a parallel additive field instead).
   Under the lockstep policy (§3) this is no longer about tolerating a
   *foreign* peer — there is no other build to tolerate — it is about not
   forcing a stale-but-current daemon through an unnecessary
   drain-and-respawn for a change it can already decode.
2. **The schema is a committed artifact, checked mechanically.** Every
   wire-visible type derives `schemars::JsonSchema`; a generator writes
   one canonical schema file per runtime
   (`crates/horizon-agent/schema/agent-wire.json`,
   `crates/horizon-terminal-core/schema/terminal-wire.json` — one union
   file until `docs/runtime-crate-alignment-design.md` phase 2) which is
   committed. Two checks enforce it:
   - a nextest test regenerates the schema and fails on any drift from
     the committed artifact — so every wire change is visible, reviewable
     text in its PR diff, and forgetting to regenerate is a red test;
   - a checker script (pre-commit / quality gate) diffs the artifact
     against the merge-base's copy and classifies every change as
     *additive* (new optional field, new trailing variant, new method —
     pass) or *reshape* (anything else — fail without an explicit
     version-bump marker in the same change).
   postcard-rpc's `Key` mechanism — a content hash of each endpoint's
   schema, compared at connect time — is the reference implementation for
   the idea; Horizon needs the comparison at *merge* time, not connect
   time, so a repo artifact plus a merge-base diff is the same guarantee
   applied earlier.
3. This checker **replaces the four `CONTRACT_VERSION` pin tests** that
   used to live in `crates/horizon-agent/src/wire.rs` (`contract_version_*`):
   their job — forcing a human decision on every wire-shape change — is
   exactly what the artifact diff does, with structure instead of a
   hand-maintained integer assertion.
4. **Postbag positional discipline** (added 2026-07-21, from the v10
   review's measurements). Postbag is not self-describing, which makes
   enum placement wire-meaningful:
   - Wire enums may appear only in **struct-field, top-level, or newtype
     positions** — never as `Vec`/tuple/array *element* types. A
     misaligned read in element position can silently misdecode
     neighbouring elements.
   - The receive-side floor is a **per-item decode error, never a panic
     or a torn channel**: an unrecognized identifier and a *known*
     identifier with a structurally broken payload (missing required
     field) both surface as the same non-final `recv` error, which every
     receive loop skips (this project carries no wire decode
     compatibility by default — owner decision 2026-08-03 — so there is
     no `Unknown` catch-all left to degrade an unrecognized identifier
     into; it is corruption to skip past, not a peer to tolerate).
     **Corruption detection must not be expected beyond that**: a
     type-level mismatch can silently misdecode (measured: a `String`
     payload read into a `u16` field as the string's length varint) —
     which is why retyping a wire field is a version-bump reshape, never
     an in-place change.

## 5. Frame delivery — the owner decision (resolved 2026-07-20)

The largest design fork the migration exposed, stated explicitly because
it needed an owner call before implementation — resolved in favour of
Option A (full-frame watch) and shipped as protocol v11 (§6 phase 3); the
Decision note below records the call. The two options, for the record:

**Option A — `rch::watch<TerminalFrame>`, every delivery a full frame.**
The frame channel becomes a snapshot-valued signal. Consequences:

- The wire diff machinery is deleted wholesale: `TerminalFrameDiff`,
  `TerminalRowDiff`, `apply_frame_diff`, the daemon's per-connection
  baseline map (`ClientConnection::baselines`), the
  Snapshot-vs-FrameDiff branching on both ends, and the
  attach/reconnect "establish a baseline first" dance. Resync becomes
  structural: the watch receiver's current value *is* the latest frame,
  at every moment, for every subscriber.
- Backpressure disappears as a design problem: watch's latest-value
  semantics (spike §1c — a slow reader observes a skipping sequence but
  always converges on the final value) are exactly the right policy for
  a screen, with no queue to bound.
- Diffs are stateful, and statefulness composes badly with §4's
  tolerant decoding — this is the decisive argument, not performance. A
  skewed peer degrades what it reads (an `Unknown` span attribute, a
  defaulted field); under snapshot⊕diff that degradation is baked into
  the receiver's baseline, every subsequent diff extends the divergence
  between what the daemon believes the row holds and what the UI holds,
  and the drift survives until the next full snapshot. The diff
  contract implicitly assumes both ends share identical frame
  semantics; v10's skew regime explicitly abandons that assumption.
  Under watch, every delivery is the complete truth: a degraded decode
  lasts exactly one frame and self-heals on the next.
- Measured headroom says full frames are affordable: 361 fps at 200x50
  full-styled frames (the pathological all-rows-change case) against a
  60 fps ceiling; same-host unix socket, so bandwidth is not a scarce
  resource (`docs/terminal-protocol-goals.md` non-goals: remote domains
  out of scope).
- Cost, stated honestly: `changed_rows` stops arriving on the wire, so
  the UI's row-generation table (the ShapedLine cache's invalidation
  signal, PR #13) is fed by a client-side row comparison of consecutive
  frames instead — the same `TerminalLine` `PartialEq` the daemon runs
  today, at a cost already measured negligible. And watch does not save
  wire bytes versus sending every frame on mpsc — it saves receiver
  work only.

**Option B — keep diffs, on `rch::mpsc`.** Minimal bandwidth, but the
entire baseline/diff/resync complexity survives the migration untouched,
and backpressure policy (queue depth, coalescing interaction) remains
Horizon's to own.

**Decision (2026-07-20): the owner ratified Option A** for the current
stage, after the statefulness-vs-tolerant-decoding argument above was
made explicit. The recorded reopening conditions stand: a remote-domain
goal or measured multi-pane aggregate encode cost would revisit this in
Option B's favor.

**The delivered design uses Option A.** The daemon keeps its retained
latest frame (it seeds the watch and serves attach) and the 16 ms
coalescing (which bounds full-frame production rate); everything else in
the diff pipeline is deleted. If bandwidth ever becomes real — a remote
domain, a many-subscriber future — the recorded fallbacks are scoped to
the frame channel alone and do not reopen the architecture: **PostbagSlim**
(measured: wire 1/8 of JSON at JSON-par CPU; evolution restricted to
tail-append, acceptable for a single quarantined, rarely-evolving type
behind its own negotiated gate) and an **Arrow-style columnar layout**
for span data (investigated as an optimization seed; same quarantine).
Postbag Full's numeric-field-id rename (`_0`…) is the milder middle step
for codec CPU. None of these are v10 scope.

**The latest-only property must survive the local UI boundary.** A
2026-07-22 split-pane profile found that the remoc watch was immediately
copied into two unbounded FIFO channels inside the client runtime. A busy
terminal therefore stopped producing frames promptly, but the GPUI thread
continued replaying obsolete snapshots for several seconds while also
rendering the adjacent agent transcript. The local runtime-to-GPUI frame
route now remains `tokio::sync::watch<TerminalFrame>` and the GPUI task
borrows only its latest changed value. Non-frame `TerminalUpdate`s retain
their ordered FIFO route: exit, clipboard, title, and scroll-window events
are events rather than replaceable snapshots. This is part of Option A's
backpressure contract, not a UI-only optimization.

Live verification on the corrected split-pane build showed terminal traffic
ending at 140.1 seconds after startup and the frame loop falling from the
preceding 20-plus draws per second to its ordinary 2-3 periodic redraws per
second by 144.6 seconds. The pre-fix capture had continued replaying terminal
work for roughly eight seconds after output stopped. The remaining periodic
redraws are separately attributed wake/animation activity rather than queued
terminal frames.

## 6. Migration plan (completed)

Hard cutover in four PRs, all landed 2026-07-21, no dual-stack daemon
(pre-release, single-owner project): skew groundwork on the live JSONL
wire, the remoc cutover itself (v10, hub trait + Postbag, `hello` range
negotiation), the frame path (Option A, §5, protocol v11), then cleanup.
The one transition hazard — a v10 UI meeting a still-running pre-remoc
JSONL daemon, or the reverse — was bridged by a quarantined legacy JSONL
prober/encoder, itself retired once no such daemon could plausibly still
be running. The event log's on-disk format was out of scope throughout —
independent of the wire, a daemon-local persistence concern.

## 7. Test strategy

- **Real-socket e2e stays the house style.** The
  `crates/horizon-agentd/tests/e2e.rs` approach — spawn the actual
  daemon binary, talk over the actual unix socket — ports to remoc
  clients and remains the proof for attach/reconnect, PTY survival, cwd
  resolution, and drain. The `Connect::io` both-ends rule (adoption
  condition 3) applies to any in-process harness.
- **Wire-decode robustness tests are permanent residents.** The spike's
  V1/V2 type-pair method (`remoc-spike:spike/remoc/tests/skew.rs`) was
  promoted from spike code to a standing test module,
  `crates/horizon-agent/tests/skew.rs`; since the 2026-08-03 compat sweep
  removed cross-build decode compatibility, what it proves narrowed to
  corruption robustness alone — a structurally broken item is a per-item
  decode error, never a panic or a torn channel, over the actual wire
  codec.
- **Mismatch recovery keeps its e2e coverage** across the generation gap:
  a v10 client against a real JSONL-daemon fixture must probe, drain,
  respawn, and connect — the #18 scenarios re-anchored on the new
  detection path.
- The frame-path benchmarks are not made CI gates (no CI; runtime
  variance would make them flaky as tests). The spike's bench binary and
  its numbers remain the recorded baseline; re-measure on codec or
  frame-path changes, per `docs/terminal-protocol-goals.md` goal 4's
  "verified, not asserted".

## 8. Relation to prior decisions

Superseded (by this document, or by §5 Option A, ratified 2026-07-20):

- `docs/session-daemon-design.md` decision 4, "row-diff push; full
  snapshot on attach" — Option A replaces both halves with the
  snapshot-valued watch (the 16 ms rate control it names survives).
  Under Option B only its transport changes.
- `docs/session-daemon-design.md` step-1 note "neutral shared framing
  crate" — the crate survives, the framing does not; dependency
  direction inverts (§2).
- `docs/terminal-protocol-goals.md` goal 1's *letter* ("declarative
  snapshot ⊕ row replacement") under Option A — its *intent* (any client
  state recoverable by one snapshot, O(1) resync, no stateful command
  streams) is strengthened: every delivery is the resync anchor.
- `docs/terminal-protocol-goals.md` goal 3's wire half ("`changed_rows`
  reaches the view layer") under Option A — change information is
  re-derived client-side at measured-negligible cost; the view-layer
  half (generations drive cache invalidation, correctness never depends
  on diffs) stands.
- The exact-version handshake and its "reload required" fatal (§3).
- `crates/horizon-agent/src/wire.rs`'s four `CONTRACT_VERSION` pin tests
  (§4).

Inherited unchanged:

- Sister contracts, no union vocabulary (decision 3) — the hub trait is
  the allowed thin shared layer.
- Daemon owns PTY + emulation; UI renders frames (decisions 1, 8, 9);
  logical colors on the wire.
- One client connection with the `client_id`/multi-subscriber hedge
  (decision 6) — rtc attach calls make future fan-out additive.
- Socket discovery, spawn-on-demand, explicit destructive
  `Reload Agent Runtime`, drain semantics.
- PR #18's mismatch auto-recovery decisions, extended across the
  transport generation (§6).
- The event log's format and forward-compat guard (out of scope, §6).

## References

- `docs/research/remoc-spike-2026-07-20.md` — all measurements cited
  here; reproduction commands; spike code on branch `remoc-spike`
  (PR #19).
- `docs/session-daemon-design.md`, `docs/terminal-protocol-goals.md` — 
  the decisions §8 maps.
- `crates/horizon-agent/src/wire/hub.rs`,
  `crates/horizon-terminal-core/src/wire.rs` (the two remoc hubs and the
  wire policy; one union crate until
  `docs/runtime-crate-alignment-design.md` phase 2),
  `crates/horizon-wire/` (the domain-free foundation),
  `src/runtime/` (remoc client runtime),
  `crates/horizon-agentd/src/terminal.rs` (full-frame watch publisher),
  `crates/horizon-terminal-core/src/types/frame.rs` (snapshot type),
  `crates/horizon-agent/src/wire.rs` (agent vocabulary).
- remoc CHANGELOG (chmux v2→v3 compatibility note; 0.18.0 default-codec
  change), postcard-rpc's `Key` schema-hash design (§4's reference
  implementation).
