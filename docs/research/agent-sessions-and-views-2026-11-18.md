# 外部エージェントのセッションとビューの管理 — 調査記録(2026-11-18)

調査日: 2026-11-18(一覧の見せ方の調査は同じ週の後半)。前回の記録
`docs/research/hosting-external-agents.md`(2026-08-06)からの差分として
調べた。この調査を材料にした判断はボード #82 にある。

問い:

1. herdr と orca は、既存のエージェント(Claude Code、Codex ほか)の
   セッションと UI をどう制御しているか。
2. 走っているエージェントのセッションを Horizon が拾う手段に何があり、
   前回の記録から何が変わったか(Claude Code、Codex、ACP、OSC 7501、
   OTel、そのほかの規格)。
3. 先行事例は、複数のエージェントの一覧をどう見せているか。

## 0. 調べ方と表記

- ソースを読んだのは主に調査用のサブエージェントで、結論を左右する主張は
  ソースや手元のコマンドで抜き取り確認した。確認の度合いを各項目に付ける。
  - **[ソース]** ソース・スキーマ・JSON を読んだ。
  - **[実測]** 手元で実行して確かめた。
  - **[docs]** 公式の文書・README・CHANGELOG の記述。
  - **[二次]** 検索結果の要約や第三者の記事。
  - **[未確認]** 確かめていない(各節の末尾と §13 にまとめた)。
- 対象の版: herdr v0.9.5(HEAD `ee5bfb50`、2026-10-29)、stablyai/orca
  HEAD `b7f42562`(2026-11-17)、Claude Code 2.1.326(手元、実測は
  2.1.327)、codex-cli 0.156.1(手元、2026-09-23 リリース。最新安定版は
  0.169.0、2026-11-13)と Codex のソース HEAD `ce5a565b`(2026-11-17)、
  ACP の crate `agent-client-protocol` 5.3.0 / `agent-client-protocol-schema`
  1.18.0 と仕様リポジトリ `af7218e3`(2026-11-17)。
- 前回(2026-08-06)の版: herdr v0.8.0、Claude Code 2.1.222、codex-cli
  0.145.0、ACP crate 2.0.0。

---

## 1. herdr

対象: `github.com/ogulcancelik/herdr` の shallow clone(`Cargo.toml` の
repository は `github.com/herdrdev/herdr`)。docs はリポジトリ内の
`docs/next/website/src/content/docs/*.mdx`。コードは実行していない。

### 基本

- 最新タグ v0.9.5(2026-10-14)。Rust 約 30 万行。VT エミュレータは
  vendored の libghostty-vt。[ソース]

### どのエージェントか、セッション ID、再開

- **どのエージェントかはプロセス検出が主**: ペインのシェルの前景ジョブ
  (Linux では `/proc/<pid>/stat` の tpgid)のプロセス名・argv0・
  node/bun ラッパの argv を固定の表と照合する(`detect/mod.rs` の
  `identify_agent_process_in_job`)。ポーリングは未同定 500ms、
  同定済み 300ms。補助に前景プロセスの環境変数 `HERDR_AGENT`。[ソース]
- **セッション ID はエージェント側のフックがソケットに報告する**: Claude
  用のフック(`integration/assets/claude/herdr-agent-state.sh`)は
  `SessionStart` だけで動き、hook 入力の `session_id` などを NDJSON で
  unix socket に送る。Codex 用もほぼ同じ。統合はユーザーの
  `~/.claude/settings.json` や `~/.codex/hooks.json` に SessionStart の
  フックを 1 つ書き込み、旧版のフック(PreToolUse ほか)は取り除く。
  [ソース]
- **復元は再開コマンドを新しいシェルに打ち込む**: 保存した ID から
  `claude --resume <id>` や `codex resume <id>` を組み、新しいシェルを
  PTY で起動して quote した argv と `\r` を書き込む
  (`app/agent_resume.rs`)。[docs と報告。打ち込む箇所はアシスタントの
  抜き取りでは確かめられず未確認]
- 0.9.2 から、エージェントが `resume_argv` を自己申告できる。[ソース]
- ペインには `HERDR_ENV=1`、`HERDR_SOCKET_PATH`、`HERDR_PANE_ID` などを
  入れ、`CLAUDECODE`、`CODEX_THREAD_ID`、`TMUX` などを消す。[ソース]

### ライフサイクル

- `agent start` は空いているシェルペインに起動コマンドを**タイプ入力**する
  (bracketed paste を考慮)。[ソース]
- server/client 分離。サーバは `setsid` したデーモンで全 PTY を持つ。
  0.9.0 から TUI は各クライアント側で描く。PTY の master fd を
  SCM_RIGHTS で新しいサーバへ渡す live handoff(experimental)。[ソース]

### 状態

- 内部 `Idle / Working / Blocked / Unknown`、公開はそれに `Done` を足した
  5 つ。`Done` は「Idle かつ未閲覧」で導く(ペインごとの `seen`)。
  [ソース]
- 状態の入力源は 3 区分([ソース、アシスタントが一致を確認]):
  - 画面の照合(埋め込み TOML のマニフェストが画面下部のテキスト・
    タイトル・OSC の進捗を照合)。
  - フックの状態報告を権威とするのは pi、omp、mastracode、opencode、
    kilo、kimi の 6 種だけ。
  - claude、codex、copilot、devin、droid、qodercli、qwen、cursor、grok の
    9 種はフックの状態を捨て、セッション ID だけを取る。
- **Claude Code と Codex の状態は今も画面の照合**。Codex は 0.9.4 で一時
  フック経由の完了報告をしていたが、0.9.5 で画面の照合に戻した
  (CHANGELOG)。マニフェストは `herdr.dev` から自動更新される。[docs]
- 通知: 完了と Blocked をデスクトップ通知(外側の端末の OSC を使う)。
  未読はクライアントごと。[ソース]

### データモデルと外部操作

- `Workspace ⊃ Tab ⊃ TileLayout`(BSP の二分木)。**ペインはビューで、
  端末の実体は別**(`PaneState.attached_terminal_id`)。エージェントの
  情報は端末の側に付く。[ソース]
- NDJSON のソケット API: `workspace.*`、`tab.*`、`pane.*`、
  `agent.list/get/read/send_keys/view.set/focus/start/prompt/wait`、
  `events.subscribe` ほか。`agent.prompt --wait` は blocked なら
  `agent_blocked` を返す。[ソース]

### 前回(2026-08-06)の記録から変わった点

Claude と Codex が画面照合の側である点は変わっていない。新しく分かった
のは、SessionStart による ID の報告と再起動後の自動再開、自己申告
(0.9.2)、状態の権威の 3 区分、公開状態 `Done`、エージェント操作 API の
拡充、0.9.0 の client 側描画と live handoff、マニフェストの遠隔更新。

---

## 2. orca(stablyai/orca)

同名のものが複数ある(weill-labs/orca、orca-cli/orca、araa47/orca ほか)。
herdr と並べて語られ、外部エージェントのセッションとビューを管理している
のは stablyai/orca だけだったので、これを調べた。

### 基本

- HEAD `b7f42562`(2026-11-17)、最新リリース v1.4.261(2026-11-16)、
  MIT。Electron、React、TypeScript。描画は xterm.js、PTY は node-pty。
  依存に `@anthropic-ai/claude-agent-sdk`。[ソース]

### エージェントの動かし方 — TUI と構造化の両方

- **TUI モード(既定)**: 対応 45 種。エージェントごとに検出コマンド・
  起動コマンド・プロンプトの渡し方(argv、フラグ、起動後の stdin など)を
  表で持つ。ログインシェルを起動し、シェルの準備ができてから起動
  コマンドを流す。プロンプトはブラケットペースト。[ソース]
- **構造化モード(native chat)**: Claude は Agent SDK の `query()`、
  Codex は `codex app-server`、grok・opencode・omp・antigravity・copilot・
  kimi・cursor は ACP、pi は独自 RPC。[ソース、アシスタントが確認]
- **ターミナル付きチャット**: TUI を PTY で動かしたまま、ビューだけ
  トランスクリプト(`~/.claude/projects` などを session id で探して
  tail)を表示する。承認の回答は PTY へのキー入力。[ソース]
- 端末とチャットの行き来は会話の fork(Claude `--resume <id>
  --fork-session`、Codex `codex fork <id>`、ACP の `session/fork`)。
  [ソース]

### セッション ID と再開

- session id はフックのペイロードから取る(出力のパースやディレクトリの
  監視ではない)。[ソース]
- フックはユーザーのグローバル設定に入れる(23 エージェント)。Claude は
  `~/.claude/settings.json` に SessionStart、UserPromptSubmit、Stop、
  Pre/PostToolUse、PermissionRequest ほか。フックは
  `curl -X POST http://127.0.0.1:$ORCA_AGENT_HOOK_PORT/hook/<source>` で
  orca に届く。[ソース、アシスタントが確認]
- 再開の argv をエージェントごとに持ち、デーモンが PTY を失ったときの
  コールドリストアで自動で組む(発動条件の一部は未確認)。通常の再起動は
  デーモンへの再接続。[ソース]

### 状態

- `working / blocked / waiting / done` の 4 つ。表示の idle と
  unverifiable は派生値。[ソース、アシスタントが確認]
- 入力源の優先順位: 1. フック、2. OSC 7501(PTY の出力から読み、`?` の
  問い合わせにも答える)、3. タイトル、4. OSC 9999(orca 独自、書き手は
  未確認)、5. Claude のステータスラインとプロセスの監視。**フックが 2 分
  黙っていて、OSC 7501 と食い違い続けたときだけ OSC 7501 に譲る**
  (`hook-precedence.ts`)。[ソース、アシスタントが確認]
- Claude の POSIX フックだけが判断を返せる(最大 8 秒待つ)。[ソース]

### ビューと外部操作

- `Project ⊃ Repo ⊃ Worktree`。分割は 2 階層(外側の tab group の二分木
  と、内側のターミナルタブの分割)。[報告、一部照合]
- CLI `orca`: Unix ソケットの NDJSON RPC(メソッド約 860)。
  `terminal wait --for exit|tui-idle`、`agent launch --surface
  chat|terminal`、オーケストレーションなど。MCP は注入しない。
  [報告、一部照合]
- worktree の作成と同時にエージェントを起動できる。[報告]

### herdr との対比

どちらも既存のエージェントを PTY の中で TUI のまま動かし、セッション
ID はエージェント側のフックから受け取り、再起動後は再開コマンドを
組んで打ち直す。違いは状態の取り方(Claude と Codex について、herdr は
画面照合、orca はフックが権威で OSC 7501 は予備)と、TUI 以外の動かし方を
持つかどうか。前回の記録が「採らない」とした ACP と、Codex 向けに挙げた
`codex app-server` は、orca が構造化モードで実際に使っている。

---

## 3. 2 つの系統の比較

| | PTY の中の TUI をそのまま動かし、外から情報を足す | プロトコルで繋ぎ、ホスト側の UI で描く |
|---|---|---|
| 例 | herdr の全部、orca の既定 | orca のチャット表示 |
| 繋ぎ方 | フック、OSC、画面の照合 | Claude は Agent SDK、Codex は `codex app-server`、他は ACP |
| 同定 | `SessionStart` フック | 接続そのものが ID を持つ |
| 状態 | フックのイベント(orca)か画面の照合(herdr) | 型のついたイベント |
| 入力 | PTY への貼り付けとキー入力 | メッセージ |
| 承認 | 画面のダイアログにキー。Claude だけ `PreToolUse` フックの応答で可 | 要求として届き、応答を返す |
| 再開 | `claude --resume <id>` などを打ち直す | 同じ ID で繋ぎ直す |
| 本来の UI | 全部残る | Claude は失う。Codex は `codex --remote` で TUI も同じスレッドに繋げる |
| 追随 | 画面照合は UI の変更のたび、フックはイベント名の変更 | プロトコルの版 |

- 前者は、実質的にエージェントごとの個別の対応が要る。herdr も orca も、
  判定・起動・フックの導入・ID の読み取り・状態の判定・再開コマンド・
  入力の癖を、エージェントごとのコードとデータの表として持っている。
  共通で済むのは、PTY の所有・描画・キーの転送・ソケット API・ビューの
  モデル。
- 個別の対応を減らすものとして、herdr の自己申告(置き場所が移るだけ)と
  OSC 7501(orca は実装によらず同じ読み取りで受ける)がある。
- 後者も一つではない。orca の構造化モードも、Claude は SDK、Codex は
  app-server、pi は独自 RPC、それ以外は ACP と、相手ごとに別の実装。
  「TUI を残すと個別の対応、プロトコルなら共通」とは言い切れない。

---

## 4. ACP(Agent Client Protocol)v1 / v2

### 版 [ソース]

- **v1 が安定版、v2 は Draft**(2026-07-20 公開)。schema crate 1.18.0 の
  `src/version.rs` が v2 を "unstable draft … must be selected explicitly"
  と書く。スキーマの版は `schema-v1.32.0` と `schema-v2.0.0-alpha.16`
  (どちらも 2026-11-14)。

### 拾いたいものと、対応するメソッド [ソース]

- **同定・一覧**: `session/new` → `sessionId`(v1 安定)。`session/list`
  (v1 安定 2026-02-18、行は sessionId・cwd・title・updatedAt で**状態の
  列は無い**)。`session_info_update`(安定)。`session/status`(unstable。
  意味は「メモリに載っているか」で、作業中かどうかではない)。
- **状態**: **v1 にはセッション単位の状態が無い**(ターンの終わりは
  `session/prompt` の応答の stopReason)。**v2 の `state_update`**:
  `running` / `idle`(+stopReason)/ `requires_action` / `unknown` /
  `_` で始まる拡張値と `background[]`。アシスタントが crate の
  `src/v2/client.rs` で確認。
- **入力**: `session/prompt`(v1 安定。v1 の文書にターン中の送信を
  定めた文言は見つからなかった)。v2 の「Prompts during active work」は
  "Clients MAY send `session/prompt` in any state … Agents MUST NOT reject
  a prompt only because foreground work is active" と定める(PR #2627、
  2026-10-29。この仕様文はアシスタントの抜き取りでは確かめられず、
  報告の引用のまま)。steer と queue の区別を広告する RFD(PR #1261)は
  open。
- **承認**: `session/request_permission`(v2 で作り直し)、
  `elicitation/create`(安定 2026-07-22)。`session/set_mode` は v2 で
  削除され config option に統合。`fs/*`・`terminal/*` は v2 で削除。
- **再開・複製**: `session/load`(v1)、`session/resume`(v1 安定
  2026-04-22)、v2 は load を廃止し resume に `replayFrom`。
  `session/fork` は unstable。
- **共有**: 一つのセッションを複数のクライアントで見る仕組みは仕様に無い
  (RFD PR #533 は open)。transport は stdio だけが定義されている。

### 対応するエージェント [ソース: ACP registry、2026-11-18 取得、46 件]

- Claude Code はアダプタ `@agentclientprotocol/claude-agent-acp` 0.108.0、
  Codex はアダプタ `@agentclientprotocol/codex-acp` 2.7.0。
- ネイティブ: Gemini CLI、OpenCode、Cursor CLI、Copilot CLI、Grok、Kimi、
  oh-my-pi、Goose、Antigravity、Kiro、Qwen Code、Factory Droid ほか。
- **v2 を話す外部エージェントは見つからなかった**(アダプタ 2 つを v2 の
  印で検索して何も出なかった)。

### orca の使い方 [ソース]

v1 だけ。`state_update` は使わず、作業中は「prompt の応答待ち中」で
判定する。steer は `session/cancel` で止めてから次の prompt を送る。
Claude と Codex に ACP を使わない理由は見つからなかった。

### 前回の記録の 2 つの指摘への答え

- 「セッション単位の active/idle が無い」→ v2 Draft の `state_update` で
  解決。v1 では未解決。
- 「走行中のターンへの差し込みは非標準の `_meta`」→ 送ること自体は v2
  Draft で標準になった。意味はエージェント次第。

---

## 5. Claude Code に繋ぐ手段

| 手段 | 同定・一覧 | 状態 | 走行中への入力 | 承認 | 再開 | TUI |
|---|---|---|---|---|---|---|
| Agent SDK(TS/Py) | ○ `listSessions()` | ○ ストリーム。`session_state_changed` は環境変数で有効化 | ○ ストリーミング入力 | ○ `canUseTool` | ○ | 無い |
| `claude -p` + stream-json | △ | ○ | ○ | ○ `--permission-prompt-tool` | ○ | 無い |
| Channels | | | △ 次のターンにまとめて配送 | ○ 権限の中継 | | 残る。research preview のまま |
| hooks(`type:"http"` 可) | ○ 全入力に `session_id` | ○ イベント単位 | △ | ○ `PermissionRequest`、`PreToolUse` | | 残る |
| `claude agents --json` | ○ 対話・バックグラウンド両方 | ○ `status`(busy/waiting/idle)、`state`(working/blocked/done/failed/stopped) | × | × | | 残る。ポーリング |
| inbox ソケット(2.1.224) | | | △ 「別セッションから」として届く | × | | 残る |
| OSC 7501(2.1.295) | | ○(§7) | | | | 残る |
| ACP(claude-agent-acp) | | △ active/idle が無い(#864 open) | | | | 無い。実体は SDK |

([docs] と [実測] の混在。表の根拠は以下)

### 前回(2026-08-06)から変わった点

- **inbox ソケット(2.1.224)**: 前回「汎用 IPC は存在しない」とした点が
  変わった。セッションごとに Unix ソケットを開き、パスを
  `CLAUDE_CODE_MESSAGING_SOCKET` で hooks と Bash に渡す。外から書いた
  ものは「別セッションから」として届き、承認には使えず `/` コマンドは
  実行されない。[docs] `/run/user/<uid>/cc-socks/<pid>.sock` が実在し、
  `~/.claude/sessions/<pid>.json` に `messagingSocketPath`、`sessionId`、
  `status` がある。[実測] メッセージ行の形式は docs に無く、送信は
  試していない。[未確認]
- **OSC 7501 対応(2.1.295)**。§7。
- **`--permission-prompts none|host`(2.1.259)**。2.1.302 から権限モード
  未指定の `-p` は Manual モード。[docs]
- **走行中のバックグラウンドセッションへ `--resume` で送れる(2.1.285)**。
  [docs]
- **hooks のイベントは 33 個**(前回 30)。Notification の種類に
  `agent_needs_input` と `agent_completed`。[docs]
- **Channels は research preview のまま**。[docs]
- **前回の記録の見落とし**: agent view(2.1.139)、`claude agents --json`
  (2.1.145)、`waitingFor`(2.1.162)は前回の時点で既にあった。
  `claude agents --json` を手元で実行し、対話・バックグラウンドの両方が
  `sessionId`、`pid`、`cwd`、`status` つきで返ることを確かめた。[実測]
- **ACP アダプタの issue**: #144(hooks)、#838、#642 は close。#580、
  #883、#531、#864(active/idle)は open。Claude Code 本体は ACP を
  ネイティブには話さない。[実測: `--help`・CHANGELOG・バイナリ]

### 変わっていない点 [docs]

`~/.claude/projects/*/<id>.jsonl` の書式は「internal で版ごとに変わる」。
SDK と `-p` に TUI は無い。同じセッションを 2 つのプロセスで同時に再開
するとメッセージが欠けることがある。claude.ai のログインを第三者製品で
提供することは SDK でも許されていない(ポリシー)。

### ソケットを足すと何が増えるか

- Claude Code の inbox ソケット: 加わるのは走行中のセッションへの書き込み
  だけで、同定と状態の観測には何も足さない。隣の
  `~/.claude/sessions/<pid>.json`(非文書化)から、ペインのプロセスの pid
  とセッション ID を結び付けられる可能性がある。
- Codex の共有デーモンのソケット(§6): セッションの一覧と ID、承認待ち
  と入力待ちの区別、ターンと項目のイベント、再開と複製、追加入力と承認の
  応答が加わる。ただし 0.157.0 以降、デーモンは experimental、
  プロトコルに版番号が無い。

---

## 6. OpenAI Codex に繋ぐ手段(command center を含む)

| 手段 | 要点 | 安定度 |
|---|---|---|
| `codex app-server` をホストが起動 | `thread/list`、`thread/status/changed`、`turn/start`・`turn/steer`、承認は JSON-RPC の request、`thread/resume`・`thread/fork` | サブコマンドは `[experimental]`、メソッドは stable と experimental に分かれる |
| `codex --remote` | 本物の TUI が app-server に繋ぐ。TUI にも承認が届く | |
| 共有デーモンの control socket に後から繋ぐ | TUI が既定でデーモンに乗るので、ホストは socket に繋ぐだけ | デーモンは experimental |
| `codex queue` | 手が空いた時点で配送 | docs は Stable、中身は experimental な `thread/queue/add` |
| hooks | `session_id` つき、`PermissionRequest` で allow/deny。入力待ちのイベントは無い | 既定で有効 |
| `codex exec --json` | `thread.started`、`turn.*`、`item.*` | |
| `codex mcp-server` | **0.154.0 で削除** | |
| Python SDK | app-server を spawn、steer、承認の仲介、resume と fork | 0.147 から stable |
| OSC 7501 | 0.163.0 から(§7) | |
| ACP(codex-acp) | アダプタ自身が app-server を起動するので TUI とは別プロセス | |

### app-server の詳細

- `--help` は `[experimental] Run the app server…`。[実測]
- `--listen` は `stdio://`(既定)、`unix://PATH`、`ws://IP:PORT`。unix
  socket の中身は WebSocket。[実測]
- 0.156.1 で生成したスキーマを集計して stable 101 本、`--experimental`
  付きで 164 本。[実測]
- 状態: `ThreadStatus` は `notLoaded`、`idle`、`systemError`、
  `active{activeFlags}`、`activeFlags` は `waitingOnApproval` と
  `waitingOnUserInput`(`v2/thread.rs`)。[ソース、アシスタントが確認]
- 承認の request は 6 種(コマンド実行、ファイル変更、権限、ユーザー入力、
  MCP の elicitation、ツール呼び出し)。[ソース]
- `InitializeResponse` に**プロトコルの版番号は無い**。生成物は「それを
  実行した Codex の版専用」。[ソース、docs]

### `codex --remote` と共有デーモン

- **ホストと TUI が同じ thread を同時に見られる**: 実行中の thread に
  `thread/resume` すると購読者に加わり、通知も承認の request も購読者
  全員に届く。[ソース。動かしての確認はしていない]
- 0.157.0 でデーモンの自動起動が stable・既定有効になった。手元の
  0.156.1 では無効。[ソース、実測] socket は
  `$CODEX_HOME/app-server-control/app-server-control.sock`。
- TUI がデーモンに乗らない条件: `--no-daemon`、`-c` による上書き、
  `--strict-config`、`CODEX_EXEC_SERVER_URL`、WSL の drvfs 上の
  CODEX_HOME ほか。[ソース]

### hooks [ソース]

14 種(`PreToolUse`、`PermissionRequest`、`PostToolUse`、`SessionStart`、
`Stop`、`Interrupt`、`PreModelRequest` ほか)。ハンドラの型は `command`、
`mcp_tool`、`prompt`、`agent` の 4 種で、`prompt` と `agent` は
「not supported yet」で読み飛ばされる。Claude Code の `Notification` に
当たる入力待ちのイベントは無い。

**前回の記録の訂正**: 前回の「Codex hooks の `http` は parse されるが
skip」は根拠が見つからない。v0.145.0 のソースでも variant は
`command`・`prompt`・`agent` だけで、`http` は無かった。docs が
「parsed but skipped」としているのは `prompt` と `agent`。

### そのほか

- rollout: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`。形式の
  仕様書は見つからなかった。[ソース]
- 通知は OSC 9、OSC 777(0.168 から)、BEL。タイトルは OSC 0。[ソース]
- 版ごとの変化: 0.149.0 `codex agents`(デーモン上の全セッションの
  ダッシュボード)と `codex queue`、0.154.0 `codex mcp-server` 削除、
  0.157.0 デーモンの自動起動が既定、0.163.0 OSC 7501、0.168.0 OSC 777。
  [docs]

---

## 7. OSC 7501(Program Status Protocol)

### 7.1 出どころと仕様

- Mitchell Hashimoto(Superlogical / Rex)が公開
  (https://mitchellh.com/writing/program-status-osc7501 、2026-10-06)。
  rev 0.1 が 2026-09-28、**rev 0.4 が 2026-10-10**。仕様:
  https://www.superlogical.com/rex/docs/build/program-status 、
  https://control-codes.page/proposals/osc-7501/ 。Draft。
- 形式: `ESC ] 7501 ; key=value(:key=value)* ST`、4096 バイトまで。キーは
  `state`(必須)、`kind`、`id`、`progress`(0〜100)、`app`、`title`、
  `msg`(`title` と `msg` は UTF-8 の base64)。知らないキーは無視する。
  [ソース: orca `src/shared/program-status/protocol.ts` と仕様]
- 状態(rev 0.4 の文言):
  - `idle`: "At rest, waiting for the user's next instruction."
  - `working`: "Running. May carry `progress`."
  - `done`: "Finished a piece of work. The result is ready to look at and
    the user has not seen it yet."
  - `blocked`: "Cannot continue until the user does something. `kind` says
    what, `msg` says why."
  - `error`: "Failed and stopped."
  - `clear`: 状態ではなく、指したレコードとその下のすべてを消す。`id` が
    無ければ端末上の全レコードを消す。
  - `kind`: `permission`(何かをする承認)、`question`(ユーザーが答えを
    打つ)、`auth`(ログインやトークン)。
- 中断されたら `idle`。終わってすぐ抜けるプログラムは抜ける直前に `done`
  か `error` を送るべき(SHOULD)。端末はプロセスが抜けたとき、または
  新しいシェルのプロンプト(OSC 133 `A`)で `working` と `blocked` を
  捨てる(MUST)。
- 子レコード: `/` で親子(`build/test` は `build` の子)。消去に親子関係を
  使う(MUST)。
- 問い合わせ: プログラムが `OSC 7501;? ST` を送り、対応する端末は `?` で
  始まる返事を書き戻す。返事が無い場合に備え、直後に `CSI c` を送って
  判定する。
- **セッション ID は運ばない**。`id` はレコードのパスで、レコードは pty に
  属する。orca の README も "no prompt and no session id" と書き、フックの
  代わりにはならないとする。

### 7.2 送る側と受ける側

orca の README の検証表(2026-10-17 の実機確認)[docs]:

| エージェント | 版 | 問い合わせへの返事 | 備考 |
|---|---|---|---|
| Claude Code | 2.1.302(2.1.295 から対応) | 必要 | 状態 5 つ、kind 3 種、子レコードあり |
| Grok | 1.0.60 | 必要 | |
| pi | 1.3.0 | 必要 | |
| omp | 18.10.3 | 不要 | |
| Codex | 0.163.0 | 不要 | 状態だけで kind は出さない(Codex の HEAD のソースは kind を送る。どの版からかは未確認) |
| Kiro | 3.1.0 | | 報告だけで実機未確認 |

- 送らないもの: OpenCode、Cursor、Gemini ほか。
- Claude Code: 起動時に問い合わせ、返事が来たときだけ状態を書き出す。
  tmux の中、バックグラウンドセッション、`CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`
  のときは出さない。返事をされるとタイトル先頭の状態記号を出さなくなる。
  [docs、後に実測 §7.6]
- Codex: 問い合わせをせず最初から書き出す。`msg`・`id`・`progress` は送らず、
  終了時と外部プログラムへ端末を渡すときに `state=clear`。マルチプレクサの
  中でも出す(`tui/src/terminal_program_status.rs`)。[ソース]
- 受け取る端末(2026-10-15 時点の記事): Rex(beta)、tuios 0.9.0、termio、
  diri、cmux、orca。kitty は master のみ。Ghostty は libghostty-vt には
  あるがアプリは無視。tmux は採用を見送りシーケンスを捨てる。[二次]
- orca 独自のものではなく、作者は別で、送る側・受ける側とも複数の実装が
  ある。ただし rev 0.4 の Draft。
- 食い違い: 検索の要約には「herdr は画面照合の代わりに OSC 7501 を読む」と
  あったが、herdr のソースの調査では状態の判定に OSC 7501 は出てこず、
  OSC 9;4 とタイトルを使っていた(§9)。この食い違いは解いていない。

### 7.3 各状態が出る場面

Claude Code 2.1.326 のバイナリ内の関数と、Codex HEAD の
`tui/src/chatwidget/status_surfaces.rs` ほかから読んだ。[ソース。
Claude Code は後に実測 §7.6]

| 状態 | Claude Code | Codex |
|---|---|---|
| `working` | ターン実行中。入力がキューにあるとき。ターン完了後もバックグラウンドのコマンドやモニタを待っているとき | ターン実行中、または送信済みのターンが開始待ち。goal が有効なあいだ |
| `blocked`+`permission` | ツールの承認、サンドボックスの要求、ワーカーからの要求、goal の提案、フォルダの信頼の確認 | コマンド実行・権限・パッチ適用の承認。MCP サーバからの承認形式の要求 |
| `blocked`+`question` | 入力を求める質問 | ユーザーへの質問、MCP のフォーム入力(エージェントが止まってから) |
| `blocked`+`auth` | 直前のターンが認証の失敗で終わり、まだログインし直していない | ユーザー確認が未完了、アプリ連携の認証が要る |
| `blocked`(種類なし) | 何かのダイアログが開いている | 該当なし |
| `done` | 直前のターンが正常に完了し、待っているものが無い | ターンが正常に完了 |
| `error` | 直前のターンが認証以外の失敗で終わった | ターンが失敗で終わった |
| `idle` | まだターンが無い。中断した。認証失敗の後にログインし直した | 起動直後。中断。`done`/`error` の後にキー・貼り付け・マウス操作。過去のセッションを開いた |

- `done` → `idle` に自分で戻すのは Codex だけ。Claude Code は次のターン
  まで `done`。
- 子レコードを送るのは Claude Code だけ。`msg` に作業の要約・承認の対象・
  失敗の内容を入れるのも Claude Code だけ。

### 7.4 OSC 7501 だけでは区別できない差

- `working`: モデルの応答待ちとツールの実行中。Claude Code のキュー待ち・
  バックグラウンド待ち、Codex の goal 有効中。
- `blocked`: Claude Code の種類なし。Codex 0.163.0 は種類を出さないので、
  承認待ちと質問待ちの区別が付かない(HEAD は送る)。
- `done`: 回答が完結したのか、問いかけで終わったのか。見たかどうかも
  出さない。
- `idle`: 未着手・中断・(Codex で)`done` の後のキー・過去のセッションの
  再オープン。
- `error`: 失敗の種類(Claude Code は `msg` に入れる)。
- 通知に現れないもの: どのセッションか(`/clear`、別セッションの再開、
  再起動を区別できない)、Codex のサブエージェント、Codex の実行中の質問。
- 「通知が無い」の意味が複数ある: エージェントでない、送らないエージェント、
  送らない条件(Claude Code の tmux 内やバックグラウンドセッション、
  0.163.0 より前の Codex)。
- 未確認: 最大トークン到達や拒否で終わったターンが `done` と `error` の
  どちらか。

### 7.5 子レコードの送信形(Claude Code)

```
親:          ESC ] 7501 ; state=working:app=claude-code:msg=<base64> ST
子:          ESC ] 7501 ; state=working:app=claude-code:id=bt7g05417:title=<base64> ST
子の消去:    ESC ] 7501 ; state=clear:id=bt7g05417 ST
全体の消去:  ESC ] 7501 ; state=clear ST
```

- `id` は Claude Code 内部のタスク ID。英数字と `_ . + -` 以外は `-` に
  置き換えるので、仕様の `親/子` の階層にはならず平らな ID になる。最大
  32 件。[ソース]
- 子になるもの: バックグラウンドのコマンド・モニタ、サブエージェントなどの
  タスク、リモートのエージェントの入力待ち・計画の承認待ち。終わると `id`
  指定の消去。子に `done`/`error` を付ける箇所は見当たらない。[ソース]
- 子の消去はバックグラウンドのコマンドが終わるたびに届くので、`id` の
  無い消去と同じに扱うと親の状態まで消える。親だけを持つなら、`id` の
  付いた通知は報告も消去も読み捨てる。

### 7.6 実測(Claude Code 2.1.327、窓なしの PTY、2026-11-18)

実際の `claude` を PTY で起動し、問い合わせに答え、届いた通知を時刻
つきで記録した(`crates/horizon-terminal-core/src/program_status_probe.rs`、
無視指定のテスト)。[実測]

| 場面 | 届いた通知 | 所要 |
|---|---|---|
| 起動 | 問い合わせ(`?`)、答えた直後に最初の状態 | 起動から 0.3〜0.6 秒 |
| フォルダの信頼の確認 | `blocked`+`permission`、`msg` は「trust this folder」 | 即時 |
| 信頼を承諾 | `clear` の後に `idle` | 0.5 秒 |
| プロンプト送信 | `working` | 0.03〜0.1 秒 |
| ツール実行中 | `working` に `msg`(「Reading notes.txt」など) | |
| ツールの承認待ち | `blocked`+`permission`、`msg` は「approve Bash: touch …」 | |
| 質問(AskUserQuestion) | `blocked`+`question`、`msg` は問いと選択肢 | |
| ターン完了 | `done` | |
| Esc で中断 | `idle` | 0.07 秒 |
| `/exit`、プロセスの停止 | `clear` | |

- `done` は下りない(完了後にキーを打っても次のプロンプトまで `done`)。
- 子レコードは `id` つきの別の通知(`sleep 25` で
  `state=working:…:id=bfbrr70jw:title=…`、終了時に
  `state=clear:id=bfbrr70jw`)。
- バックグラウンドのコマンドを待つあいだ、ターンの回答の後も親は `done` に
  ならず `working`(`msg` は「1 shell still running」)。コマンドが
  終わると Claude が自動で次のターンを始め、済んでから `done`。
- `msg` は日本語でも届く。
- タイトル: 問い合わせに答えない場合は `✳ Claude Code`(待機)・
  `◐ Claude Code`(実行中)と先頭に記号が付き、答えた場合は付かない。
  会話の要約が題名になる動きはどちらも同じ。

### 7.7 入力待ちの発見に使えるか

主要な場面は拾える(承認待ち = `blocked`+`permission`、質問 =
`blocked`+`question`、次の指示待ち = `done`、ログイン = `blocked`+`auth`)。
穴は、Claude Code のバックグラウンドセッション(attach 中でも送らない。
拾うなら `claude agents --json`)、0.163.0 より前の Codex、文章での
問いかけ(`done` と区別できない)、「見た」の判定(受ける側が持つ必要が
ある)、Codex の実行中の質問、送らないエージェント。

---

## 8. OpenTelemetry(GenAI semantic conventions)

- **Claude Code は OTel を書き出せる**: `CLAUDE_CODE_ENABLE_TELEMETRY=1`
  と書き出し先の指定で、メトリクス、イベント、トレース(beta)が OTLP で
  出る。全部に `session.id` が付く。[docs]
- **Codex も書き出せる**: `codex-rs/otel`、`codex.user_prompt`、
  `codex.tool_decision`、`codex.api_request` などのイベント。[ソース]
- **共通の規約は OTel の GenAI semantic conventions**(`invoke_agent` や
  ツール実行のスパン、`gen_ai.agent.id` など)。文書の表記は全部
  「Development」で安定版ではない。規約の本文は
  `open-telemetry/semantic-conventions-genai` リポジトリに移った。[docs]
- 両者とも名前は独自(`claude_code.*`、`codex.*`)で、GenAI 規約に沿うのは
  トークン数の属性など一部。[docs、ソース]
- **状態の監視には合わない**: Claude Code の承認待ちは
  `claude_code.tool.blocked_on_user` スパンと `tool_decision` イベントで
  記録され、スパンは終わった時点、イベントは判断が下されたときに書き
  出されるので、待っている最中には何も届かない。書き出しはイベントが既定
  5 秒ごと、メトリクスが 60 秒ごと。GenAI 規約のエージェント関連の文書に
  承認・待機・アイドルの属性は無い(状態を持つのは A2A 向けの
  `a2a.task.state` だけ)。[docs]
- 向いているのは状態の表示ではなく後から読む記録。OSC 7501 が運ばない
  セッション ID が、OTel には全イベントに付く。
- 未確認: 実際に OTLP を受けて何がいつ届くか、Codex の OTel の設定と
  セッション ID の属性。

---

## 9. ほかの規格

### Agent Host Protocol(AHP、Microsoft)

- エージェントのセッションの状態を複数のクライアントの間で同期する
  プロトコル。ホストが状態の正本を持ち、クライアントは購読して変更の通知を
  受ける。MIT。版の一覧は 1.0.0、1.1.0、1.2.0(draft)。[docs]
- ルートのチャネル(`ahp-root://`)でセッションの一覧と追加・削除・要約の
  変更(`root/sessionAdded` ほか)、セッションチャネルに集約した状態、
  チャットチャネルにターンやツール呼び出し。`listSessions`、`subscribe`、
  `dispatchAction`。[docs]
- ACP との関係: "AHP is a coordination layer. ACP is a communication layer.
  They compose naturally." AHP のホストは ACP をエージェント側の
  プロトコルに使える。[docs]
- 話すのはエージェント自身ではなく、エージェントを動かすホスト。参照実装は
  VS Code の agent host。実装一覧に pi-ahp、ahpd、AHP-ACP Gateway ほか。
  [docs]
- Claude Code や Codex の TUI が AHP で状態を出すわけではないので、
  ターミナルペインで動くエージェントを拾う手段にはならない。関係するのは、
  ホスト(Horizon では agentd)がセッションの状態をクライアントに見せる
  側の形。
- 出典: https://github.com/microsoft/agent-host-protocol 、
  https://microsoft.github.io/agent-host-protocol/ 、
  https://microsoft.github.io/agent-host-protocol/guide/ahp-and-acp 、
  https://code.visualstudio.com/blogs/2026/08/26/agent-host-architecture 。
- 未確認: セッションの状態の値の語彙。版と draft の表記は出典の間で
  食い違う。

### そのほかに当たったもの

| 名前 | 何か | 関係 |
|---|---|---|
| AG-UI | エージェントのバックエンドと画面の間のイベント規約。実行の開始・終了と、人の入力待ちでの中断を定める | アプリ組み込みのエージェント向け、draft。Claude Code や Codex は出さない |
| coder/agentapi | 各種 CLI エージェントを端末エミュレータ越しに HTTP で操作する Go のサーバ | 仕様ではなく実装、herdr と同じ画面読み取りの系統 |
| Open Agent View | 複数エージェントを一つの画面で見るダッシュボード | 製品。状態の取り方は未確認 |
| 独自 OSC の提案(seance #2) | `ESC ] <num> ; agent ; state=… ` | 採用している実装は見つからず |
| A2A | タスクの状態に input-required などがある | エージェント間の協調向けで、走っている CLI エージェントの状態を外に出す用途ではない |

### 実装をまたいで使われている慣行 [ソース、docs]

- **OSC 9;4(進捗)**: herdr のマニフェストが Claude・Kiro・Grok・Qwen の
  進捗出力を状態の判定に使う。表せるのは working/idle 程度で、承認待ちは
  表せない。
- **OSC 0/2 のタイトル**: Claude Code の記号(`⠂`/`✳`)を orca と herdr が
  読む。慣行で規格ではない。
- **OSC 133**: orca はシェルの準備状態に使い、エージェントの状態には
  使わない。
- **hooks**: 共通の仕様は無い。orca の README によれば、Codex、Droid、
  Devin、Kimi、Copilot、Grok、Antigravity ほかが Claude Code と同じ
  `PreToolUse`/`PostToolUse` という名前を使う。

### 結論

走っている Claude Code や Codex 自身が状態を外に出すための、複数の実装が
採用している仕様は、探した範囲では OSC 7501 だけだった。OSC 7501 を出す
よう求める issue がほかのエージェント(command-code、cake)にも立って
いる。LangChain の Agent Protocol と MCP の tasks は一次資料に当たれて
いない。

---

## 10. 一覧の見せ方

### 10.1 herdr

- **左の常設サイドバー**。上段が workspace の一覧、下段がエージェントの
  一覧。docs は "The sidebar is the main Herdr dashboard" と書く。幅は
  既定 26 桁(18〜36、ドラッグで変更)、開閉は `prefix+b`、折りたたみは
  compact(既定)と hidden。ほかに中央のポップアップの Go to ピッカー
  (`prefix+g`)。
- 1 行 = エージェントのいるペイン 1 つ、平坦。行の構成は設定できる
  (状態の記号、マシン、workspace、tab、エージェント名など)。経過時間・
  トークン数・cwd・最後のメッセージの組み込みトークンは無い。
- 状態は**記号と色**: blocked・done・working は既定で同じ `●` で色だけ
  違う(赤・teal・黄)、idle は `○`。
- 並び順の既定は workspace の順で、**入力待ちを上に寄せない**。
  `"priority"` にすると blocked > done > working > idle。
- タブと workspace の状態は配下で最も高い順位(rollup)。既読はそのペインを
  含む画面をそのクライアントが表示した時点。トーストは既定で off。
- 「次のエージェントへ」の操作は既定でキー未割り当て。「次の入力待ちへ」の
  専用操作は見つからなかった。
- 調査の制約: この調査では Bash が使えず、Read で辿れた範囲だけを確認した。

### 10.2 orca

- 左サイドバーが worktree のカードとエージェント単位の一覧(Activity)の
  2 種で、鈴のボタンで切り替える。鈴の右肩のバッジは常に描く(入力待ちの
  件数、作業中の件数)。ダッシュボードは実験フラグのときだけ。
- worktree のカードの中にエージェントの行が入れ子になる。ダッシュボードは
  4 列固定(attention / working / done / idle)。
- 状態は形の違う記号(回転するリング、吹き出しの「?」、チェック、点)。
  サイドバーは blocked と waiting を区別しない。
- 行: 状態、エージェントのアイコン、いましていること(要注意の内容、
  「No update in Nm」、ツールと入力、最後の発言などから選ぶ)、モデル名、
  経過時間。
- 並び順の既定は Recent。smart にすると、入力待ち → 完了から 30 分以内 →
  作業中 → idle。並べ替えは行が跳ねないよう間引いている。
- 未読の理由は完了・端末のベル・手動。既読はペインが画面上・窓が前面・
  180 秒以内にユーザー入力。
- 行のクリックでペインを開く。承認や返信の専用ボタンは無い。「次の入力待ち
  へ」のコマンドは無い(キーバインドの ID を全部確かめた)。

### 10.3 Claude Code の agent view(`claude agents`)

```
1 awaiting input · 2 working · 8 completed
Pinned
  ✽ clawd walk cycle          Drawing the walk-cycle sprite frames          3m
Ready for review
  ∙ jump physics              Opened PR with collision fix                 #2048  2h
Needs input
  ✻ power-up design           double jump or wall climb?                    1m
Working
  ✽ collision detection       Adding swept-AABB checks to CollisionSystem   2m
Completed
  ✻ title screen              menu, options, and credits done               9m
  … 6 more
```

(docs の例示から抜粋)

- 端末全体を使う。空のプロンプトで `←` でも開く。下端に新しいタスクの
  入力欄。ヘッダに件数。
- 1 行 = 1 バックグラウンドセッション。**状態で区分**(Pinned、Ready for
  review、Needs input、Working、Completed)、`Ctrl+S` でディレクトリ別。
- 行: アイコン、名前、要約 1 行(Haiku 級のモデルが書く)、PR、経過時間。
- 並び順は Pinned、review → 入力待ち → 作業中 → 完了。入りきらない分は
  `… N more`。
- 一覧の外: 端末のタイトルに件数、通常のセッションのフッタに
  `← 2 agents`。既読・未読の印は見つからなかった。
- 操作: `Space` で覗く(返信はできるが権限の確認には答えられない)、
  `Enter` で attach、`Ctrl+X` で停止、`Ctrl+T` でピン留め、絞り込みは
  入力欄に `s:<state>` など。

### 10.4 Codex の command center(`codex agents`)

```
  Agent command center  Group: Project  g
   All 6   Needs you 2   Working 1   Ready 2   Inactive 1                     tab/⇧tab filter
      Tasks                                              Status         Updated  │ Task details
  Pinned  2                                                                      │
  › ○ ☁︎ Current task                                     Ready              now  │ ☁︎ Current task
    ● Needs an answer                                    Needs input     2h ago  │ ○ Ready
  /tmp/project  4                                                                │ Model: Unknown
    ● ☁︎ Failed task                                      Error           1h ago  │
    ● Working task                                       Working         1d ago  │ Project
    ○ Unloaded task                                      Inactive        2d ago  │ /tmp/project
  ? help  esc back  ↑/↓ move  enter open  n new
```

(テストのスナップショットから抜粋)

- `codex agents`、チャット中の `/agents`、空のプロンプトで `←`。端末の
  高さいっぱい。幅 90 桁以上で一覧と詳細のパネル。
- 1 行 = 1 ルートスレッド、サブエージェントは詳細のパネル。**親の区分は
  子孫で最も急ぐもの**。グループは Project(既定)/ Directory / Status /
  Model。
- 状態: Needs input(赤 ●)、Error(赤 `!`)、Working(緑 ●)、Ready
  (未読なら青 ●、既読なら灰 ○)、Inactive。
- 既定の Project 表示では、グループ内で入力待ちを上に寄せない。状態の
  タブ(`Tab`)で絞る。
- 既読はサーバ側が持ち、そのスレッドを表示・端末にフォーカス・
  オーバーレイ無し、で付く。
- 操作: `enter` で開く、`n` 新規、`f` fork、`x` 停止、`a` アーカイブ、
  `p` ピン留め、`/` で検索。覗く・返信・承認はできない。

### 10.5 比較

| 観点 | herdr | orca | Claude Code | Codex |
|---|---|---|---|---|
| 置き場所 | 常設のサイドバー | 常設のサイドバー | 全画面 | 全画面 |
| 既定のまとめ方 | workspace の順 | プロジェクトごと | 状態ごと | プロジェクトごと |
| 入力待ちを上に寄せるか | 既定では寄せない(切り替えで可) | 既定では寄せない(切り替えで可) | 寄せる | 既定では寄せない(状態のタブで絞る) |
| 状態の出し方 | 点と色 | 形の違う記号 | 記号の形と色、見出し | 点の色と状態の文字 |
| 行の 2 つ目の情報 | エージェント名 | いましていること | 要約 1 行 | 状態と更新時刻 |
| 件数の表示 | モバイルのみ | 鈴のバッジ | ヘッダと通常画面のフッタ | 状態のタブ |
| 未読 | 完了を見るまで `done` | 鈴の印 | 持たない | 青い点 |
| 一覧からの返信・承認 | できない | できない | 覗いて返信できる。権限の確認は不可 | できない |
| 次の入力待ちへ飛ぶ操作 | 無い | 無い | 無い(区分の先頭) | 無い(状態のタブ) |

- 「入力待ちを上に」が既定なのは Claude Code だけ。
- 件数を一覧の外に出すのは共通。
- 「次の入力待ちへ飛ぶ」専用の操作はどれにも無い。
- 操作は、どれも直接のキーに割り当て、下に短い案内を出している。メニューを
  主な経路にしているものは無い。
- 時刻: どれも行に出し、並び順にも使う。意味は orca が完了から・開始から、
  Codex が更新時刻、Claude Code が作成から(終了後は実行にかかった時間で
  止まる)。

---

## 11. 付随して調べたこと

- **Blitz のインライン SVG**: `el("svg")` に `path` を足す作りで描ける。
  線を `currentColor` にすると周りの文字色で描かれる。[実測]
- **既成のアイコン集**: Lucide(ISC、24 のマス目・線幅 2、`path` と
  `circle` が混ざる、試した名前のうち 3 つは存在せず 1 つは改名済み)と
  Phosphor(MIT、取得した 7 つはすべて `path` 1 つ・`fill="currentColor"`、
  太さは 6 種の別ファイル)。どちらもテーマの色に従う。14px では標準の
  太さだと線がかすれ、吹き出しの中の「?」「…」は 16px でも判別が難しい。
  [実測: 取得した SVG を描いた]
- **Blitz の `text-overflow: ellipsis`**: 固定している版には指定を有効にする
  設定が 1 行あるだけで、省略記号を作る処理が無い(はみ出した文字は切り
  取られる)。上流の issue #888(2026-09-13、open)と PR #893
  (2026-09-14、open、変更要求)。ボード #83。[ソース]

---

## 12. 前回の記録(2026-08-06)の訂正

- Codex hooks の `http` は「parse されるが skip」ではなく、そもそも定義
  されていない(§6)。
- Claude Code に汎用の IPC は無い → 2.1.224 で inbox ソケットが入った
  (§5)。
- Claude Code の agent view・`claude agents --json`・`waitingFor` を
  見落としていた(§5)。
- ACP の「セッション単位の状態が無い」は v2 Draft で解決、v1 では未解決
  (§4)。
- OTel を調査の対象に入れていなかった(§8)。

---

## 13. 未確認のまま残った点

- herdr: 再開コマンドをシェルに打ち込む箇所。一覧の見せ方のうち、通知の
  題名の組み立て、既定の並び、hidden の描画、行クリックの処理。
- orca: デーモンが PTY を失ったときの自動再開の発動条件、承認要求の保留と
  UI・モバイルからの回答の仕組み、OSC 9999 の書き手、Claude と Codex に
  ACP を使わない理由。
- ACP: v2 を実装している外部エージェント、各エージェントの ACP 対応の
  実機確認、v2 の「ターン実行中でも入力を送ってよい」の仕様文(報告の
  引用のまま)。
- Claude Code: inbox ソケットに外から書いたときの挙動と正式な形式、
  OSC 21337 の出どころ、hooks のイベント数の差の残り 1 つ、SDK の安定度の
  表記。バックグラウンドセッション(OSC 7501 を送らない)を一覧に取り込む
  手段。
- Codex: 手元が 0.156.1 のため、デーモンへの同時接続、承認の先着順、
  OSC 7501(0.163.0 以降)をいずれも実物で確かめていない。`kind` を
  送るようになった版、rollout の形式の公式仕様。
- OSC 7501: 最大トークン到達や拒否で終わったターンが `done` と `error` の
  どちらか。Codex の実際の出力。受け取る端末の一覧(二次情報のみ)。
- OTel: OTLP を実際に受けたときの内容とタイミング、Codex の設定と
  セッション ID の属性。
- AHP: セッションの状態の値の語彙。LangChain の Agent Protocol と MCP の
  tasks は一次資料に当たれていない。
- アイコン: 集全体の絵柄の揃い、拡張(wasm)から SVG の要素を作れるか。
