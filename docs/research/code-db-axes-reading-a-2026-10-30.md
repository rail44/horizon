# 現在の木の軸の読み判定・A:size、complexity、interface、intent(2026-10-30)

コードデータベース([設計文書](../code-database-design.md)の「Rebuilt on SCIP」以降)が現在の木について出す品質の軸のうち、`size`・`complexity`・`interface`・`intent` の 4 つについて、既定の設定で上位 10 件を読み、障害かどうかを判定した。記録だけを行い、コード、SQL、Rust、設定は変えていない。`duplication`・`coupling`・`unreferenced`・`module_cycles` は別の読み手が並行して扱っており、ここでは読んでいない。

目的はオーナーの言葉で次のとおりである。refactor audit の改善(「refactor auditの仕組みをよりよくしたい」、2026-10-24)。構造と依存グラフを把握して候補を見つけ、判定する(2026-10-25)。品質は「継続的にメンテナンスをする上での障害の少なさや機能の足しやすさ」であり(サイズ、再利用性、意図の明確さ、ほかの軸)、好みではない。重複と置き場所は構造上の性質である(2026-10-28/29)。この評価の形(軸ごとに上位 10 件を下の規則で判定し、軸ごとの適合率と誤報の機構を出す)は、2026-10-30 にオーナーが「はい。お願いします。」と承認した(依頼文による)。

## 要約

| 軸 | 問題あり | 問題なし | 判断できない | 適合率@10 |
| --- | ---: | ---: | ---: | ---: |
| `size`(unit) | 2 | 8 | 0 | 0.2 |
| `complexity` | 0 | 10 | 0 | 0.0 |
| `interface` | 4 | 6 | 0 | 0.4 |
| `intent` | 1 | 9 | 0 | 0.1 |
| 合計 | 7 | 33 | 0 | 0.18 |

- `size` の 2 件は、どちらも長さそのものではなく、unit の中の写し(重複の性質)が費用を生んでいた。長さだけの 8 件は、データの表、網羅的な `match`、外部の実装の移植、線形の導出だった。
- `complexity` の上位 10 件のうち 8 件は、平らな `match` の腕と `?` が数の大半を占めていた。上位に、分岐の多さが次の変更を妨げている unit は無かった。
- `interface` の 4 件は 2 つの原因に帰着する。agentd のセッション起動の 6 つ組の値が 3 つの関数を通って運ばれること、rig の 1 ターンの入力が 4 つの層を通って運ばれることである。どちらも、引数を 1 つ足し引きした過去のコミットが全層を触っている。
- `intent` の TODO/FIXME の側は、この木の production では 0 件だった。上位はすべて「他の crate から参照され、文書の無い unit」で、1 件(`horizon-board` の `Comment`)だけが、型から読み取れない約束(`source` が冪等の鍵であること)を持っていた。
- 軸の外の性質で障害になっていたものとして、`apply_agent_event_to_frame`(`complexity` 7 位、問題なし)の turn を開く条件が、自身のコメントが「写し」だとする `TurnTracker` の条件から食い違っているのを見つけた(4 節)。

## 方法

### 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、変更の無い状態で `origin/code-quality-survey` の先端 d6299e9b に合わせた。測定はその使い捨ての複製(作業ツリーの `target/code-db/clone`、`git clone --no-checkout` の後に d6299e9b を checkout)で行った。機械はオーナーの対話用デスクトップで、すべて `nice -n 19` で動かした。

```sh
nice -n 19 duckdb-ext/build.sh                       # 複製の中で。code-db は複製の SQL を読む
nice -n 19 uv run --locked --script scripts/refactor-audit/run.py setup   # jscpd(index が要求する)
nice -n 19 duckdb-ext/code-db/target/release/code-db index CLONE
rm -rf CLONE/target/debug CLONE/preview-plugin/target/debug CLONE/duckdb-ext/target/debug CLONE/duckdb-ext/code-db/target/debug
code-db query CLONE size LIMIT=15            # 同様に complexity、interface、intent
code-db query CLONE size level=file LIMIT=10 # file と crate の段の様子
code-db sql CLONE measure/*.sql              # 補助の集計(コミットしていない)
```

`index` の時間は jscpd 2.6 s、rust-analyzer scip がルート 253.7 s、`duckdb-ext` 12.6 s、`duckdb-ext/code-db` 9.5 s、`duckdb-ext/host-check` 8.7 s、`preview-plugin` 101.1 s だった。`.code-db.toml` は無く、すべて既定の設定である。unit は 9,335(テストでないもの 5,464)、ファイルは 532 だった。

4 つのクエリはどれもテストの切り替え `include_tests` を持ち、既定の `false`(テストの unit を除く)で実行した。`size` は unit の段(既定)で判定した。

補助の集計として、上位の unit の範囲の中で `//` で始まる行(doc コメントを含む)の数と、`complexity` の `decisions` から `?` と 2 本目以降の腕を引いた数を出した。どちらも判定の根拠ではなく、5 節の機構の記述に使った。

### 判定の規則

依頼文の規則に従う。前の読み判定(3 回目まで)の規則は置き換えられている。

- **問題あり**:unit(またはモジュール)がその軸の性質をどう損ねているかと、それが次の変更や機能の追加のどこで障害になるかの両方を名指しできるもの。`ファイル:行` を引き、役に立つときは、それによって難しくなる具体的な変更を書く。
- **問題なし**:そのような障害を名指しできないもの。数が大きい理由(データの表、生成されたか宣言的な本体、コンパイラが網羅を強制する `match`、テストなど)を書く。
- **判断できない**:書かれていない意図に依存するもの。問いを書く。

好みでは判定しない。コードを書くのは AI なので、小さな重複や並べ方の好みは、変更の費用を生んでいない限り障害としない。費用を生んだかどうかは、該当の行に触れた過去のコミットで確かめた(`git log -S`、`git show`)。数の大きさは根拠にしていない。行番号は d6299e9b のものである。

---

## 1. `size`(unit の段、テストでない unit、loc の降順)

「注釈」は範囲の中の `//` で始まる行(先頭の doc コメントを含む)の数と、loc に対する割合である。

| # | unit | 種類 | loc | 注釈 | 判定 |
| --- | --- | --- | ---: | ---: | --- |
| 1 | `crates/horizon-agent/src/tools/catalog.rs` `build_definitions` | function | 344 | 35(10 %) | 問題なし |
| 2 | `src/workspace/render.rs` `WorkspaceShell.render_node` | method | 326 | 111(34 %) | 問題なし |
| 3 | `src/workspace/render.rs` `Render for WorkspaceShell.render` | method | 291 | 41(14 %) | 問題あり |
| 4 | `src/terminal/glyphs.rs` `box_draw_geometry` | function | 286 | 5(2 %) | 問題なし |
| 5 | `src/terminal/paint.rs` `paint_terminal` | function | 266 | 34(13 %) | 問題なし |
| 6 | `crates/horizon-agentd/src/session/run.rs` `run_session` | function | 253 | 32(13 %) | 問題なし |
| 7 | `crates/horizon-workspace/src/commands.rs` `core_commands` | function | 245 | 0 | 問題なし |
| 8 | `crates/horizon-agent/src/providers/mock.rs` `respond_to_user_message` | function | 241 | 37(15 %) | 問題あり(小) |
| 9 | `crates/horizon-agentd/src/hub/mapping.rs` `Mapper.map_event` | method | 233 | 0 | 問題なし |
| 10 | `src/theme/scheme.rs` `scheme_from` | function | 229 | 70(31 %) | 問題なし |

### 判定

1. **`build_definitions` — 問題なし。** ツールの定義の表で、`Definition` の値を並べた `vec!` 1 つである(`catalog.rs:25-381`)。ツールを 1 つ足すことは 1 つの要素を足すことで、長さはツールの数と説明文の長さによる。3 回目の読み判定(churn の 2 番)と同じ理由である。
2. **`WorkspaceShell.render_node` — 問題なし。** `LayoutNode` の 2 つの腕(ペイン `render.rs:546-666`、分割 `render.rs:667-872`)で、それぞれ 1 つの要素の木を組む。326 行のうち 111 行がコメントで、多くは見た目の決定の経緯(枠の太さ、scrim、光彩、flex の比の計算)である。同じ処理の写しは無く、分割の取っ手の要素を足したり変えたりする場所は 1 か所に決まる。
3. **`Render for WorkspaceShell.render` — 問題あり。** シェルの根の描画で、4 つのリストのモーダル(パレット、ビューの選択、セッションの管理、モデルの選択)の重ね描きが、同じ形の約 40 行の写しとして並ぶ(`render.rs:1112-1152`、1153-1193、1194-1245、1246-1287)。違うのは id、幅と高さ、取り消しの handler、セッションの管理だけの key context と 2 つの action である。この写しは費用を生んだ。1c27f1ab(2026-08-05)は、行のクリックでモーダルが閉じる不具合を 4 か所に同じ `on_mouse_down(… stop_propagation)` と注釈を足して直した。モーダルを 1 つ足すと、ここの重ね描きをもう 1 つ写すことになる。3 回目の読み判定(churn の 8 番)と同じ原因で、障害は長さではなく写し(重複の性質)にある(4 節)。
4. **`box_draw_geometry` — 問題なし。** 罫線素片の文字を、Ghostty の `linesChar` の辺の置き方に従って矩形に変える(`glyphs.rs:423-722`、doc が出所を書く)。上下左右の 4 つの腕の処理が対称に並ぶ。対象は Unicode の U+2500..U+257F に閉じており、このファイルの変更は作成(7ad0e9b2)と移動(1b15a6a5)の 2 コミットだけである。
5. **`paint_terminal` — 問題なし。** 1 フレームの描画を、寸法の計算とリサイズの送信、スクロールバックの経路(`paint.rs:182-213`、`paint_scrollback_window` に出してある)、行の描画(237-268)、選択(280-312)、IME の前編集(314-348)、カーソル(350-389)の順に並べる。層は描く順に一度ずつ現れ、各層に理由のコメントがある。スクロールバックの経路と行の描画のループは似た形(`paint.rs:79-107` と 237-268)だが、費用を生んだ記録は無い(`paint.rs` のコミットは 1b15a6a5 の 1 つ)。
6. **`run_session` — 問題なし。** セッションの準備、事象のループ、後始末を 1 つの関数に持つ(`run.rs:36-296`)。ループの各腕は `handle_provider_event`、`fold_tool_completion`、`dispatch_inbound_command` に委ね、環境の構築は 4fe9f4fd で外に出された後である。3 回目の読み判定(churn の 1 番)と同じ。引数の数は `interface` で別に判定した(2 節の 2 番、問題あり)。
7. **`core_commands` — 問題なし。** コマンドの仕様(`CommandSpec`)の表である(`horizon-workspace/src/commands.rs:144-388`)。
8. **`respond_to_user_message` — 問題あり(小)。** mock provider の、プロンプトの部分文字列で選ぶテスト用の筋書きの一覧である(`mock.rs:241-481`)。ツールの呼び出しを要求する筋書きのうち 3 つは補助関数 `request_tool`(`mock.rs:484-506`)を使い、5 つは同じ 15 行前後を手で書いている(`mock.rs:331-348`、349-374、375-398、399-419、420-437)。この写しは費用を生んだ。06a52e22(2026-09-25「Require execution identity throughout tool lifecycle」)は `occurrence_id` を写しのそれぞれに足した(このファイルに 16 行の `occurrence_id` の追加)。補助関数はその後 720cf24f で足され、古い写しは置き換えられていない。ツールの要求の形がまた変わると、同じ 6 か所を直すことになる。障害は長さではなく写しにある(4 節)。テストの筋書きを production の registry に置いた unit を production として数えていることは 5 節に書いた。
9. **`Mapper.map_event` — 問題なし。** 内部の `Event` を ACP の更新へ写す、コンパイラが網羅を強制する `match` である(`mapping.rs:253-485`)。事象を 1 つ足すと、ここに腕が無いとコンパイルが通らない。
10. **`scheme_from` — 問題なし。** 設定の種(6 つの色相、背景、文字の対比)から約 30 の役割の色を導く、一度ずつ書かれた線形の導出である(`scheme.rs:333-566`)。役割を 1 つ足すことは、1 つの `let` と `Scheme` の 1 つの欄を足すことになる。229 行のうち 70 行は、導出の理由と経緯のコメントである。

### file と crate の段(判定していない)

file の段の上位 10 件は `event_log/writer.rs` 1,687、`session/resume.rs` 1,552、`terminal/scrollback.rs` 1,529、`terminal/glyphs.rs` 1,497、`workspace/render.rs` 1,407、`terminal-core/session_loop.rs` 1,376、`judge/mod.rs` 1,209、`workspace/commands.rs` 1,203、`agent/contract.rs` 1,195、`agent/view/adapter.rs` 1,118 である。`include_tests := false` が外すのは、モジュール自体がテストであるファイルだけで、ファイルの中の `#[cfg(test)] mod tests` の行は loc に残る。中のテストの行を引くと、`session_loop.rs` は 1,376 のうち 981、`resume.rs` は 1,552 のうち 969 がテストで、テストを除いた loc の順位は 57 位と 28 位になる。テストを除いた順で上位にとどまるのは `render.rs`(1,249、1 位)、`glyphs.rs`(1,175、2 位)、`commands.rs`(1,059、3 位)、`contract.rs`(1,051、5 位)、`writer.rs`(904、6 位)だった。

crate の段は `horizon-agent` 62,209、`horizon` 41,737、`horizon-agentd` 16,907、`horizon-terminal-core` 9,092 の順で、`test_loc_pct` は多くの crate で 50〜60 % である(`horizon` 31.8 %)。

## 2. `interface`(テストでない関数とメソッド、引数の数の降順)

「引数」は `self` を含む。8 引数の関数は 7 つあり、9・10 位は `fan_in_crates` の降順、次にパスの順で決まった(`fan_in_crates` が 1 の `run` が 9 位、残り 6 つのうちパスの順で最初の `run_writer_loop` が 10 位)。10 件すべてが `#[allow(clippy::too_many_arguments)]` を持つ。

| # | unit | 引数 | 可視性 | fan-in(crate) | loc | 判定 |
| --- | --- | ---: | --- | ---: | ---: | --- |
| 1 | `src/terminal/paint.rs` `paint_scrollback_window` | 15 | private | 0 | 57 | 問題なし |
| 2 | `crates/horizon-agentd/src/session/run.rs` `run_session` | 12 | pub(super) | 0 | 253 | 問題あり |
| 3 | `crates/horizon-agent/src/providers/rig/completion.rs` `complete_rig_turn` | 10 | pub(super) | 0 | 169 | 問題あり |
| 4 | `crates/horizon-agent/src/providers/rig/session/state.rs` `SessionLoopState.new` | 10 | pub(super) | 0 | 54 | 問題なし |
| 5 | `crates/horizon-agent/src/providers/rig/completion.rs` `run_provider_stream` | 9 | private | 0 | 68 | 問題あり |
| 6 | `crates/horizon-agent/src/tools/bash/exec/sandboxed.rs` `run_sandboxed` | 9 | pub(in …) | 0 | 115 | 問題なし |
| 7 | `crates/horizon-agentd/src/session/spawn.rs` `spawn_session_thread_with_context` | 9 | pub(super) | 0 | 126 | 問題あり |
| 8 | `src/workspace/session_creation.rs` `WorkspaceShell.control_plane_new_session` | 9 | pub(crate) | 0 | 66 | 問題なし |
| 9 | `crates/horizon-cli/src/lib.rs` `run` | 8 | pub | 1 | 143 | 問題なし |
| 10 | `crates/horizon-agent/src/persistence/event_log/writer.rs` `run_writer_loop` | 8 | private | 0 | 117 | 問題なし |

### 判定

1. **`paint_scrollback_window` — 問題なし。** 呼び手は `paint_terminal` の 1 か所だけで(`paint.rs:192-208`)、引数はその呼び手が持つフレームの描画の文脈(寸法、フォント、色の上書き、shaping のキャッシュ、`window`、`cx`)である。描画の入力を 1 つ足すと、この署名と 1 つの呼び出しを直すことになるが、コンパイラがその 2 か所を示す。費用を生んだ記録は無い。
2. **`run_session` — 問題あり。** セッションの起動の属性 6 つ(`session_id`、`provider_id`、`role_id`、`workspace_root`、`spawn_source_session_id`、`restored_worktree`)が、`spawn_session_thread`(8 引数、`spawn.rs:111-133`)→ `spawn_session_thread_with_context`(9 引数、`spawn.rs:135-261`)→ `run_session`(12 引数、`run.rs:47-60`)の 3 つの署名を、まとまりにされないまま通る。外側の 2 つの関数は 8 か所から呼ばれる(`board_flow/operations.rs:179`、333、`session/connection.rs:167`、349、359、`session/exploration.rs:47`、`session/resume.rs:276`、509)。起動の属性を 1 つ足すか除く変更は、3 つの署名、2 つの受け渡し、8 つの呼び出しを触る。実際に d7a3046a(2026-10-19「Remove spawn-time worktree isolation」)は `isolate: bool` を除くために 3 つの署名と受け渡しと `board_flow/operations.rs`、`connection.rs`、`exploration.rs`、`resume.rs` の呼び出しを直した。
3. **`complete_rig_turn` — 問題あり(5 番と同じ原因)。** rig の 1 ターンの入力のうち `config`、`environment`、`extra_sections`、`events_tx`、`token` が、`complete_rig_turn`(`completion.rs:144-313`)→ `rig_provider_turn_with_retry`(7 引数、`completion.rs:341-349`)→ `rig_provider_turn_streaming`(8 引数、`completion.rs:415-438`)→ `run_provider_stream`(9 引数、`completion.rs:441-508`)の 4 つの層を通る。`environment` と `extra_sections` を読むのは最下層の `system_prompt(environment, extra_sections)`(`completion.rs:469`)だけである。システムプロンプトに入力を 1 つ足すと、4 つの署名、3 つの受け渡し、テストの呼び出し(`completion/tests.rs` の `complete_rig_turn` 7 か所、`run_provider_stream` 2 か所)を触る。8e45bb64(2026-07-06「Load repository instruction files…」)が `extra_sections` を足したときに、各層の署名と受け渡しに 1 行ずつ足している。`complete_rig_turn` の唯一の production の呼び手(`session/turn.rs:188-199`)が `self` の欄を別々に借りて渡すのは、`select!` のループが `self.commands` を同時に使うためで、それ自体は借用の分割として必要である。
4. **`SessionLoopState.new` — 問題なし。** 引数がほぼそのまま欄になるコンストラクタで(`state.rs:108-161`)、production の呼び手は `session/mod.rs:185` の 1 か所である。
5. **`run_provider_stream` — 問題あり(3 番と同じ原因)。** 上の 4 つの層の最下層で、`environment` と `extra_sections` を使うのはここだけである(`completion.rs:463-469`)。
6. **`run_sandboxed` — 問題なし。** 呼び手は `bash/job.rs:121-137` の 1 か所で、引数はその呼び手が持つ `BashJob` と `SandboxedRun` の欄を開いたものである。サンドボックスの入力を足すと `SandboxedRun` とこの署名の 2 か所になるが、費用を生んだ記録は確かめた範囲に無い。
7. **`spawn_session_thread_with_context` — 問題あり(2 番と同じ原因)。** 起動の 6 つ組を受け取り、`run_session` に渡す中間の層である(`spawn.rs:135-261`)。
8. **`WorkspaceShell.control_plane_new_session` — 問題なし。** 9 のうち 3 つは `&mut self`、`window`、`cx` で、残り 6 つは制御面の `Command::NewSession` の欄(`control_plane/invoke.rs:12-19`)をそのまま受ける。呼び手は `control_plane.rs:161` の 1 か所である。
9. **`horizon_cli::run` — 問題なし。** argv、2 つの環境変数、標準入出力、tty の判定、確認の質問を注入で受けるのは、テストが本物のプロセスの状態に触れずに終了コードと出力を確かめるためで、doc がそう書いている(`lib.rs:26-58`)。呼び手は `src/entry.rs:37` と統合テストである。
10. **`run_writer_loop` — 問題なし。** 書き手のスレッドの本体で、引数はループが所有して持ち回る状態(ファイル、受信口、次の連番、ログの位置、DuckDB の store、再構築)である(`writer.rs:767-885`)。呼び手は同じファイルの 2 か所(275、755)だけである。

## 3. `intent`(TODO/FIXME のある unit、または文書が無く他の crate から参照される unit)

この木の production には TODO/FIXME の行が無い。リポジトリ全体の 6 行はすべて `crates/horizon-agent/src/tools/tests.rs` のテストデータ(grep の対象の文字列)である。`intent` の production の 242 行はすべて後半の条件(文書が無く、`fan_in_crates` > 0)による。

| # | unit | 種類 | 可視性 | fan-in(crate / ファイル) | 判定 |
| --- | --- | --- | --- | --- | --- |
| 1 | `crates/horizon-wire/src/session.rs` `SessionId.new` | method | pub | 4 / 65 | 問題なし |
| 2 | `crates/horizon-board/src/model.rs` `Item` | type | pub | 4 / 22 | 問題なし |
| 3 | `crates/horizon-board/src/model.rs` `Comment` | type | pub | 4 / 15 | 問題あり |
| 4 | `crates/horizon-wire/src/session.rs` `SessionId.as_uuid` | method | pub | 3 / 28 | 問題なし |
| 5 | `crates/horizon-wire/src/session.rs` `SessionId.from_uuid` | method | pub | 3 / 10 | 問題なし |
| 6 | `crates/horizon-acp/src/ext.rs` `ToolCallProgressNotification` | type | pub | 3 / 5 | 問題なし |
| 7 | `crates/horizon-acp/src/ext.rs` `MemoryNotification` | type | pub | 3 / 4 | 問題なし |
| 8 | `crates/horizon-acp/src/ext.rs` `ProviderRequestNotification` | type | pub | 3 / 4 | 問題なし |
| 9 | `crates/horizon-agent/src/contract.rs` `contract` | module | pub | 2 / 144 | 問題なし |
| 10 | `crates/horizon-agent/src/contract.rs` `ProviderId` | type | pub | 2 / 29 | 問題なし |

### 判定

1. **`SessionId.new` — 問題なし。** `Self(Uuid::new_v4())` の 1 行で(`session.rs:27-29`)、型自身の doc(`session.rs:15-22`)が UUID の newtype であることを書いている。
2. **`Item` — 問題なし。** ボードのタスクの型で、欄の名前が使い方を表している(`model.rs:14-30`)。名前から読めない約束である「閉じているか」と自由文の `status` の関係は、欄の doc が書いている(`model.rs:20-21`)。`rank` は文字列の並びの鍵で、書くのは logd の `compute_rank`(`horizon-logd/src/writer/relationships.rs:10`)だけ、他は並べるだけなので、型を読む書き手が形式を知る必要は無い。
3. **`Comment` — 問題あり。** ボードの投稿の型で、欄に doc が無い(`model.rs:6-13`)。名前から読めない約束が 2 つある。`source` は投稿の冪等の鍵で、logd は同じ `source` を持つ既存の投稿があれば重複として扱い、内容が違えば拒む(`horizon-logd/src/writer/messages.rs:13-24`)。agentd の配送はこれを使って同じ通知の二重の投稿を防いでいる(`horizon-agentd/src/board_flow/dispatch.rs:186`、335)。`at` の単位はミリ秒で、それを決めているのは logd の書き手(`horizon-logd/src/writer/prepare.rs:84`)である。投稿を作る新しい経路(たとえば別の配送元)を足す人は、型からは `source` を安定な鍵にすべきこと、`None` では重複を防げないことを知ることができず、logd の実装を読む必要がある。
4. **`SessionId.as_uuid` — 問題なし。** 名前と署名がすべてを言う取り出し(`session.rs:31-33`)。
5. **`SessionId.from_uuid` — 問題なし。** 同じく包み直し(`session.rs:35-37`)。
6. **`ToolCallProgressNotification` — 問題なし。** `session_id` と平らに展開した事象だけの封筒で、`#[notification(method = "_horizon/tool_call_progress")]` が method を名指しし、中身の `ToolCallProgressEvent` に出所を示す doc がある(`ext.rs:142-163`)。
7. **`MemoryNotification` — 問題なし。** 同じ形の封筒で、中身の `MemoryEvent` 以下の各型に doc がある(`ext.rs:167-180`)。
8. **`ProviderRequestNotification` — 問題なし。** 同じ形の封筒で、中身の `ProviderRequestEvent` に doc がある(`ext.rs:282-297`)。
9. **`contract` モジュール — 問題なし。** モジュールの doc(`//!`)は無いが、名前から読めない項目には個別の doc がある(`SessionId` の再公開の理由 `contract.rs:11-17`、`OccurrenceId` の 2 つの鍵の理由 `contract.rs:35-64` など)。fan-in の 2 crate・144 ファイルは、モジュールの中身すべての利用者の和で、モジュールの文書の必要を測ったものではない(5 節)。
10. **`ProviderId` — 問題なし(軸の外の障害あり、4 節)。** `ProviderId(pub String)` 自体に doc は無いが、値の形式(`builtin.agent.rig.<name>`、既定の `builtin.agent.rig`、`builtin.agent.mock`)は、それを作る `named_rig_provider_id` の doc が書いている(`registry.rs:52-60`)。

## 4. 別の軸の性質による障害

上位に入った軸とは別の性質で障害になっていたもの。

- **`Render for WorkspaceShell.render`(`size` 3 位)と `respond_to_user_message`(`size` 8 位)。** 障害は長さではなく、unit の中の写し(重複)である(1 節の 3 番と 8 番)。`size` は写しを含む unit を長さで指し、障害の場所は指すが中身は指さない(軸の記録 3.2 の (d) と同じ観察)。
- **`apply_agent_event_to_frame`(`complexity` 7 位、問題なし)。** 新しい turn を開く条件は、コメントが「`TurnTracker` の条件の写し」と書いている(`frame/fold.rs:120-133`)。しかし `TurnTracker` は `MessageRole::AutoContinue` でも turn を開く(`persistence/event_log/turn.rs:44-52`)のに、fold の条件は `User` と、turn が開いていないときの `TaskNotification` だけである(`fold.rs:134-135`)。dfaafef3(2026-07-29「…auto-continue」)が `TurnTracker` にだけ `AutoContinue` を足した時点から食い違っている。`AutoContinue` は production で送られる(`providers/rig/session/turn.rs:333`、392、`session/continuation.rs:293`)。読んだ範囲では、`TurnEnded` で `started_at` が `None` に戻った後の自動継続の turn では fold の時計が始まらず、受け取りの経過時間は既定値(0)になると読める。実行して確かめてはいない。コンパイラが検査しない規則の写しが、片側の変更で食い違った例である。
- **`ProviderId`(`intent` 10 位、問題なし)。** 形式を作るのは `named_rig_provider_id`(`registry.rs:58-60`)だが、agentd はその逆を、同じ文字列の写しで解いている(`horizon-agentd/src/session/spawn.rs:82-97`、`id == "builtin.agent.rig"` と `strip_prefix("builtin.agent.rig.")`)。名前空間の接頭辞を変える変更は、agentd の解析をコンパイラの助けなしに見落としうる。性質は文書ではなく重複と結合である。
- **`run_session`。** `size`(6 位)では問題なし、`interface`(2 位)では問題あり。同じ unit で、性質によって判定が分かれた。`box_draw_geometry`(`size` 4 位、`complexity` 5 位)はどちらでも問題なしだった。

## 5. 誤報の機構

依頼文の 4 つの区分で書く。誤報は `問題なし` の 33 件である。

### `size`(誤報 8)

- **軸の定義が、障害にするものを見ていない(8 件すべて)。** loc は、データの表(`build_definitions`、`core_commands`)、コンパイラが網羅を強制する変換(`Mapper.map_event`)、閉じた領域の外部の実装の移植(`box_draw_geometry`)、一度ずつ書かれた線形の導出や描画の層(`scheme_from`、`paint_terminal`、`render_node`)、委譲済みの組み立て(`run_session`)を、写しや絡みのある長さと区別しない。問題ありの 2 件で障害だったのは写しで、長さではなかった。
- **数え方(何を数えるか)。** loc は空でない行で、コメントと先頭の doc を含む。上位 10 件ではコメントの割合が 0〜34 % で、`render_node`(111 行)と `scheme_from`(70 行)は経緯のコメントの分だけ上がっている。コメントを除いた数で並べ直してはいない。
- **`.code-db.toml` に属する判断。** `respond_to_user_message` は mock provider のテスト用の筋書きだが、production の registry に `builtin.agent.mock` として登録された production のコードである。テストに数えるかどうかは言語の規則では決まらず、設定の `tests.paths` で表すかどうかの判断になる(この件の判定は数え方によらず写しで決めた)。
- **SCIP や木の限界。** 上位では見当たらなかった。

### `complexity`(誤報 10)

- **軸の定義が、障害にするものを見ていない(10 件すべて)。** McCabe の決定点は、平らな `match` の腕を 1 本ずつ数え、`?` を 1 つずつ数える。上位 10 件のうち、表引きの `match`(`box_draw_segments` 112 腕、`command_for` 37 腕、`legacy_bytes` 38 腕)、引数の解析と dispatch(`board.rs` の `dispatch` 14 腕と 39 個の `?`、`cli.rs` の `parse` 19 腕と 36 個の `?`、`invoke.rs` の `parse` 17 腕と 21 個の `?`)、コマンドの dispatcher(`execute` 30 腕)、網羅的な fold(`apply_agent_event_to_frame` 26 腕)の 8 件は、腕か `?` が数の大半を占めた。残る 2 件は、外部の実装の移植(`box_draw_geometry`)と、追記されるだけの会話記録の不変条件の検査(`ConversationHistory.apply`、`?` 7 個、検査ごとに 1 つの規則)である。テストでない 3,425 の関数とメソッドの決定点 7,780 のうち、`?` が 1,309、2 本目以降の腕が 2,595 で、合わせて半分(3,904)になる。
- **数え方。** `?` と腕を引いて並べ直すと、上位は `box_draw_geometry`、`Scrollback.on_wheel`、`ConversationHistory.apply`、`legacy_bytes`、`WorkspaceShell.execute`、`SessionLoopState.handle_continuation_check`、`scheme_from` … となり、3 回目の読み判定で問題ありだった `WorkspaceShell.reconcile` が 14 位に現れる。並べ直した上位は判定していない。
- **`.code-db.toml` に属する判断。** 無し。
- **SCIP や木の限界。** マクロの引数がトークンのままである制限は、上位には効いていなかった。

### `interface`(誤報 6)

- **軸の定義が、障害にするものを見ていない(6 件)。** 引数の数は、(a)呼び手が 1 つで、その呼び手が持つ構造体の欄や文脈を開いて渡すもの(`paint_scrollback_window`、`run_sandboxed`、`control_plane_new_session`)、(b)欄にそのまま入れるコンストラクタ(`SessionLoopState.new`)、(c)ループの状態を所有で受けるスレッドの本体(`run_writer_loop`)、(d)テストのために環境を注入する入口(`horizon_cli::run`)を、(e)同じ値の組が複数の層の署名を通るもの(問題ありの 4 件)と区別しない。障害だったのは (e) だけで、(e) は 1 つの unit の数ではなく、呼び出しの鎖の上で同じ引数の組が繰り返されることにある。
- **数え方。** `self` を数え、GPUI の `window` と `cx` も 1 つずつ数える(`control_plane_new_session` の 9 のうち 3)。8 引数の 7 関数は、`fan_in_crates` とパスの順で 9・10 位が決まり、10 位はパスの順の偶然である。テストの呼び出しは `include_tests` に関係なく fan-in には入らないが、引数を変える費用にはテストの呼び出しも含まれる(`complete_rig_turn` は production 1 か所に対しテスト 7 か所)。
- **`.code-db.toml` に属する判断。** 無し。上位 10 件はすべて `#[allow(clippy::too_many_arguments)]` を持ち、書き手が数を知った上で許していることはコードに書かれている。
- **SCIP や木の限界。** 無し。引数の数は SCIP の署名から正しく取れていた。

### `intent`(誤報 9)

- **軸の定義が、障害にするものを見ていない(9 件)。** 「doc が無い」ことは、名前と署名がすべてを言う取り出しやコンストラクタ(`SessionId` の 3 つ)、method 名を属性で持ち中身の型に doc がある封筒(`horizon-acp` の 3 つ)、欄の名前が使い方を表す型(`Item`)、形式の説明がそれを作る関数にある型(`ProviderId`)を、読み取れない約束を持つ型(`Comment`)と区別しない。`Comment` で欠けていたのは型の doc ではなく、欄(`source`、`at`)の意味だった。欄は unit ではないので、欄に doc が無いことはこの軸に現れない。
- **数え方。** モジュールの行は、中身すべての利用者を fan-in として持つ(`contract` の 144 ファイル)。モジュールの `has_doc` は `//!` だけを見る。可視性が `private` のモジュール(`horizon-terminal-core` の `frame`、`contract`、`size`、`key_event`)も、項目が `pub use` で再公開されていれば他の crate からの fan-in を持ち、上位(11〜23 位)に入る。並びは fan-in の crate の数で、wire や共有の語彙の型(多くは自明な newtype や封筒)が先に来る。
- **TODO/FIXME の側。** この木の production では 0 件で、軸の半分が何も出さなかった(3 節の冒頭)。
- **`.code-db.toml` に属する判断。** 無し。
- **SCIP や木の限界。** `has_doc` は SCIP の documentation による。trait の実装のメソッドは `NULL` になるが、上位には効いていない。

## 6. 記録やクエリの見出しとの食い違い

- **`intent` の見出しは「Debt markers first」と並びを書くが、この木の production には印が無い。** 見出しの誤りではないが、今の木では軸が「文書の無い、他の crate から参照される unit」だけになっている。
- **`size` の file の段は、テストのファイルを除いても、ファイルの中の `mod tests` を含む。** 見出しは「files whose module is a test are left out」と書き、それと一致する。テストの行を引くと順位は大きく動く(1 節の file と crate の段)。
- **クエリの誤りは見つからなかった。** 読んだ 40 件の行範囲、引数の数、可視性、`has_doc`、`complexity` の各数(`apply_agent_event_to_frame` の `max_nesting` 5 を含む)は、ソースと合っていた。
- **前の読み判定との一致。** 3 回目の読み判定と重なる unit(`run_session` 問題なし、`build_definitions` 問題なし、`WorkspaceShell.execute` 問題なし、`Render for WorkspaceShell.render` 問題あり)は、規則が置き換わった後も同じ判定になった。

## 限界

- 判定は 1 人の読み手のもので、2 人目の一致率は取っていない。
- 費用を生んだかどうかは、該当の行に触れたコミットを `git log -S` で探した範囲で確かめた。見つからなかったことは費用が無かったことの証明ではない。
- 4 節の `AutoContinue` の食い違いの影響(経過時間が 0 になること)は読んだだけで、動かしていない。
- 上位 10 件だけを読んだ。`complexity` の並べ直しの上位や、`interface` の 11 位以下(`rig_provider_turn_streaming`、`handle_provider_event`、`spawn_session_thread` など、2 節の原因の他の層を含む)は判定していない。
- 結果はこのリポジトリの d6299e9b の木についてのもので、設定ファイルは無い。
