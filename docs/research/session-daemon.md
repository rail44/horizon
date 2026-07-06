# セッションデーモン(tmux 型のセッション常駐化) — 設計相談のための材料集

セッション常駐化(「UI やエージェントを変えてもセッションが死なない」)の設
計相談のための調査メモ。**この文書は推奨を押し付けるものではない。**
`docs/research/cli-control-plane.md` と同じ形式 — 各論点を「事実 → 選択肢
→ 各選択肢の得失」で並べ、判断は所有者に委ねる。最後の「未決事項リスト」
が相談のアジェンダそのものになる想定。

前提として共有されている所有者の言葉(このメモの軸):

- UI やエージェントを変えてもセッションが死なないこと(デーモン化の動機)。
- 端末プロトコルの「頭脳」(エミュレーションコア)は1つのまま所有し、デー
  モン境界は VT バイト列の再解釈ではなく**自前契約(フレーム+コマンド)**
  で渡す — tmux 的な「各層で頭脳を再実装」への批判の文脈。
- 委譲されたエージェントチームの住処もデーモンになる(段階1と一体)。

---

## 1. 現状の棚卸し

### 1.1 agentd: 実績のある「デーモン化」の先例

`horizon-agentd`(`docs/agent-runtime-split-design.md`)は「tmux 型セッシ
ョンデーモンの胎児」と設計文書自身が明言する存在で、単なる構想ではなく
実装・dogfooding 済みの実績を持つ:

- 1 プロセスが全エージェントセッションを `SessionId` でホストし、Unix ソ
  ケット + JSONL envelope(`{v, session_id?, kind, payload}`、`kind` は
  `Command | Event | Control`)で通信する。
- **生存の実績**: `kill -9` でのセッション中断からの復帰(`resume_
  persisted_sessions` — 中断中のツール呼び出しを `cancelled` として合成
  し `TurnEnded(Cancelled)` を発行してから再開)、graceful drain(`Control::
  Drain` → `WriterHandle::flush()` を待ってから `exit(0)`)、`Reload Agent
  Runtime` コマンドによる drain→respawn→reconnect→`session_load` の一連
  が e2e テストで検証済み(`crates/horizon-agentd/tests/e2e.rs`)。
- **再接続の実績**: `hello` → `session_list` → `session_load` の順で、
  Horizon 再起動後に生きているセッションへ再接続し、フレームを再構築す
  る(`agent::agentd_runtime::reconnect_all_sessions`/`attach_sessions`)。
  未知のセッション(Horizon がまだ知らない、agentd 上で生き続けていたも
  の)は detached セッションとして「生存が可視化される」形で現れる。
- **既知の限界、設計時から明記**: 1 接続だけを相手にする(accept ループ
  は 1 接続を完全に処理し終えるまで次を受け付けない — 多重クライアント
  は設計上スコープ外)。「Out of scope here」節が明示的に列挙するのも
  「Terminals in the daemon(長期的な形。この分割はそれを妨げてはならな
  い)、MCP、multiple simultaneous clients」— つまり**ターミナルをデーモ
  ンに載せる話は agentd の設計時点で既に視野に入っていたが、意図的に対
  象外にされている**。

### 1.2 trust-boundaries.md 第3層: 撤回された「プロセス境界不要」判断

- 元々の記述(改訂前)は「PTY 出力の解釈とグリッド描画はレイテンシ・帯域
  に敏感なので wasm 境界はもちろん、プロセス境界も見送る」という判断だ
  った。
- 2026-07-04 に撤回: 「daily-driver 要件(UI を再起動してもセッションが
  死なない)の前ではその見送りは成立しない」— tmux 型分割(セッションデ
  ーモンが PTY(とエージェントランタイム)を所有し、UI は自由に再起動で
  きる使い捨てクライアントになる)を指し示す、と明記されている。
- 重要な区別: これは「ホットリロードのための議論」(第2層 = agentd の動
  機)ではなく「**セッション生存のための議論**」であり、根拠が異なる。第
  2層は「エージェントコードを変えてもセッションを殺さない」、第3層は
  「UI プロセス自体が落ちても(またはアップデートされても)セッションを
  殺さない」— 両方ともデーモン化に行き着くが、動機の系譜は別。
- 「委譲マイルストーンと一緒に設計する長期的な形」「デーモンは委譲された
  エージェントセッションの自然な住処でもある」と明記されており、
  `docs/roadmap.md` の共有基盤4「Inter-agent messaging + session daemon」
  に直結する。撤回されるまでの間の運用は「安定版の上に dev 版をネストし
  て起動する」— 今のプロセス境界不在ゆえの回避策で、`cli-control-
  plane.md` 2.E が扱う複数インスタンス発見の話と地続き。

### 1.3 cli-control-plane-design.md: 「契約を先に固定し、端点を後で動かす」の二つ目の実例

agentd に続き、CLI 制御面も同じ発想を明示的に採用した実装例が既にある
(決定済み、実装未着手 — `docs/cli-control-plane-design.md` 冒頭)。セッ
ションデーモン設計にそのまま参照できる先例:

- 「Endpoint: in-process now, contract written for the daemon later」—
  リスナーは今の Horizon 本体プロセスに置くが、契約はセッションデーモン
  へ端点を移しても壊れないよう書く: セッションは常に `SessionId` のみで
  参照する、契約は複数同時クライアントを前提にする(agentd の「1接続だ
  け」という単純化は意図的に継承しない)。
- discovery: 固定の well-known ソケットパス(`$XDG_RUNTIME_DIR/horizon/
  control.sock`、agentd と同じ流儀)+ `HORIZON_SOCKET` によるペイン単位
  の環境変数上書き(ネストした dev インスタンスが自分のペインに対して変
  数を上書きする、という形で安定版/dev 版の共存を解く)。
- reversibility audit(承認条件として実施済み)は明文で「Nothing regresses
  the session-daemon future(the contract is designed for the move)」と
  結論している — つまり CLI 側の設計判断は、セッションデーモンが実際に
  どんな形になっても壊れないことを既に検証済みという前提でこのメモは書
  かれている。

### 1.4 Registry / Frames: 現在セッション状態を握っているもの

- `Registry`(`src/session/registry.rs`)は `HashMap<SessionId,
  Sender<TerminalCommand>>`(terminals)と `HashMap<SessionId,
  SessionHandle>`(agents)を持つだけの素朴な構造体。Horizon プロセス内
  完結、crossbeam channel を持つだけで非同期性もシリアライズ境界も一切
  前提にしていない。`agents` 側の `SessionHandle` は、段階4以降は実体と
  して agentd 接続への配線(`agent::agentd_runtime`)であり、in-process
  実行パスは既に退役済み(`docs/agent-runtime-split-design.md` Step4
  「In-process retirement」)。
- `Frames`(`src/session/frames.rs`)は `HashMap<SessionId, TerminalFrame>`
  + `HashMap<SessionId, AgentFrame>` + 状態滞在時間のサイドカー
  (`agent_state_entries`)。UI が実際に描画する対象そのもので、既に
  `SessionId` だけをキーにしている(トランスポートに依存しない安定キー
  という、agentd 自身の原則と同型)。
- Terminal 側は Frames/Registry のどちらも Horizon プロセス外に永続化さ
  れない: detached ターミナルセッションは Horizon プロセスが生きている
  間だけ生存し、Horizon 再起動で失われる。これはエージェントセッション
  (agentd 経由で再接続・再構築される)との非対称そのものであり、「セッ
  ション生存」という動機が今まさに満たされていない領域を具体的に指し示
  す。

### 1.5 端末セッションランタイム: PTY 所有・エミュレーションコアの構造

- `TerminalSession::spawn`(`src/terminal/session.rs:39-121`)はセッショ
  ンごとに OS スレッドを3本立てる: PTY reader(`read_pty` — 64KiB バッ
  ファで `read(2)`)、core owner(`run_terminal_core` — `TerminalCore` を
  所有する、いわば「頭脳」担当スレッド)、PTY writer(`run_writer` —
  `MasterPty` を所有し、resize の重複排除も行う)。三者は crossbeam
  channel で結ばれ、Registry/Frames へは floem の ext_event ブリッジ経由
  で反映される(`cli-control-plane.md` 1.1 で既出の橋渡しパターンと同
  型)。
- `TerminalCore`(`src/terminal/core.rs`)は alacritty_terminal の `Term`
  をラップし、VT バイト列のデコードとキーボード入力のエンコード
  (termwiz 経由、kitty プロトコル込み)を行う — 所有者の言う「頭脳」が
  実体として指すコード。
- **60fps 級のコアレシング機構が既に存在する**: `notify_snapshot`/
  `flush_snapshot`(`src/terminal/session/runtime.rs:69-148`)は PTY バー
  ストを ~60Hz(`COALESCE_WINDOW = 16ms`)の `TerminalUpdate::Snapshot
  (TerminalFrame)` 送出へまとめる。単発キー入力は即時反映(レイテンシを
  待たせない)、アイドル時は追加の wakeup なし。コード中のドキュメント
  コメントが明言: 「This is session-runtime-local by design: it is the
  shape a future session daemon would use to decide what to stream over
  a socket」(`runtime.rs:82-84`)— つまりこのコードは自分自身を「将来デ
  ーモンがソケット越しに何をどう流すかの雛形」だと既に名指ししている。
- **今のスナップショットは全量再構築であり、差分ではない。**
  `TerminalCore::snapshot_frame` → `core::render::snapshot_frame`
  (`src/terminal/core/render.rs:13-`)は毎回 alacritty の
  `renderable_content()` から可視行すべてを組み立て直す。`TerminalFrame
  { text, lines: Vec<TerminalLine>, cursor, mouse_reporting }` は毎回グ
  リッド全体を運ぶ。`TerminalFrame`/`TerminalLine` に `Serialize`/
  `Deserialize` は今のところ付いていない(in-process の crossbeam channel
  越しにしか流れたことがないため)。
- **今存在する「diff」はクライアント側・転送境界の外にある。**
  `src/terminal/view/layout.rs` の `update_line_layouts` は前回・今回の
  `TerminalFrame.lines` を行単位で `PartialEq` 比較し、変化した行だけ
  `TextLayout` を再構築する(`TerminalLine` は既に `PartialEq` を導出済
  み)。これは明示的に「alacritty のダメージ追跡を snapshot/session 境
  界まで配線するより小さい選択」と書かれている(`layout.rs:41-49`)。こ
  の diff は一度もプロセス/ソケット境界を越えたことがなく、floem のビュ
  ー内部、Frames より下流にしか存在しない。

---

## 2. 設計の選択肢空間

### 2.A 何がデーモンへ移るか: PTY+コアごとか、PTY のみか

| 選択肢 | 得失 |
|---|---|
| **PTY + `TerminalCore`(頭脳ごと)** | agentd 自身の前例と対称(agentd は providers/tools/persistence という「エージェントの頭脳」全体を子プロセスへ移した — 一部だけ残す発想は取らなかった)。所有者の明言「頭脳は1つのまま所有」とも整合する。コスト: `TerminalCore`/alacritty_terminal/termwiz の kitty エンコードは現状 `crates/horizon-agent` のような独立クレートになっていない(`src/terminal` 直下) — agentd の Step1(クレート分割)に相当する下準備がまだ何もない状態からの移行になる。 |
| **PTY のみ(VT 解釈は Horizon = UI プロセスに残す)** | 移動量は小さい(reader/writer スレッドだけがプロセス境界を越え、core スレッドはそのまま)。ただし、これは所有者が既に名指しで批判している「tmux 的な、各層でバイト列を再解釈するパターン」そのものを別の境界で再現することになる。さらに実務上の欠陥: UI(=`TerminalCore` を持つプロセス)が落ちれば、画面状態(カーソル位置・スクロールバック・現在のグリッド内容)自体が失われる — 「セッションが死なない」という動機の核(子プロセス=シェルが生きている、だけでなく画面状態も生きている)を満たさない。移行の最終形としてではなく、あくまで中間ステップとして評価すべき事実。 |

### 2.B agentd との関係: 統合・拡張・並置

- **agentd を拡張する**(1つのデーモンが端末とエージェント両方を持つよ
  う育てる)— `docs/agent-runtime-split-design.md` の decision 1 が既に
  この方向を明言している: 「It is the embryo of the long-term tmux-style
  session daemon — when that lands, this process grows PTY ownership
  rather than being rewritten.」実装での検証はまだない(設計文書上の見
  通しのみ)。利点: ソケットパス発見規約・`Hello{binary_id, capabilities}`
  ハンドシェイク・drain/reconnect の仕組みを作り直さずに済む。既に
  daily-driver で動いている運用実績(kill -9 耐性、graceful drain)を転
  用できる。
- **並置する**(`terminald` + `agentd` を別プロセスとして持つ)— 利点:
  障害ドメインを分離できる(alacritty_terminal/portable-pty のバグと
  rig-core/duckdb のバグが互いのセッション種別を道連れにしない)。コス
  ト: ソケット・ライフサイクル・「再接続」フローが二重化する。`SessionId`
  はエージェント側 `contract::SessionId` と Horizon 側 `session::
  SessionId` が既に `Uuid` 経由で相互変換される同一の識別子空間であり
  (`docs/agent-runtime-split-design.md` Step1 実装ノート)、Registry が
  今1つの型で terminals/agents 両方を持っている実態とも噛み合わない
  (「セッション」という概念が2つの所有者に分裂する)。
- **統合(新規デーモンをゼロから起こす)** — 機能的には「拡張」とほぼ同
  型だが、agentd という名前・バイナリ・ソケットパスを引き継ぐか新設する
  かという命名/バージョニング上の判断が主な違いになる。`Control::Hello`
  の `binary_id` フィールドは元々「相手が期待するバイナリと違う」ことを
  検知するために存在する(Step2 実装ノート)— 既存プロセスをその場で育
  てる場合はこの検知対象がそのまま拡張される。

### 2.C 契約の形

- `wire.rs`(`Envelope{v, session_id?, kind, payload}`、`Command | Event
  | Control`)は既にセッション種別に依存しない形をしている
  (`kind`/`session_id` はドメイン非依存)。一方、`horizon_agent::contract`
  の `Command`(`Initialize`/`UserMessage`/`Cancel`/`ApproveToolCall`/
  `DenyToolCall`/`ToolCallResult`/`Shutdown`)と `Event`(`StateChanged`/
  `MessageCommitted`/`ToolCallRequested`/`ApprovalRequested`/`TurnEnded`
  ...)はエージェント形状専用に設計されており、`src/terminal/session/
  contract.rs` の `TerminalCommand`(`Input`/`Key`/`Resize`/`Scroll`/
  `Mouse`/`Selection...`)・`TerminalUpdate`(`Snapshot`/`Title`/`Bell`/
  `Clipboard`/`Exited`/`Error`)とは構造的に別物 — 元から同じ enum を共
  有する設計にはなっていない。
- `docs/cli-control-plane-design.md` 自身が「契約の形」節で採った選択
  (「shared framing, sibling vocabulary」— envelope は共有するが CLI 独
  自の語彙は `horizon_agent::contract` の `Command`/`Event` に相乗りせ
  ず、別立ての姉妹契約にする)が、そのままここに転用できる先例になって
  いる。
- 得失: envelope 共有+ドメインごとの姉妹語彙は、既に実証済みのコード
  (`wire.rs`/`socket.rs`/hello ハンドシェイク)の再利用を最大化しつつ、
  「エージェントの provider 契約にワークスペース制御を持ち込まない」と
  いう既存の境界規律(AGENTS.md「モジュール内部はクレート内に閉じる」)
  も保つ。対して単一の統合 `Command`/`Event` enum(セッション種別ごとに
  match で分岐)は「1箇所で経路分岐できる」利点と引き換えに、エージェン
  ト契約のコンパイル単位が端末の形状を(あるいはその逆を)知ることにな
  る。

### 2.D フレーム転送の帯域・頻度: 差分か全量か

- 事実(1.5): 現状は全量再構築が 60fps 級でコアレシングされている(差分
  は floem ビュー内部にしか存在しない、境界を越えたことがない)。
- PTY+コアが丸ごとデーモンへ移る場合(2.A のオプション1)、ソケット境界
  は今まさに `TerminalUpdate::Snapshot(TerminalFrame)` が流れているチャ
  ネル送出点(セッションランタイムスレッド → in-process の受け手)と同
  じ場所に位置することになる — 16ms ウィンドウ・dirty フラグ・「アイド
  ル時は追加 wakeup なし」という既存のレート制御は、ペイロードが全量か
  差分かに関わらずそのまま転用できるトランスポート非依存の仕組み。
- ソケットを越える際のペイロード形の選択肢:
  1. **`TerminalFrame` を毎回まるごと送る** — 最小の実装コスト。ただし
     crossbeam channel(シリアライズコスト実質ゼロ、同一プロセスのメモ
     リ共有)と違い、ソケット越しでは行数×列数×(前景色+背景色+文字)が
     60Hz で流れ続ける — アイドルに見えて再描画し続けるペイン(例: 大き
     な `htop`)では帯域が非自明になり得る、in-process では起きなかっ
     た種類のコスト。
  2. **デーモン側が前回スナップショットとの行単位比較を行い、変化した行
     だけを送る** — `layout.rs` が既に持つ「`TerminalLine` の `PartialEq`
     による行比較」という**証明済みのロジックを境界の反対側で走らせる**
     だけで済む可能性がある。agentd の Step3(「`process_agent_provider_
     event` reused as-is, unchanged」— ロジックは変えず実行場所だけ動か
     した)と同型のパターン。
  3. **alacritty_terminal 自体のダメージ追跡を配線する真の差分** —
     `layout.rs` が「core/session/contract 層まで配線するのは見送り」と
     明示的に退けた、より重いオプション(`layout.rs:46-49`)。ただしこれ
     は client-side diff についての判断であり、ソケット越しでは境界を跨
     ぐこと自体が無料でなくなる分、損得計算が変わる可能性がある。
  4. **push か pull か** — 3節の通り、「フレームを運ぶ」設計(wezterm)
     は必ずしもサーバー起点の逐次差分 push を意味しない(`GetLines` は
     クライアント要求に応じる pull 型 RPC)。Horizon の現行モデル(セッ
     ション側が能動的に Snapshot を送る push 型)を踏襲するか、pull 型
     に寄せるかは独立した軸として残る。

### 2.E 生存・再接続の意味論

- agentd の前例のうち直接転用できるもの: hello を遅い resume から切り
  離すレディネス設計(`SkippedLines`/段階的ステータス)、drain 前にログ
  を flush してから終了する保証(グレースフルな drain は書き込みを一切
  失わない)、`session_load`/`reconnect_all_sessions` の冪等な再接続(既
  知のセッションは handle だけ差し替え、未知のセッションは detached と
  して新規に可視化)。
- ただし terminal と agent はそもそも「状態」の形が違う: エージェントの
  状態は追記専用イベントログであり、replay は「記録済みイベントを再生す
  る」ことを意味する。ターミナルの「本当の状態」は「今画面に映っている
  もの」であって逐語的なトランスクリプトではない(スクロールバック分は
  別)— 再接続時の「replay」は「イベント列の再生」ではなく「最新1枚の
  Snapshot を送り直す」に近い形になりそうだ、という状態モデルの違いその
  ものが未決の論点。
- **デーモン自身の更新と、生きている PTY の両立** — agentd の drain 前
  例(`Reload Agent Runtime`: drain → 再構築されたバイナリを再起動 →
  reconnect)が terminal 側の「Reload Terminal Runtime」にそのまま転用で
  きるかは自明ではない。agentd の状態は「追記専用ログ + インメモリ再構
  築」であり、プロセスが死んでも再起動後にログから復元できれば済む。し
  かし PTY のマスター fd と、その先で動く子プロセス(シェル、vim、他の
  CLI ツール)は今の `portable-pty` の使い方(`TerminalSession::spawn`)
  ではデーモンプロセスと生死を共にする — バイナリだけ入れ替えて PTY と
  子プロセスを生かし続けるには fd の exec 越しの引き継ぎ(`execve` re-
  exec、systemd 的なソケット活性化に類する仕組み)が要る。agentd の
  drain 実績が一度も証明したことのない、質的に異なる信頼性要件で、**お
  そらくこのメモ全体で最大の新規未知数。**
- **複数 UI クライアント同時接続** — agentd は「1接続だけを相手にする」
  ことを設計時から明記済みの単純化(1.1)。tmux/zellij の実際の売りは
  「同一ペインへの複数クライアント同時アタッチ」であり、Horizon がこれ
  を望む場合(同じセッションを2つのウィンドウで見る、将来のヘッドレス
  ビューアがライブ観察する等)、「サーバー側で1回描画しクライアントごと
  に個別送信する」という 3節の zellij の設計(`ServerToClientMsg::
  Render` はクライアントID単位)が具体的な参照実装として存在する — 採
  用するかどうかは未決。

### 2.F 段階的な移行路

一括移行ではない道の候補(`cli-control-plane-design.md` 自身が「本体プ
ロセス内で今すぐ着手し、契約だけをデーモン後日移行に耐える形で書く」と
いう同型の道を既に選んだ実例):

1. **新規セッション種別だけデーモン住まいにする** — agentd がエージェン
   トセッションで既に実証済みのパターン。次に増える種別(例: 将来の
   WASM プラグインビュー)だけ最初からデーモン住まいにし、既存のターミ
   ナル/エージェントの移行と時間的に切り離す道もある(ただし `plugins/`
   はまだ app shell に配線されていない段階 — `docs/roadmap.md` "Later"
   節)。
2. **`control.sock` の端点移動を先にやる** — CLI の契約は既に「セッシ
   ョンデーモンへ端点が移っても壊れない」ことを前提に設計されている
   (`SessionId` のみで参照、複数クライアント前提、env var 経由の
   discovery)。CLI が先に「Horizon 本体プロセス内リスナー」として着地
   していれば、後日「実は相手がセッションデーモンだった」に切り替わっ
   てもクライアントは気づかない、という設計そのもの
   (`docs/cli-control-plane-design.md` "Endpoint" 節)。CLI が先に着地す
   ることが、セッションデーモンの存在が最初に露出する経路になり得る。
3. **agentd をそのまま育てる**(2.B「拡張」)— 新しいバイナリを起こさ
   ず、既に daily-driver で動いている agentd のプロセス生存・drain・
   reconnect の実績をそのまま転用し、セッション種別を1つ(terminal)増
   やすところから始める。ソケットパス・`binary_id` ハンドシェイクなど
   の発見規約を作り直さずに済む。
4. **部分的 PTY 移譲を先に試す**(2.A のオプション2寄り)— ただし 2.A
   で述べた通り、UI 再起動時に画面状態(`TerminalCore`)自体は失われる
   ため、「セッション生存」という動機の核を満たさない暫定策にとどまる
   — 移行の中間ステップとしてでなく最終形として選ぶと動機を裏切る、と
   いう点は移行順序を考える上で留意すべき事実。

---

## 3. 先行事例(Web 調査、各5行以内)

- **tmux server/client + control mode** — サーバーが PTY を所有し、ク
  ライアント接続の生死とは独立にセッションが生き続ける(server-owns-the-
  PTY)。control mode(`-CC`)は同じソケット上でテキストプロトコルに切り
  替わる拡張で、`%output` はバイト列をほぼ生のまま(制御文字だけ8進エス
  ケープ)転送する — **クライアント自身が VT エミュレータを実装/内蔵し
  て再解釈する**設計。つまり tmux は「バイト列を運ぶ」側の代表例。
- **wezterm mux** — SSH/Unix/TLS の「ドメイン」抽象でリモート/ローカル
  の接続方式を統一する。`GetLines` のような PDU ベース RPC でクライアン
  トが必要な範囲の行データを要求し、サーバーは構造化された行/セル情報を
  返す(クライアント側で VT バイト列を再解釈しない)— **「フレーム(構
  造化された行)を運ぶ」側の代表例**。GUI クライアントが直接描画に使え
  る形でサーバー側がレンダリングを担う点は、Horizon の `TerminalFrame`
  の立ち位置と近い。
- **zellij server/client** — サーバーが VTE 解釈・グリッド管理を担い、
  クライアントには `ServerToClientMsg::Render{content}` として既に描画
  済みの ANSI 文字列を送る(クライアントは stdout に書くだけ)。**フレー
  ムを運ぶが、その中身は「構造化データ」ではなく「サーバー側で再合成し
  た ANSI テキスト」** — zellij のクライアント自身が端末(TUI in TUI)だ
  からこそ成立する選択で、GUI クライアントの Horizon にはそのままは当
  てはまらない。
- **Eternal Terminal (et)** — tmux/wezterm/zellij と異なり「多重化」や
  「頭脳の所在」を扱う設計ではない。SSH でハンドシェイクした後は通常の
  シェル PTY をそのまま使い、et 自身の役割はネットワーク切断/ローミング
  を跨いで**再接続可能な信頼性層**(シーケンス番号付きバッファでの再送)
  を提供することに限定される — フレームもバイト列の再解釈も持ち込まな
  い、「輸送だけを差し替える」という第三の設計として並べておく価値があ
  る(参照した公式リポジトリの記述からは実装の詳細までは確認できておら
  ず、一般的に流布している設計理解に基づく)。

---

## 4. 段階1(委譲)との接続

- `docs/roadmap.md` の共有基盤4は「Inter-agent messaging + session
  daemon」を一体の項目として明記し、「designed together with the
  tmux-style session daemon per the standing agreement. The CLI control
  plane is the seam it grows from.」と書く。「Later」節でも「Inter-agent
  messaging: designed together with the session daemon — a project-level
  consultation comes first」と重ねて明記されている — つまりこのメモが
  扱うセッションデーモンの論点と、委譲(段階1)の設計は所有者の頭の中で
  既に一体のものとして扱われている。
- `docs/agent-runtime-split-design.md` の元々の5つの動機の1つが「give
  delegated agent sessions a home」であり、これは agentd がエージェント
  セッションについて既に満たしている。**満たされていないのは**「agentd
  はエージェント種別のセッションだけの住処であり、委譲先(delegate)が
  シェルを操作する必要がある場合の**ターミナルセッション**には、まだ同
  等の住処がない」という非対称 — これがセッションデーモンと委譲を結ぶ
  具体的な接続点: 監督エージェントが委譲先の**ターミナルペイン**を CLI
  制御面越しに監督するには、そのターミナルの状態が(エージェントセッシ
  ョンと同様に)`SessionId` で安定してアドレス可能である必要がある。
- `docs/cli-control-plane-design.md` の既に確定した設計(v1 は「Targets
  are explicit in v1」「固定ソケットパス」「`SessionId` のみでの参照」)
  は、セッションデーモンがどんな形に着地しても composable — CLI 側で既
  に済んだ設計判断は、ターミナルセッションがデーモンへ移ったとしても
  やり直しにならない、という既存の reversibility audit の結論がそのま
  ま繰り返し使える。

---

## 5. 未決事項リスト(相談のアジェンダ、依存関係の順)

1. **何がデーモンへ移るか** — PTY + `TerminalCore`(頭脳ごと)か、PTY
   のみか(2.A)。ここが「セッション生存」という動機そのものを満たすか
   どうかを左右する最上流の問い。
2. **agentd との関係** — 統合/拡張/並置(2.B)。agentd 自身の設計記録
   は既に「拡張」を明言しているが、実装レベルでの検証はまだない。
3. **契約の形** — `wire.rs` の Envelope を共有しつつ、`Command`/`Event`
   をどう分けるか: 単一の統合 enum か、CLI 契約と同型の「姉妹契約」か
   (2.C)。
4. **フレーム転送の帯域・頻度** — 全量か行単位の差分か、push か pull か
   (2.D)。既存の row-diff 資産(`layout.rs`)をどちらの側(ビュー/デー
   モン)で使うか。
5. **デーモン自身の更新と、生きている PTY の両立** — drain-then-respawn
   の間、生きている PTY(と子プロセス)をどう扱うか(2.E)。agentd の
   drain 前例がそのまま転用できない、最大の新規論点。
6. **複数 UI クライアント同時接続** — agentd の「1接続だけ」という単純
   化を引き継ぐか、zellij 型の複数クライアント fan-out を見据えるか
   (2.E)。
7. **段階的な移行路** — どこから着手するか: 新規セッション種別限定/
   `control.sock` 端点移動先行/agentd 拡張先行/部分的 PTY 移譲、のどれ
   から始めるか(2.F)。
8. **(セッションデーモンと独立だが波及する問い)委譲チームの「住処」と
   してのデーモン** — エージェントだけでなくターミナルも含めた委譲先セ
   ッションの生存要件(4節)。CLI 制御面が監督フローの主動線になるなら、
   早めに触れておく価値がある。
