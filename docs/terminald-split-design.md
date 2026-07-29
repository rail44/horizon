# terminald 分離 — ターミナルを reload の巻き添えから外す

Status: designed 2026-07-30（オーナー決定）。実装はフェーズ分割で着手。

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
