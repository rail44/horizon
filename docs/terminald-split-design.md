# terminald 分離 — ターミナルを reload の巻き添えから外す

Status: implemented 2026-07-30。当時の wire は v17（番号は以降 drift するため、現行値はコード — `TERMINAL_PROTOCOL_VERSION` — を参照）。

## 動機（実測）

`Reload Agent Runtime` は毎回全ターミナル PTY を殺す（UI 側の明示
terminate + daemon 側の SIGHUP + master close の二重殺し —
`src/workspace/commands.rs:17,151` / `crates/horizon-agentd/src/
hub.rs:389-393` / `terminal.rs:186`）。中で走る対話 CLI（オーナーの
Claude Code）が道連れになるのが現在最大の運用痛。

直近 60 日の daemon 関連 203 コミットのうち **135（67%）は agent 側
のみ**で、ターミナルを殺す因果的必要がない。reload の動機の主流
（agent コード変更・[provider] 反映）はどちらも PTY 所有プロセスの
再起動を要求しない。1 プロセスが両方を抱えていることだけが理由。

## 先行事例の要点（docs/research/ 相当の調査 2 本、2026-07-30）

- tmux/screen は「旧 server が旧バイナリで走り続け、新 client が
  attach する」を ~12 年成立させている。条件はプロトコルの実質凍結
  （追記のみ・引退 slot は墓標化・bump しない規律）
- server の hot-swap / fd 引き継ぎは tmux/screen/zellij/mosh の
  どこにも存在しない（novel work であり、避けるのが正道）
- tmux 3.6 事件: 12 年守った番号の下（vendored imsg の fd-passing）
  が変わり沈黙破壊。**凍結は自分が所有する層しか守らない** — 下層
  変化には clean refuse で備えるのが唯一の防御
- zellij の教訓: 完全一致要求は「黙って消える」を生む。tolerant +
  疎 epoch へ転向した（Horizon の範囲交渉は最初からその形）

## 決定

1. **`horizon-terminald` を分離する**: TerminalHost と全 PTY を所有し、
   自分の socket を持つ（on-demand spawn）。
2. **`Reload Agent Runtime` は agent runtime のみ drain・respawn**
   する。ターミナルは無傷。
3. **`Reload Terminal Runtime` を別コマンドとして新設**（明示的・
   破壊的 — close/terminate 分離の既存規律に一致）。
4. **UI 側の reload 経路を 2 ランタイム化**: agent reload はターミナルを
   触らず（terminate しない）。その再 adopt は agent 側の
   `spawn_agent_resume` のみ（ターミナルは切断されていないので再 adopt
   不要）。両 handle を検証する `spawn_workspace_restore` は UI 起動時の
   restore 経路。terminal の terminate は destructive な `Reload Terminal
   Runtime` の経路（`prepare_workspace_for_terminal_runtime_reload`）のみ。
5. **terminal 向き wire 型は append-only**。reshape は terminald 再起動を
   要求する重い変更として扱う。
6. **binary 不一致は clean refuse + 再起動案内**（hello の `binary_id` を
   使い、silent 継続にしない）。
7. **backstop はスナップショット復元**（workspace restore）。

## 実装記録（2026-07-30、wire v17）

The split landed as one change; what follows is what a reader of the code
needs that the design above does not already say.

**Shape.** `horizon-terminald` is a new workspace crate
(`crates/horizon-terminald`) with `TerminalHost` moved into it verbatim and
its own `main` (bind-first accept loop, no persistence to resume, no
readiness gate). `horizon-agentd` dropped `terminal.rs`, its
`portable-pty`/`sysinfo`/`horizon-terminal-core` dependencies, and the three
terminal hub methods; its `drain` no longer touches a PTY.

**Protocol.** One crate (`horizon-session-protocol`) now holds *two*
`#[rtc::remote]` traits: `TerminalHub` (hello / list_terminals /
create_terminal / attach_terminal / drain, replying `TerminalHubHello`) and
the narrowed `SessionHub` (hello / list_agents / new_agent / attach_agent /
drain). `HubError`, `ClientHello`, `VersionRange`, the codec pin and every
size cap stay shared, so one handshake serves both daemons and one artifact
(`schema/session-wire.json`, now with a `terminal_hub` section) documents
both wires.

Wire cost, as anticipated: removing methods from the middle of an
index-encoded request enum is a hard reshape, so `SESSION_PROTOCOL_VERSION`
is 17 **and** `MIN_SUPPORTED_PROTOCOL_VERSION` rises to 17 with it (only the
second time, after v11). One transition wart is accepted rather than hidden:
the automatic drain a v17 client sends to a still-running *v16* daemon (the
binary then named `horizon-sessiond`) is
itself index-shifted, so that daemon ignores it and the client reports
"kept accepting connections after the drain call; stop it manually". One
manual kill, once, at this boundary.

**Client runtime.** `src/runtime/` hosts two runtimes: `AgentdHandle`
(agent ops) and `TerminaldHandle` (terminal ops), each with its own
connection, op queue, `RuntimeControl`, and route table (`AgentRoutes` /
`TerminalRoutes` in `routing.rs`; `common.rs` holds what is genuinely
shared). Splitting the route tables removed a coupling the design doc did
not name: the single `Routes` used to fan a connection failure out to *both*
domains, so a dead agent daemon painted every terminal pane with an error.
Terminald's connection additionally issues one `list_terminals` probe right
after `hello` — the clean-refuse insurance described above — and refuses cleanly, naming the
peer's `binary_id` and `Reload Terminal Runtime`, when that probe fails on a
still-live connection. Per-item decode failures on the live attachment
channels stay tolerant (skipped, rate-limit logged): one poisoned frame must
not kill every running shell, which is the outcome this split exists to
prevent. What the probe does *not* catch is written down at
`runtime::terminal::establish`.

**UI.** `Reload Agent Runtime` now drops only agent sessions, agent
entities, and agent pane views; terminal panes keep their views (and thus
their scroll/selection state) because their sessions never died.
`Reload Terminal Runtime` (palette, `reload-terminal-runtime` keybinding id,
`horizon reload-terminal-runtime`) is the destructive counterpart and owns
what used to be collateral damage: terminating the terminal model sessions,
reseeding a pane, and re-adopting anything that survived a refused drain.
`spawn_workspace_restore` now takes both handles and validates both runtimes
before adopting; its cross-inventory conflict check is unchanged in logic but
now compares reports from two processes.

**Acceptance.** `horizon-terminald::e2e`'s
`an_agentd_drain_and_respawn_leaves_a_live_terminald_session_attachable`
spawns both daemons, performs the real `Reload Agent Runtime` sequence
against agentd (rtc drain → exit 0 → respawn on the same socket), and then
proves the terminal session is still listed, still attachable, still carrying
its retained frame, and still running a shell that answers new input. The
client-side half is
`draining_the_agent_runtime_leaves_the_terminal_runtime_untouched` in
`src/runtime/tests.rs`.

**Deliberately not done.** `horizon-terminald` still depends on
`horizon-session-protocol`, which names the agent vocabulary, so
`horizon-agent` (and DuckDB) sits in the terminal daemon's *link* graph
without a single symbol being used. That is build-time only — process,
socket, and trait separation all hold — and carving the protocol crate's
domain-free foundation into its own crate is tracked as follow-up
(`docs/tasks/backlog.md`) so the wire itself moves exactly once.

*Update (`docs/runtime-crate-alignment-design.md` phase 1):* the
foundation is carved out as `horizon-wire`, and `horizon-terminald`'s
manifest no longer names `horizon-agent` — the socket-path convention it
reached for lives in `horizon_wire::socket` now. The *transitive* link
survives, because both hub traits still share `horizon-session-protocol`;
phase 2 moves `TerminalHub` into `horizon-terminal-core` and closes it.

*Update (phase 2, landed):* closed. `TerminalHub` and its version pair
live in `horizon_terminal_core::wire`, `SessionHub` and its pair in
`horizon_agent::wire`, `HubError` and the rest of the shared handshake in
`horizon-wire`, and `horizon-session-protocol` is deleted. So the
"Protocol" paragraph above is superseded in two places: there is no
single crate holding both traits, and the one artifact became two
(`crates/horizon-agent/schema/agent-wire.json`,
`crates/horizon-terminal-core/schema/terminal-wire.json`) with the inner
keys unchanged. The version pair split with them — both halves start at
18, so the split itself is wire-neutral — which is the point: an
agent-side bump no longer rejects a running `horizon-terminald` and
auto-drains its PTYs. `cargo tree -p horizon-terminald -e normal` now has
neither `horizon-agent` nor libduckdb; the only agent edge left is
`tests/e2e.rs`'s dev-dependency, which exists precisely to drive a real
agentd through the acceptance property above.

## Geometry at runtime entry points (2026-09-24)

Wire geometry remains a plain `TerminalSize`. Spawn and resize normalize its
cell dimensions with the emulation engine's minima (two columns, one row)
before updating both the PTY and core. The core also normalizes direct
session-loop input. Zero dimensions previously panicked during construction
or resize; a one-column screen cannot hold a wide cell safely. Valid sizes
and supplied pixel dimensions remain unchanged, including zero pixels for
unknown geometry. This adds no protocol fields or user configuration.
