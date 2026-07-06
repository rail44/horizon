# CLI 制御面 — 設計相談のための材料集

週末に予定している「Horizon の CLI インターフェース設計」相談のための調査
メモ。**この文書は推奨を押し付けるものではない。** 各論点を「事実 → 選択肢
→ 各選択肢の得失」の形で並べ、判断は所有者に委ねる。最後の「未決事項リス
ト」が相談のアジェンダそのものになる想定。

前提として共有されている所有者の言葉: 「人間とエージェントで統一されたコ
マンドモデルが芯。パレットは差し替え可能な表面」。CLI はパレット・キーバ
インドに続く第 3 の表面になる。

---

## 1. 現状の棚卸し

### 1.1 コマンドモデル (`src/app/commands.rs`, `src/app/command_actions.rs`)

- `CommandId` は 12 個の variant を持つ平坦な enum（`NewTerminal` /
  `NewAgent` / `SplitActivePane` / `FocusNextPane` / `CloseActivePane` /
  `CloseActiveTab` / `TerminateActiveSession` / `TerminateAllDetachedSessions`
  / `ApproveToolCall` / `DenyToolCall` / `CancelAgentTurn` /
  `ReloadAgentRuntime`）。`CommandSpec { id, title, category, description,
  destructive }` がメタ情報を持ち、`destructive: bool` は「セッションを終
  了する／状態を破棄する」系コマンドに立つフラグ（パレットの視覚差別化用
  に既に用意されている — 外部公開時の確認プロンプトの自然なフックになり
  うる）。
- `CommandState`（`tab_count`, `visible_pane_count`, `has_active_session`,
  `detached_session_count`, `has_pending_approval`, `has_turn_in_flight`）は
  カウント/真偽値だけの `Copy` 構造体で、`command_enabled(id, state)` は副
  作用のない純粋関数。`Workspace`/`Frames` の生スナップショットから
  `control_surface::command_state`（`src/control_surface/items.rs:8`）が組
  み立てる。
- 実行は `CommandId` そのものではなく、より詳細な `CommandInvocation` enum
  （`Simple(CommandId)` に加え `ApproveToolCall{session_id, call_id}` /
  `TerminateSession{session_id}` / `ClosePane{index}` など明示ターゲット付
  きの variant）を介す。`Simple` は「対象をその場で走査して自動解決する」
  （例: `find_pending_agent_approval` は attached/detached 問わず全エージ
  ェントセッションを走査し、承認待ちの**最初の 1 件**を対象にする — 複数
  同時承認待ちへの対応は今後の課題として明記されている）。
- `execute_command(invocation, state: CommandActionState)` の `state` は
  floem の `RwSignal<Workspace>` / `RwSignal<Frames>` / `RwSignal<Registry>`
  / `RwSignal<Option<AgentdConnection>>` などを束ねた構造体。**floem の
  `RwSignal` は UI スレッド前提の反応的プリミティブで、すべての呼び出し元
  （`workspace/view/pane.rs`, `workspace/view/tab_strip.rs`,
  `control_surface/actions.rs`, `app/input.rs`）は floem のイベントハンド
  ラ内、つまり UI スレッド上で同期的に `execute_command` を呼んでいる。**
  他スレッドからこの反応グラフに値を注入する唯一の確立パターンは
  `floem::ext_event::create_signal_from_channel` + `create_effect` の橋渡し
  （`src/agent/agentd_runtime.rs`, `src/app/runtime/terminal.rs` が多用）。
  CLI のリスナーがどこで走るにせよ、受理したコマンドをこの橋渡しを通して
  UI スレッドに渡す必要がある。
- 直近の実装（`TerminateAllDetachedSessions`,
  `src/app/command_actions.rs:345-359`）は「detached セッション ID を一括
  スナップショットしてから 1 件ずつ終了する」形。コード中のコメントは
  「今日この一括処理中に detached 集合が変化する経路は存在しない、なぜな
  ら全てが UI スレッド上で同期的に走るから」と明言している——外部コマンド
  ソース（CLI）が入るとこの前提そのものが崩れる、という点は設計上留意が
  要る。
- 「セッション列挙」自体は新規実装ではない: `Workspace::
  detached_session_summaries()`/`session_summaries()` が既にパレット・
  `CommandState` の両方を支えている。CLI の「セッション一覧」クエリは新し
  いアクセサではなく、既存アクセサへの新しい呼び出し元を追加するだけで済
  む——ただし前項の制約通り、UI スレッド上の生きた `Workspace` に対しての
  み。

### 1.2 既存の IPC 前例: `horizon-agentd` の wire 契約

- `crates/horizon-agent/src/wire.rs`: `Envelope { v: u32, session_id:
  Option<SessionId>, body: EnvelopeBody }`、`EnvelopeBody` は `{kind,
  payload}` で adjacently-tagged された `Command | Event | Control`。
  `CONTRACT_VERSION: u32 = 1` は全 envelope の `v` フィールドとして構造的
  にチェックされ（`WireError::VersionMismatch`）、これは `Hello::
  contract_version` によるハンドシェイク時の意味的バージョンチェックとは
  **意図的に別**（ACP の JSON-RPC over stdio には envelope の `v` 自体が存
  在しないため — guardrail 2「framing over any stream」）。
- `Control` enum が接続グローバルな制御語彙を持つ: `Hello{contract_version,
  binary_id, capabilities}` / `SessionList` / `SessionListResult(Vec<..>)` /
  `SessionNew{session_id, provider_id, config_overrides}` /
  `SessionLoad{session_id}` / `HostToolRequest`/`HostToolResponse` /
  `Ping`/`Pong` / `Drain` / `HandshakeRejected(String)` /
  `ToolCallProgress` / `SkippedLines(String)`。
- 転送層は「任意の `AsyncBufRead`/`AsyncWrite` 上の改行区切り JSON」で汎用
  化されており（`wire.rs` はどこにも `UnixStream` を名指ししない）、実際
  には `horizon-agentd`（`crates/horizon-agentd/src/main.rs`）と Horizon の
  `agent::agentd_client` が Unix ソケットに具体化している。
- ソケット発見: `horizon_agent::socket::default_socket_path()` は
  `$XDG_RUNTIME_DIR/horizon/agentd.sock`、無ければ
  `/tmp/horizon-agentd-$UID.sock`。agentd（bind 側）と Horizon（connect
  側）が互いに依存せず同じ既定パスに独立して到達できるよう共有モジュール
  化されている。`horizon-agentd --socket <path>` で明示上書き可能（クライ
  アント側の対称なオーバーライドは未実装、設計コメント上は "future
  HORIZON_AGENTD_SOCKET" として言及のみ）。
- bind/readiness 構造（`crates/horizon-agentd/src/main.rs`）: `bind_listener`
  はスタルソケット処理を持つ（パスが存在してもオーナーが応答しなければ削
  除して bind し直す／実際に accept 中なら奪わない）。accept ループは 1 接
  続を完全に処理し終えるまで次を受け付けない（多重クライアントは設計上ス
  コープ外と明記）。`SIGTERM` は `tokio::select!` で `listener.accept()` と
  競わせてループを止め、ソケットファイルを削除して正常終了する。1 接続あ
  たりの処理は「同期的な hello ハンドシェイク（バージョン不一致は
  `HandshakeRejected` を返して切断、これが完了するまで並行処理は始めな
  い）→ `run_session_hosting_loop`（ライターを 1 個立てて読み続ける。ホス
  ト側からの push — イベントや host-tool-request — がいつでも起こりうるた
  め）」の 2 段構え。レディネスは DuckDB 投影の再構築から意図的に切り離さ
  れている（`hello`/`session_list`/`session_load`/`session_new` はどれもそ
  れを待たない）。

### 1.3 セッション/レジストリ構造 (`src/session/`)

- `Registry { terminals: HashMap<SessionId, Sender<TerminalCommand>>, agents:
  HashMap<SessionId, SessionHandle> }`（`src/session/registry.rs`）。
  `crossbeam_channel::Sender` を持つだけの素朴な構造で、非同期性も外部公開
  も一切前提にしていない。`SessionId` は `Uuid` の newtype
  （`src/session/mod.rs`）— agent 側の `contract::SessionId` とは相互変換
  （`agent::mod` の `From` 実装）で結ばれており、wire 上でも安定して運べる
  形。
- `TerminateAllDetachedSessions` の実装（1.1 参照）は「detached セッション
  を列挙し、まとめて操作する」というまさに CLI が欲しがりそうな操作パター
  ンが、既存のアクセサだけで新規ワークスペース API なしに書けることを直近
  で証明した前例になっている。

---

## 2. 設計の選択肢空間

### 2.A チャネル

| 選択肢 | 得失 |
|---|---|
| Unix ドメインソケット + JSON Lines（agentd と同型） | `wire.rs`/`socket.rs`/bind 手順をほぼそのまま転用できる。ただし Horizon 本体は tokio ランタイムを持たない（tokio は agentd 側のみ）— リスナーを本体プロセス内に置くなら、そのためだけの非同期ランタイムを持ち込むか、素の `std::os::unix::net::UnixListener` + 専用 OS スレッドで済ませるかの判断が要る。 |
| D-Bus | Linux デスクトップの標準的な IPC。introspection や型付きシグナルが強み。macOS 展開時に `dbus-daemon` 依存が生まれる、Rust 実装（`zbus` 等）という新規依存が増える、`wire.rs` の資産をゼロから作り直しになる。 |
| varlink | JSON ベースの IDL、Unix ソケット前提で D-Bus よりポータブル。JSON Lines という発想自体は wire と近いが、契約（スキーマ言語）は別物で再利用できない。crates.io 実装のメンテ状況は薄い。 |
| TCP + localhost | クロスプラットフォームで最も均一（名前付きパイプの差異を回避）。ただし他ユーザー/他プロセスからの到達を防ぐのにポート予約とトークン認証などの追加設計が要る。Unix ソケットはファイルシステムパーミッションがそのまま認可境界になる（agentd が既に依拠している性質）。 |

### 2.B エンドポイントの居場所

所有者の既存合意（`docs/agent-runtime-split-design.md` の精神）: 「契約を
固定して端点を後から動かせる」。CLI にも同じ発想を適用するなら:

- (a) **Horizon アプリプロセス内にリスナー** — 今すぐ着手できる。ただし
  agentd の「1 接続を占有的に処理」という前提（1.2）は CLI 用にはそのまま
  引き継げない: CLI は本体・複数の呼び出し元（人間の一発コマンド、監督エ
  ージェントの購読）から同時に叩かれうる。
- (b) **将来の tmux 型セッションデーモン**（`docs/trust-boundaries.md` が
  言う「PTY とエージェントランタイムを持つセッションデーモン、UI は使い捨
  てクライアント」という長期形）— セッションの生存が UI プロセスの生死と
  切り離される版。
- (a) で始めて (b) へ移す場合に契約へ入れておくべきもの:
  - セッションは常に `SessionId` で参照する（プロセス内表現に依存しない安
    定キー）— wire で既に実践済みの原則をそのまま踏襲できる。
  - コマンドの「対象解決」をクライアント側の暗黙スキャンに依存させない。
    `find_pending_agent_approval` 的な「最初に見つかったものを対象にする」
    自動解決は UI 操作としては妥当だが、CLI からは
    `ApproveToolCall{session_id, call_id}` のように対象を明示できる形を優
    先した方が、将来複数エンドポイントが同時に存在する環境でも意味がぶれ
    ない。
  - 「今どちらが応答しているか」の発見規約（ソケットパス）を、(b) の存在
    を見越した名前空間にしておく（本体用と将来のセッションデーモン用を最
    初から別ソケットにするか、同じソケットの上で機能拡張していくかは相談
    事項）。
  - agentd の「1 接続だけを相手にする」という単純化は、CLI 契約の設計指針
    としては採用しない方が (b) への移行が楽になる可能性が高い。

### 2.C コマンド公開の粒度

- **`CommandId` をそのまま晒す** — 実装コストは最小。ただし `CommandId` は
  今のところ「UI 表示用に十分安定した識別子」であることは意図されている
  が、「外部プロトコルとして凍結される」ことは意図されていない。パレット
  の都合による rename/追加/削除がそのまま外部契約の破壊になる。
- **安定した外部コマンド名の層を挟む**（例: 名前空間付き文字列 ID +
  バージョン、`CommandId` との対応表を別途保持）。`docs/
  agent-runtime-split-design.md` の guardrail 6「keep a mapping table, not
  an implementation」と同じ発想。`CommandId` 側の内部リファクタが外部契約
  を割らなくなる代わりに、対応表のメンテというコストが増える。

### 2.D CLI の動作形

- **単発送信（fire-and-forget）** — 今の `Command::UserMessage`/`Cancel` 送
  信や大半のパレット操作（New Terminal, Split, Terminate）と同じ「送って
  終わり」の形。
- **結果取得（クエリ）** — セッション一覧（`SessionList`/`SessionListResult`
  相当）、`CommandState` 相当のスナップショット。agentd は既にこの往復を
  持つが、Horizon 側の実装（`AgentdConnection`）は「同時に 1 件しか
  `session_list` の往復を出さない」設計（`pending_session_list:
  Arc<Mutex<Option<Sender<..>>>>`）——CLI が複数クライアントから同時に問い
  合わせうることを踏まえると、この前提はそのまま流用しない方が安全。
- **購読（イベントストリーム）** — セッションの状態変化やターン終了などを
  リアルタイムに追う。段階 1（委譲）で「エージェントが CLI 越しに別セッシ
  ョンを監督する」ユースケースには、これがほぼ必須になる可能性が高い（4
  節）。
- 最初のマイルストーンに要るのはどれか、は未決（5 節）。単発送信だけでも
  「新規セッションを開く」「終了する」的な操作には価値があるが、「エージ
  ェントが起動して監督する」ユースケースには最低限クエリ（生きているか、
  完了したか）が要る。

### 2.E インスタンス発見と複数起動

- 事実: agentd は `$XDG_RUNTIME_DIR/horizon/agentd.sock` に固定、UID 単位
  でユニーク。所有者の実運用は「安定版の上に dev 版をネストして起動する」
  （`docs/trust-boundaries.md`: "UI iteration happens in a dev instance
  nested inside the stable one"）。
- 選択肢:
  1. Horizon 本体も agentd と同じ `$XDG_RUNTIME_DIR/horizon/` 配下に固定パ
     スのソケットを持つ（例: `horizon.sock`）— ただし dev インスタンスと
     安定版が同時に立っていると衝突する。
  2. PID を含む可変パス + ディスカバリ（例:
     `$XDG_RUNTIME_DIR/horizon/instances/<pid>.sock` と、どれが最新/どれ
     が「安定版」かを示す一覧・シンボリックリンク）。
  3. 環境変数（`HORIZON_SOCKET` のようなもの）で dev インスタンス起動時に
     明示的に別パスを指定させる — agentd の `--socket` オーバーライドと同
     型の考え方。
- tmux/zellij/kitty はいずれも「名前付きソケット + 明示指定」でこの問題を
  解いている（3 節）。ネスト起動運用に一番効くのは (3) + 「今どのソケット
  が安定版/dev かを CLI 側から確認できる手段」の組み合わせに見える。

### 2.F 新規セッション生成のペイロード表現

- 事実: 今の `NewAgent` コマンドは「生成」（`open_tab_with_new_session`）
  と「発話」（ユーザーがペインでテキストを打って `Command::UserMessage` を
  送る）が別ステップ。CLI から「プロンプト付きで新規エージェントセッショ
  ンを起こす」を実現する場合、この 2 ステップをどう扱うかの選択がある。
- 選択肢:
  1. CLI 側も「開く」と「メッセージを送る」を別々の 2 リクエストのままに
     し、Horizon 側で合成しない。既存の `open_tab` の薄いラッパーで足り
     る。呼び出し側（スクリプト、監督エージェント）がタイミング（セッシ
     ョンが実際にメッセージを受理できる状態になるまで待つ）を意識する必
     要がある。
  2. `new-agent --prompt "..."` のような複合コマンドを新設し、セッション
     生成後に最初の `UserMessage` を Horizon 側で自動送出する新しい
     `CommandInvocation` を追加する。呼び出し側は単純になる代わりに、
     `command_actions.rs` に新しい variant とテストが増える。

### 2.G 認可・安全性

- 3 節で見る 5 つの先行事例のうち、認証を一切持たないものが大半（tmux
  control mode、i3/sway IPC、既定の emacsclient）— いずれも「Unix ソケッ
  トのファイルパーミッション＝認可境界」に依拠している。kitty はパスワー
  ド + ECDH 暗号化という選択肢を追加で持つ（`allow_remote_control` の粗い
  全権モードと使い分け）。emacsclient は TCP モードに限りランダム生成鍵に
  よる認証を持つ。
- Horizon の `CommandSpec.destructive` フラグ（1.1）は「セッション終了/状
  態破棄」系コマンドに既に立っている——CLI 経由の破壊的操作に対して確認
  や許可リストを挟むなら、この既存フラグがそのままポリシーのフックにな
  りうる（新規に「どれが危険か」を再定義する必要がない）。

---

## 3. 先行事例（Web 調査）

- **tmux control mode (`-CC`)** — tmux は元々サーバー/クライアント型で、
  通常操作（`new-session`, `send-keys` 等）は `/tmp/tmux-$UID/` 配下の固定
  Unix ソケット経由（`-L`/`-S` で別名ソケットを選べ、複数サーバーの共存が
  可能）。「control mode」はこのソケットに繋いだクライアントをテキストプ
  ロトコルモードに切り替える拡張で、iTerm2 が tmux ペインを自前 UI に描画
  するために作られた。クライアントは通常の tmux コマンドを stdin から送
  り、サーバーはブロック応答に加えて `%` 始まりの非同期通知（ウィンドウ変
  化等）を同じ接続に多重化して返す。認証機構自体はなく、ソケットのファイ
  ルパーミッション任せ。
- **kitty remote control** — `kitty --listen-on unix:<path>` でリスナーを
  開き、`allow_remote_control`（真偽値または `socket-only`）または
  `remote_control_password` で許可レベルを制御する。前者は「動いている任
  意のローカルプロセスに全権」という粗い許可、後者はパスワード単位で権限
  を分け、X25519 (ECDH) + タイムベース nonce で通信を暗号化する。クライア
  ントは `kitten @ --to unix:<path> [--password ...] <action> ...` という
  単発呼び出し形。複数起動時のソケットパス発見はユーザー側の責任
  （`{kitty_pid}` を埋め込む例あり）で、kitty 自身はレジストリを持たない。
- **zellij action / pipe** — `zellij action <subcommand>` が CLI 引数を
  `CliAction` に変換し、実行中セッションの IPC ソケットへ転送する。セッシ
  ョンは名前で区別され（`--session`/`ZELLIJ_SESSION_NAME`）、複数セッショ
  ン並行が前提。プラグイン向けには別系統の `zellij action pipe` があり、
  CLI・キーバインド・他プラグインのいずれもが発生源になれる汎用メッセー
  ジバス。プラグインはパイプをブロックして後で解放でき、STDIN からの多重
  メッセージはプラグイン側の描画完了でバックプレッシャーがかかる——単発コ
  マンドとストリーム的用途を同じ「pipe」抽象の上に統合している。
- **i3 IPC / sway-ipc** — 固定バイナリフレーミング（`i3-ipc` というマジッ
  ク文字列 + 32bit 長 + 32bit 型番号 + JSON ペイロード）。ソケットパスは環
  境変数（`I3SOCK`/`SWAYSOCK`）経由で発見。`SUBSCRIBE` という専用メッセー
  ジ型があり、購読したいイベント種別（workspace, window, mode, tick 等）
  を JSON 配列で送ると、以後同じ接続に非同期でイベントが流れてくる——単発
  リクエスト/リプライと、SUBSCRIBE 後に購読ストリームへ転じるモードとが同
  じ接続の中で切り替わる設計。認証はソケットのファイルパーミッションの
  み。
- **emacsclient / Emacs server** — 既定では Unix ソケット
  （`/tmp/emacs$UID/server`）、TCP も選択可。TCP を使う場合に限り
  `server-auth-dir`（既定 `~/.emacs.d/server/`）に置かれたランダム生成の
  認証鍵をクライアントが読んで送る必要がある——ローカル Unix ソケットの場
  合は鍵は使わずファイルパーミッションのみ。プロトコルは単純なテキスト行
  （開くべきファイルとカレントディレクトリ）で、単発の「開いて終わり」用
  途に最適化されている。複数サーバーは `server-name` で名前空間分離——多
  重起動への対応が最初から想定されている。

---

## 4. 段階 1（委譲）との接続

- `docs/tasks/README.md` の「タスク引き渡し規約」: ミッションはファイル 1
  つ（`NNN-slug.md`、`Status: todo|in-progress|done`）に自己完結して書か
  れ、実行したエージェントは同じファイルに `## Result` セクションを追記し
  て `Status: done` に切り替える——「ミッションを書いた側がその Result を
  見に行く」という設計。現状はこの一連の流れ（ファイルを書く、実行させ
  る、Result を回収する）が完全に手作業。
- CLI が段階 1 でこの流れを自動化しうる具体像:
  1. 監督側（人、または別のエージェント）が `docs/tasks/NNN-slug.md` を書
     く。
  2. CLI の「新規エージェントセッション + プロンプト」コマンド（2.F の複
     合ペイロード）で、そのミッションファイルを読ませるプロンプトを持つ
     エージェントセッションを起動する——起動先は今の
     `command_actions::open_tab(PaneKind::Agent)` 相当。
  3. 監督側は購読（2.D のイベントストリーム）または定期的なクエリ（セッ
     ション一覧 + フレーム状態）でそのセッションの完了を検知する。特に
     `contract::Event::TurnEnded(TurnEndReason)`（`agent-runtime-split-
     design.md` step4 で導入済み）は CLI 越しの監督が「ターンが終わった」
     を判定する自然なフックの候補になる。
  4. 完了を検知したら、ミッションファイルの `## Result` セクションを読み
     に行く——ここは Horizon のコマンドモデルの外（ファイルシステム上の
     `docs/tasks/*.md`）にあるので、CLI が直接関与するとしても「セッショ
     ンのトランスクリプトを取得する」ようなクエリ止まりであり、Result を
     ファイルに書く行為自体はエージェント側の作法として残る。
- 共有される部分: セッションの生成・監視（`SessionNew` 相当、
  `TurnEnded`/`WaitingForApproval` の検知）は、人間が CLI から叩く操作と
  エージェントが同じ制御面を叩く操作とで、全く同じ形になるはず——これは
  所有者の「統一されたコマンドモデル」方針そのものの実装面での帰結。
- 分かれる部分: 承認（`ApproveToolCall`/`DenyToolCall`）の意思決定は今の
  設計では人間側に残る（`agent-runtime-split-design.md` decision 2:
  "Approval decisions stay with the human")。エージェントが別のエージェン
  トセッションを CLI 越しに監督する場合、この承認待ちにどう対応するか
  （自動承認ポリシーを持たせるか、人間へエスカレーションするか）は段階 1
  固有の新しい論点であり、CLI のプロトコル設計そのものとは別に決めるべき
  問い。

---

## 5. 未決事項リスト（相談のアジェンダ、依存関係の順）

1. **エンドポイントの居場所** — (a) Horizon アプリプロセス内にリスナーを
   置くか、(b) 将来のセッションデーモンを見越した設計を最初から取るか
   （2.B）。ここが決まらないと、契約を「1 接続前提」で作るか「複数接続前
   提」で作るかが決まらない。
2. **チャネルの形式** — Unix ソケット + JSON Lines（agentd と同型で
   `wire.rs`/`socket.rs` を再利用）にするか、別の技術（D-Bus, varlink,
   TCP+localhost）にするか（2.A）。1 の答えによらず影響するが、「再利用で
   きる資産があるか」の判断が先に要る。
3. **契約の形** — 既存の `wire.rs`（`Command`/`Event`/`Control`）をそのま
   ま拡張するのか、CLI 専用の薄い `Control` 系統を新設するのか。
4. **コマンド公開の粒度** — `CommandId` をそのまま晒すか、安定した外部コ
   マンド名の層を挟むか（2.C）。
5. **最初のマイルストーンで要る動作形** — 単発送信のみで始めるか、クエリ
   （セッション一覧等）まで含めるか、購読（イベントストリーム）まで最初
   から入れるか（2.D）。段階 1（委譲）のユースケースを見据えるなら、少な
   くともクエリは早期に要る可能性が高い（4 節）。
6. **インスタンス発見と複数起動** — ソケットパスの規約、安定版/dev 版のネ
   スト起動との共存方法（2.E）。
7. **新規セッション生成ペイロードの形** — 生成とメッセージ送信を分離した
   ままにするか、`--prompt` のような複合コマンドにするか（2.F）。
8. **認可・安全性のポリシー** — Unix ソケットのファイルパーミッションのみ
   に依拠するか、`CommandSpec.destructive` を使った追加の確認/許可リスト
   を設けるか（2.G）。
9. **（CLI 設計と独立だが波及する問い）委譲されたエージェントが承認待ちに
   遭遇した場合の扱い** — 自動承認ポリシーか、人間へのエスカレーションか
   （4 節）。CLI が監督フローの主動線になるなら、早めに触れておく価値があ
   る。
