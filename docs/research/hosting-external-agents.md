# 外部エージェント面のホスト手段 — 調査記録(2026-08-06)

調査日: 2026-08-06、web 検証つき。実測バージョン: Claude Code 2.1.222、
codex-cli 0.145.0、`@agentclientprotocol/claude-agent-acp` 0.65.0、
`agent-client-protocol` crate 2.0.0、herdr v0.8.0(2026-08-03)。

問い: Horizon が外部のコーディングエージェント面(Claude Code / Codex)を
自分の中に住まわせ、**(i) 走っているセッションにイベントを差し込む
(ii) 状態を構造化して観測する (iii) 承認を仲介する**を、**そのエージェント
のネイティブ機能を失わずに**実現する手段は何か。

オーナーの仮説(検証対象として明示): ACP や PTY での包装はネイティブ機能
への操作性を失うトレードオフがある。ベンダーが提供する標準的な口が
あるならそちらが良い。PTY は herdr が近いことをやっており、Horizon の
ターミナル機能の拡張として考える余地がある。

## 結論: 仮説は成立。ただし**ベンダーごとに勝ち筋が逆**

- **Claude Code は「与えられる」側の口を持つ** — Channels(走行中セッション
  への push)+ 権限中継 + HTTP hooks。**ネイティブ機能の損失ゼロ**
- **Codex は「ホストされる」側の設計** — `codex app-server` をホストが動かし、
  **ネイティブ TUI 自身が `codex --remote` で繋ぐクライアント**。だから
  包んでも何も失われない
- したがって**単一プロトコルに寄せる設計は誤り**。抽象は能力(注入/観測/
  承認)で切る

## Claude Code の面

**Channels**(research preview、[docs](https://code.claude.com/docs/en/channels)):
MCP サーバとして常駐し `notifications/claude/channel` でイベントを push。
**走っている対話セッションのコンテキストに `<channel source="..." ...>` として
入る**。`instructions` でイベントの意味と反応の仕方をシステムプロンプトに
教えられる。双方向は普通の MCP ツール(`reply`)で。

**権限中継**: `capabilities.experimental['claude/channel/permission']` を
宣言すると、**全てのツール承認プロンプトが channel に転送**され、
`{request_id, tool_name, description, input_preview}` に対して
allow/deny を返せる。**端末のダイアログは開いたままで、先に答えた方が
勝つ** — 乗っ取りではなく co-equal な仲介。

限界(正直に): research preview で `--channels` は `--help` に出ない。
custom channel は `--dangerously-load-development-channels` が要り全画面
警告が出る(Anthropic のキュレート済みリスト以外)。起動時にオプトイン
(= ホストが起動コマンドを握る必要がある。Horizon はペインを spawn する
ので満たす)。**走行中ターンへの割り込みは不可**(次のターンでまとめて配送)。
**配送の ack が無い**(ロードされていなければ黙って捨てられる)。
prompt-injection 面であることを docs 自身が明言。

**hooks**([docs](https://code.claude.com/docs/en/hooks)): 30 イベント、
ハンドラ5種のうち **`type: "http"` はホストのデーモンに直接 POST**。
状態の対応: busy = `UserPromptSubmit`/`PreToolUse`/`PostToolUse`、
承認待ち = `PermissionRequest`/`Notification(permission_prompt)`、
入力待ち = `Notification(idle_prompt / agent_needs_input)`、
完了 = `Stop`/`SessionEnd`/`StopFailure`。**hook の応答でホストが
ポリシーエンジンになれる**(`permissionDecision`、`updatedInput`、
`additionalContext` でコンテキスト注入)。

**存在しないもの**: 走行中セッションへの汎用 IPC。issue #53049 と #21419
はいずれも duplicate として close。Channels が公式の答え。

## Codex の面

**`codex app-server`**([docs](https://learn.chatgpt.com/docs/app-server)):
JSON-RPC。transport は stdio(production-ready)/ unix socket / ws(実験・
非サポート)。**注入が最も強力**: `turn/steer`(実行中ターンへの追加入力)、
`thread/inject_items`(**ターンを起こさずモデル可視の履歴へ注入**)。
状態は `thread/status/changed` / `turn/*` / `item/*` と
`canAcceptDirectInput`。承認はサーバ発の JSON-RPC request で 6 種の判断
(execpolicy/network の amendment 込み)。**`generate-json-schema` で
バージョン固定の型を生成できる**(Rust 実装コストが下がる)。

**ネイティブを失わない理由**: `codex --remote` でネイティブ TUI が同じ
app-server に peer として繋ぐ。ホストと人間が同じ thread を共有できる。

**Codex hooks** は 11 イベントだが **`type: "command"` のみ実装**(http /
prompt は parse されるが skip)。`notify` は hooks の legacy shim に移行済み。

**存在しないもの**: channel 相当(#15299 は open、`codex inject` #11415 は
未実装のまま close)。

## ACP の判定: 不利(採らない)

`claude-agent-acp` 越しに失われるもの(アダプタ自身の issue): **hooks が
発火しない(#144、2025-11 から open)**、組み込み slash コマンドが無反応
(#642)、`/plugin` 不可(#580)、**スケジュールタスク・`/loop` が idle 中に
発火しない(#838)**、セッションスコープの MCP が届かない(#883)、`/btw`
不可(#531)。加えて **session-level の active/idle シグナルが無い**(#864)、
走行中ターンへの差し込みは非標準の `_meta` 拡張。

つまり **logd が欲しがっている最も豊かな通知面(hooks)をちょうど殺し**、
その対価として得られる注入は Channels より弱く、状態は hooks より貧しい。
ACP の価値は 25+ エージェントへの横展開であって、単一ベンダーでの深さ
ではない。将来 Gemini CLI 等を1つのペイン型で扱いたくなったら再訪。

## PTY / herdr(オーナーの見立ての検証)

herdr は自ら *"it doesn't wrap them or replace them, it just owns their
terminals"* と位置づけ、`AppState` は PTY 無しでテストできる純データ、と
アーキテクチャ規律を置いている — **エージェント統合ではなく、エージェント
を理解するターミナルマルチプレクサ**。

- **注入**: unix socket 上の NDJSON API。`pane.send_text`/`send_keys` と
  agent 解決つきの `agent prompt`(bracketed-paste 対応、**作業中の
  エージェントにも送れる**)。この軸だけは ACP に勝つ
- **状態検知**: (1) ライフサイクル hooks が完全なら**それを権威とし、
  スクリーン判定は走らせない**(二重の真実源を作らない)、(2) 無ければ
  **スクリーンマニフェスト**(TOML の正規表現ルール + OSC タイトル/進捗)。
  **Claude Code と Codex はどちらも screen manifest 側**にリストされている
  (両者の hooks 統合は「セッション同定」用で状態権威ではない)
- **自認する限界**: 未知の UI は **idle にフォールバック**(承認待ちが
  「準備完了」に見える)、`unknown` は完了の証明ではない、alternate screen
  の履歴読みはエージェントのマウススクロールを駆動する必要があり idle 時
  のみ、nested tmux で検知不能。マニフェストは 2026-08-04 付 — **UI 変更へ
  の継続的追随が必要**

→ **オーナーの見立てどおり**: PTY は Horizon の terminald の拡張(注入 API +
バッファ/OSC 読み取り)として持ち、**状態判定は差し替え可能なフォール
バック層**に落とすのが正しい分解。herdr の manifest 設計(明示的な
`unknown`、命名された idle フォールバック理由、hooks 優先の precedence)は
Apache-2.0 なので形式の借用は clean。

## 未確認(設計前に潰すべき)

1. **Channels の preview の行方** — フラグが `--help` に無く、custom は
   `--dangerously-` 経由、リスト入りの道は「Anthropic のパートナー窓口」。
   **アーキテクチャを賭ける前に使い捨ての channel で実地検証すべき**
2. Horizon が spawn した対話ペイン + channel + Horizon 登録の HTTP hooks が
   きれいに合成されるか(未検証)
3. channel 配送の負荷時の順序・レート(coalescing や backpressure の記述なし)
4. Codex app-server の安定性(全 subcommand が `[experimental]`、WS は
   非サポート、daemon churn の既知バグ #23954)
5. Codex hooks の http ハンドラ(「parse されるが skip」= 予定はある?)
6. HTTP hooks がツール呼び出しのホットパスに入る際のレイテンシ予算

出典: agentclientprotocol.com、claude-agent-acp の issues、
code.claude.com/docs(hooks / channels / headless / sessions)、
learn.chatgpt.com/docs(app-server / hooks)、openai/codex issues、
herdr.dev/docs + github.com/ogulcancelik/herdr。
