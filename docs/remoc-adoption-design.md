# Session Wire — remoc Adoption Design

Status: adoption decided 2026-07-20 (owner decision, following the IPC
survey and the measured spike). This document records the decision, the
target architecture, and the migration boundaries; implementation has not
started. Measurements and skew experiments live in
`docs/research/remoc-spike-2026-07-20.md` (spike code preserved on the
`remoc-spike` branch, PR #19, not for merge). Companions:
`docs/session-daemon-design.md` (what the daemon split decided) and
`docs/terminal-protocol-goals.md` (where the frame path is headed) — §8
lists exactly which of their decisions this document supersedes or
inherits.

One section requires an explicit owner call before implementation starts:
**§5, frame delivery** (watch-of-full-frames vs mpsc-of-diffs). Everything
else is settled by the adoption decision plus this document.

## 1. Decision record

### Why remoc

Horizon's UI ⇄ `horizon-sessiond` IPC is hand-rolled: a JSONL envelope
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
| JSONL framing (`read_envelope`/`write_envelope`, `TornLine`) | gone — chmux framing; a minimal legacy *encoder* survives inside the drain prober (§6) |
| exact-version `Hello` / `HandshakeRejected` | `hello()` range negotiation (§3) |
| `SessionControl::Ping/Pong` | gone — channel-level disconnect detection subsumes it; a liveness probe, if ever wanted, is an rtc call |
| connection-loss fan-out (`Routes::connection_failed`) | per-channel error results |
| sister vocabularies (`TerminalCommand`, agent `Command`/`Event`, …) | **kept as-is**, serde-plain, remoc-free |
| socket discovery, `connect_or_spawn_retrying`, drain semantics | kept |
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

`src/sessiond/` keeps its public shape (`SessiondHandle`'s sync API,
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

`SESSION_PROTOCOL_VERSION` survives with its meaning shifted:

- **The remoc cutover is v10.** v10's "wire shape" is the hub trait +
  Postbag-encoded vocabularies; a v10 peer cannot talk to a v≤9 JSONL
  peer at all (detected and recovered per §6, not negotiated).
- **From v10 on, the version bumps only on a deliberate semantic break,
  which the skew discipline (§4) makes rare-to-never.** Additive
  evolution — new fields with defaults, new `Unknown`-guarded variants,
  new trait methods — needs no bump: old and new peers already tolerate
  each other. The negotiated version exists to gate *behavior* ("may I
  rely on the peer honoring X?"), not decodability; `min_supported` rises
  only when carrying compatibility code for ancient peers stops being
  worth it.

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
2. **`#[serde(other)] Unknown` on every wire enum**, receive loops skip
   undecodable items (adoption condition 2).
3. **The schema is a committed artifact, checked mechanically.** Every
   wire-visible type derives `schemars::JsonSchema`; a generator writes
   one canonical schema file (e.g.
   `crates/horizon-session-protocol/schema/session-wire.json`) which is
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
4. This checker **replaces the four `CONTRACT_VERSION` pin tests** in
   `crates/horizon-agent/src/wire.rs` (`contract_version_*`): their job —
   forcing a human decision on every wire-shape change — is exactly what
   the artifact diff does, with structure instead of a hand-maintained
   integer assertion.

## 5. Frame delivery — the one open owner decision

The largest design fork the migration exposes, stated explicitly because
it needs an owner call before implementation:

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

**This document recommends Option A.** The daemon keeps its retained
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

## 6. Migration plan

Hard cutover, no dual-stack daemon: this is a pre-release, single-owner
project (the same posture as the `agentd`→`sessiond` rename — 
`docs/session-daemon-design.md`, "Hard rename, no migration compatibility
layer"). The daemon speaks only remoc from v10 on; what needs care is the
*transition moment*, where a v10 UI meets a still-running v≤9 daemon.

**The legacy JSONL drain prober outlives JSONL.** PR #18's
contract-mismatch auto-recovery (drain the stale daemon at *its own*
envelope version, probing v9 down to v3 when it never revealed one, then
let `connect_or_spawn_retrying` start a fresh binary) is retained after
the cutover as the **only** path by which a remoc-generation UI can
automatically clear a JSONL-generation daemon: a v10 client's chmux
handshake gets no valid reply from a JSONL daemon (detected by a bounded
timeout wrapped around the connect — never the raw 60 s chmux timeout),
which triggers the same probe-drain-respawn sequence, once per runtime,
fatal-with-rebuild-hint on the second failure exactly as #18 decided. To
serve it, a small legacy JSONL *encoder* (versioned `session_control`
envelope + line framing) survives, quarantined in one module whose only
caller is the prober. A wrong-generation probe hitting a healthy remoc
daemon costs one garbage connection, which chmux drops — the same
"wrong probe is harmless" property #18 established.

**Out of scope: the event log's on-disk format.** It is independent of
the wire (a daemon-local persistence concern), already carries its own
forward-compatibility guard, and does not change in this migration.

Staged PR sequence, each independently green:

1. **Skew groundwork on the live JSONL wire (no v-bump).** Add
   `#[serde(other)] Unknown` variants and the `#[serde(default)]` audit
   across the wire vocabularies, `schemars` derives, the committed schema
   artifact, and the §4 checker (retiring the four pin tests). All
   additive on v9; lands value even before remoc does.
2. **The cutover (v10).** The hub trait in `horizon-session-protocol`,
   remoc pinned (exact version, explicit Postbag codec, default features
   off), daemon serves the hub, `src/sessiond/` client rebuilt on rtc
   calls + channel bridges, envelope/kind-dispatch/correlation code
   deleted, `hello` range negotiation, the legacy prober rewired behind
   the chmux-timeout detection. Frame delivery ships **unchanged in
   semantics** here — Snapshot/FrameDiff updates travel verbatim over the
   attachment's mpsc channel — so this PR is a transport swap, reviewable
   as such. e2e suite ported (§7).
3. **Frame path (Option A, if ratified).** Swap the frame channel to
   `rch::watch`, delete the diff/baseline machinery, move row-change
   detection client-side. Separated from PR 2 so the semantic change is
   reviewed on its own.
4. **Cleanup.** JSONL reduced to the prober's legacy module, stale doc
   sweep, roadmap update.

## 7. Test strategy

- **Real-socket e2e stays the house style.** The
  `crates/horizon-sessiond/tests/e2e.rs` approach — spawn the actual
  daemon binary, talk over the actual unix socket — ports to remoc
  clients and remains the proof for attach/reconnect, PTY survival, cwd
  resolution, and drain. The `Connect::io` both-ends rule (adoption
  condition 3) applies to any in-process harness.
- **Cross-version skew tests become permanent residents.** The spike's
  V1/V2 type-pair method (`spike/remoc/tests/skew.rs`: a frozen copy of
  a vocabulary type beside its evolved twin, exercised through live
  channels in both directions) is promoted from spike code to a standing
  test module in `horizon-session-protocol`: every rule in §4 gets a
  pair proving it (field added, field missing with default, unknown
  variant to `Unknown`, one poisoned item not killing the channel). The
  frozen types are the *executable* form of the schema artifact.
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

Superseded (by this document, or by §5 Option A if ratified):

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
  `Reload Session Runtime`, drain semantics.
- PR #18's mismatch auto-recovery decisions, extended across the
  transport generation (§6).
- The event log's format and forward-compat guard (out of scope, §6).

## References

- `docs/research/remoc-spike-2026-07-20.md` — all measurements cited
  here; reproduction commands; spike code on branch `remoc-spike`
  (PR #19).
- `docs/session-daemon-design.md`, `docs/terminal-protocol-goals.md` — 
  the decisions §8 maps.
- `crates/horizon-session-protocol/src/lib.rs` (v9 envelope/handshake),
  `src/sessiond/` (client runtime being rebuilt),
  `crates/horizon-sessiond/src/terminal.rs` (baseline map §5 deletes),
  `crates/horizon-terminal-core/src/types/frame.rs` (frame/diff types),
  `crates/horizon-agent/src/wire.rs` (agent vocabulary, pin tests).
- remoc CHANGELOG (chmux v2→v3 compatibility note; 0.18.0 default-codec
  change), postcard-rpc's `Key` schema-hash design (§4's reference
  implementation).
