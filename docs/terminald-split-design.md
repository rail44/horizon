# terminald 分離 — ターミナルを reload の巻き添えから外す

Status: **implemented 2026-07-30**（wire v17）。決定 1–7 すべて実装済み。
実装の記録は末尾「実装記録」に。

## 動機（実測）

`Reload Session Runtime` は毎回全ターミナル PTY を殺す（UI 側の明示
terminate + daemon 側の SIGHUP + master close の二重殺し —
`src/workspace/commands.rs:17,151` / `crates/horizon-sessiond/src/
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

1. **`horizon-terminald` を分離する**（TerminalHost + PTY 所有、
   ~900 LOC = 非共有コードの 13%。agent 状態との共有はゼロ —
   lineage 木もターミナルを除外済み）。自分の socket を持ち、
   sessiond と同様に on-demand spawn。
2. **`Reload Session Runtime` は sessiond（agent runtime）だけを
   drain・respawn** する。ターミナルは無傷。
3. terminal-core の変更を反映する **`Reload Terminal Runtime`** を
   別コマンドとして新設（明示的・破壊的 — close/terminate 分離の
   既存規律に一致）。
4. **UI 側の再 adopt を reload 経路に配線**: `prepare_workspace_for_
   runtime_reload` のターミナル terminate をやめ、
   `session_lifecycle.rs:848` で `spawn_terminal_resume` を
   `spawn_agent_resume` と並走させる（UI 再起動側で実証済みの機構の
   再利用 — S サイズ）。
5. **terminal 向きプロトコルスライスは append-only 規律に移行**
   （オーナー受諾済み）: 以後、terminal 系 wire 型の reshape は
   「terminald の再起動を要求する重い変更」として扱い、原則追記のみ。
   agent 側スライス（v14/15/16 の類）は従来通り自由に動かしてよい。
6. **下層破壊への保険**: hello の `binary_id` を使い、transport/
   serialization 層の不一致が疑われる場合（デコード失敗の初回）に
   silent 継続でなく **clean refuse + 再起動案内**へ落とす。tmux 3.6
   の教訓の機械化。
7. **backstop はスナップショット復元**（workspace restore、既設）。
   terminald が死ぬ時は今まで通り正直に死ぬ。

## スコープ・分担の見取り

- 新 crate（or bin target）`horizon-terminald`: TerminalHost 移設、
  terminal-only hub（既存 trait のサブセット）
- `horizon-session-protocol`: terminal サブセットの切り出し（追記
  のみで可能な見込み）
- `src/sessiond/`（client runtime）: **最大の作業箇所** — 「接続は
  1 本」前提の解体（RuntimeControl/Routes/op queue の 2 本化、
  call site ごとの routing: terminal 系 → terminald、他 → sessiond）
- `src/workspace/`: reload 2 コマンド化、`sessiond_slot` の 2 slot 化
- 既知の要注意: `spawn_workspace_restore` の cross-inventory 衝突
  検査（1 daemon が両方を報告する前提）、backlog 50（reload 時
  reseed — 本件で moot 化）、backlog 51（stale daemon 面が 2 倍）

サイズ感: M。フェーズ分割（先に UI 側 4 と config-only reload、
次に daemon 分離本体）を推奨。

## 却下した代替

- **fd handoff / self re-exec**: 先行事例ゼロ・失敗時全損・状態転送
  （エミュレータのモードフラグ等、Ink 系 CLI が依存する部分）が
  未解決。将来「terminald 自体の無停止更新」が欲しくなったら、
  小さくなった terminald の上で再検討する方が安全。
- **agent サブプロセス化（c2）**: 分割を一段深くした形だが persistence
  （単一 writer スレッド）の再設計が付随し、fit が最低。

## 実装記録（2026-07-30、wire v17）

The split landed as one change; what follows is what a reader of the code
needs that the decisions above do not already say.

**Shape.** `horizon-terminald` is a new workspace crate
(`crates/horizon-terminald`) with `TerminalHost` moved into it verbatim and
its own `main` (bind-first accept loop, no persistence to resume, no
readiness gate). `horizon-sessiond` dropped `terminal.rs`, its
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
the automatic drain a v17 client sends to a still-running *v16* sessiond is
itself index-shifted, so that daemon ignores it and the client reports
"kept accepting connections after the drain call; stop it manually". One
manual kill, once, at this boundary.

**Client runtime.** `src/sessiond/` hosts two runtimes: `SessiondHandle`
(agent ops) and `TerminaldHandle` (terminal ops), each with its own
connection, op queue, `RuntimeControl`, and route table (`AgentRoutes` /
`TerminalRoutes` in `routing.rs`; `common.rs` holds what is genuinely
shared). Splitting the route tables removed a coupling the design doc did
not name: the single `Routes` used to fan a connection failure out to *both*
domains, so a dead agent daemon painted every terminal pane with an error.
Terminald's connection additionally issues one `list_terminals` probe right
after `hello` — decision 6's insurance — and refuses cleanly, naming the
peer's `binary_id` and `Reload Terminal Runtime`, when that probe fails on a
still-live connection. Per-item decode failures on the live attachment
channels stay tolerant (skipped, rate-limit logged): one poisoned frame must
not kill every running shell, which is the outcome this split exists to
prevent. What the probe does *not* catch is written down at
`terminald::establish`.

**UI.** `Reload Session Runtime` now drops only agent sessions, agent
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
`a_sessiond_drain_and_respawn_leaves_a_live_terminald_session_attachable`
spawns both daemons, performs the real `Reload Session Runtime` sequence
against sessiond (rtc drain → exit 0 → respawn on the same socket), and then
proves the terminal session is still listed, still attachable, still carrying
its retained frame, and still running a shell that answers new input. The
client-side half is
`draining_the_agent_runtime_leaves_the_terminal_runtime_untouched` in
`src/sessiond/tests.rs`.

**Deliberately not done.** `horizon-terminald` still depends on
`horizon-session-protocol`, which names the agent vocabulary, so
`horizon-agent` (and DuckDB) sits in the terminal daemon's *link* graph
without a single symbol being used. That is build-time only — process,
socket, and trait separation all hold — and carving the protocol crate's
domain-free foundation into its own crate is tracked as follow-up
(`docs/tasks/backlog.md`) so the wire itself moves exactly once.
