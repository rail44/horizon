# 現在のコードでの読み判定・2 回目:unit の系譜を入れた後(2026-10-26)

コードデータベースの SQL 層([設計文書](../code-database-design.md))に unit の系譜(7 つ目の増分、[系譜の記録](code-db-lineage-2026-10-26.md))が入った後で、5 つの信号の上位の候補を出し直し、読み判定を行った。1 回目([1 回目の記録](code-db-dogfood-prospective-2026-10-26.md)、HEAD 2ebd0ecb)と同じ手順で、候補が変わらないものは 1 回目の判定を引き、新しく入った候補だけを読んで判定した。あわせて、1 回目に見つけた誤報の機構を、系譜が解いたか、今の上位の候補のいくつに残っているか、SQL だけで直せるかの表にした。判定と記述だけを行い、ソースコードと SQL は変えていない。

依頼文によれば、オーナーは 2026-10-26 に「実際にdoghoodして、仕組みの品質や改善点を再評価しましょうか」と依頼し、1 回目と同じ手順、変わらない判定の再利用、新しい候補だけの判定、誤報の機構の表を承認した。また、この確かめ方を「前向き」ではなく「現在のコードでの読み判定」と呼ぶよう求めた。

前提は [設計文書](../code-database-design.md) の dogfooding の節と 7 つ目の増分の節、[1 回目の記録](code-db-dogfood-prospective-2026-10-26.md)(以下「1 回目」)、[系譜の記録](code-db-lineage-2026-10-26.md)、[グラフの記録](code-db-graph-2026-10-26.md)、`duckdb-ext/sql/README.md` である。記録の決定は問い直していない。

## 要約

- **Horizon のソースは 1 回目から変わっていない。** 2ebd0ecb から 9a3c83e9 までの 5 コミットは `docs/` と `duckdb-ext/` だけを変えた(`git diff --stat 2ebd0ecb 9a3c83e9`)。したがって、候補の出入りはすべて層の側(系譜と、それを読むクエリ)の変化によるもので、1 回目に読んだ行番号はそのまま有効である。
- **出入り**(上位 10 件、循環は 5 個):
  - `unit_churn`:残った 4、出た 6、入った 6。入った 6 件はすべて系譜による(名前で数えたコミットでは上位に届かない)。
  - `unit_fan_in_churn`:残った 7、出た 3、入った 3。入った 3 件はすべて系譜による。
  - `clone_change_consistency`:残った 7、出た 3、入った 3。入った 3 件はすべて系譜による。
  - `cochange_without_reference`:残った 3、出た 7、入った 7。入った 7 件はすべて系譜による。
  - `module_cycles`:5 個とも同じ(グラフは系譜を読まない)。
- **判定の数**(`問題あり` / `問題なし` / `判断できない`):`unit_churn` 4 / 6 / 0、`unit_fan_in_churn` 3 / 7 / 0、`clone_change_consistency` 4 / 6 / 0、`cochange_without_reference` 1 / 9 / 0、`module_cycles` 1 / 3 / 1。
- **新しく入った 19 件のうち `問題あり` は 5 件で、そのうち新しい原因は 2 つだった。** 1 つはテスト用の描画関数で、監査用の 10 個の事象が「memory: seeded」として描かれる写し間違い、もう 1 つは real-git のテストの補助関数の写しで、git の大域設定から切り離す修正が片方では条件の内側に入ったことである。残りの 3 件は 1 回目と同じ原因(端末の 4 本の送信路、`AgentdState` の 2 つの lock)を、別の unit の側から指したものだった。
- **系譜は、名前を残して本体が 1 つの unit に移った場合を解いた**(1 回目の `definitions` が上位から消え、本体を受けた `build_definitions` が入った)。ただし、受け手の `build_definitions` も、ツールの一覧というデータの表で `問題なし` だった。
- **系譜は、新しい誤報の機構を 2 つ持ち込んだ。** (1) 前身をたどると、移動・改名・分割のコミットが両方の unit の共有コミットになる。`cochange_without_reference` の上位 10 件のうち 4 件は、共有 5 コミットのうち 3 つが系譜のリンクのコミットで、それを除くと閾値(共有 3)を下回る。(2) クローンの片側だけの変更に、もう片側がまだ無かった頃のコミットが入る。上位 10 組のうち 6 組で片側の数が減り、1 組(`AgentFrameItem`)は 0 になる。どちらも SQL だけで除ける。
- **1 回目の 6 つの機構のうち、系譜で解けたのは「名前が残り本体が移った」の一部だけだった。** 他の 5 つは系譜と無関係か、系譜で数が大きくなった(`cochange_without_reference` は 2,917 組から 8,424 組になった)。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 9a3c83e9 に早送りした(`git merge --ff-only 9a3c83e9`、変更の無い状態で実行。`origin/main` の先端はその祖先だった)。HEAD は `9a3c83e9fb6688bfad4bbaa04be4a61d8be23a6a` である。

機械はオーナーの対話用デスクトップで、他の作業と共有している(初回の更新を測ったときの load average は 26)。DuckDB CLI 2.0.0(`~/.local/bin/duckdb2`)、sitting_duck v1.16.0、rust-analyzer(`~/.cargo/bin/rust-analyzer`)、jscpd 5.3.1(refactor audit の固定版)を使った。

生成物はすべて `target/code-db/` の下にあり、コミットしていない。

```sh
nice -n 19 duckdb-ext/build.sh                                   # 拡張 2 つ(作業ツリーに無かった)
nice -n 19 uv run --locked --script scripts/refactor-audit/run.py setup   # jscpd
nice -n 19 duckdb-ext/sql/run.sh index <作業ツリー>                 # jscpd 0.6 s、rust-analyzer scip 165.7 s
nice -n 19 duckdb-ext/sql/run.sh sql <作業ツリー> target/code-db/scratch2/export.sql
```

jscpd は exact が 270 組、normalized が 1,232 組で、1 回目と同じだった。`export.sql` は、5 つのクエリ(`queries/unit_churn.sql`、`unit_fan_in_churn.sql`、`clone_change_consistency.sql`、`cochange_without_reference.sql`、`module_cycles.sql`)を既定の変数のまま、末尾の `;` を外して `CREATE TABLE o.q_<名前> AS` の後に置いたものと、文脈用の表(`units` にテストの印・モジュール・クレートを付けたもの、自己辺を除く `unit_edges`、`unit_cochange`、`unit_history`、`unit_lineage`、`unit_successions`、`module_cycles`、`module_cycle_edges`、`clone_pairs`、`commits`、`unit_changes`)を、`ATTACH` した `target/code-db/scratch2/r2.duckdb` に書き出すものである。候補の表と文脈は、この DuckDB のファイルを直接読む一回限りの SQL で作った(コミットしていない)。

時間は次のとおりだった。

- 初回の更新(空のキャッシュから。blob のファイルは前の実行で書き出し済み)は、`run.sh query … unit_churn` の全体で 45.9 s、`meta.seconds` の合計で 42.6 s だった。内訳は `blob_nodes` 25.2 s、`lineage` 7.5 s、`blob_units` 4.6 s、`file_changes` 1.1 s、`hunks` 1.1 s、`nodes` 1.1 s などで、系譜の層は 1,615 コミットを導出し、継承の関係を 20,970 行入れた。最初の作成は測り損ねたので、キャッシュを消して作り直した値である。
- キャッシュの上での書き出し(読み込み、差分の更新、5 つのクエリと文脈の表)は 5.4 s、`run.sh query … unit_churn LIMIT=3` は 6.2 s だった。

数は次のとおりである。`unit_churn` 9,264 行、`unit_fan_in_churn` 2,915 行、`clone_change_consistency` 1,502 行、`module_cycles` 31 個(内側の辺 354、モジュール 166)は 1 回目と同じだった。`cochange_without_reference` は 8,424 組で、1 回目の 2,917 組の約 2.9 倍になった(6 節の機構 N1・M5)。`unit_cochange` は 23,125 行、`unit_history` は 23,013 行、`unit_lineage` は 14,551 行だった。

テストの unit の判別には `unit_tests`(`views/graph.sql`)を使った。`#[cfg(test)] mod tests;` で宣言された別ファイル(`tests.rs`)の関数も `is_test` になっていたので、パスによる補いは要らなかった。

文脈は 1 回目と同じ取り方である。呼び出し元(in)と呼び出し先(out)は `unit_edges` の深さ 1 の異なる unit の数、co-change の相手は `unit_cochange` から入れ子の組を除いて共有コミットの多いもの、循環はその unit のモジュールが属する `module_cycles` の大きさである。系譜の効き方は、`unit_churn` の `commits_by_name`(系譜の前の数)と `ancestors`、`unit_lineage` の鎖、`unit_successions` のリンクで見た。

## 判定の規則

1 回目と同じである。`問題あり` は名指しできる保守の費用や危険があり、それを除くリファクタリングがあるもの、`問題なし` は信号が無害な理由で説明できるもの、`判断できない` は判定にオーナーの意図が要るものである。判定はコードとグラフから行い、信号の大きさは根拠にしていない。読んだ箇所は `ファイル:行` で示す。行は HEAD 9a3c83e9 のもので、ソースが変わっていないので 1 回目の行番号と同じである。

残った候補は、1 回目の判定と理由を引いた。系譜で数が変わった候補については、増えた分のコミットの件名を見て、1 回目の理由が変わるかどうかだけを確かめた。

---

## 1. `unit_churn`(テストでない unit の上位 10 件)

並びはクエリのとおりで、系譜のコミット数の降順、同数なら `loc` の降順である。「名前で」は `commits_by_name`(系譜の前の数)、「祖先」は `ancestors`、「1 回目」は 1 回目の順位である。

| # | unit | 種類 | loc | コミット | 名前で | 祖先 | in / out | 主な co-change の相手(共有コミット) | 循環 | 1 回目 |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- | ---: | --- |
| 1 | `crates/horizon-agentd/src/session/run.rs` `run_session` | function | 242 | 50 | 21 | 3 | 1 / 55 | `ToolSessionState` impl(20)、`Inner`(19)、`Connection` impl(19) | 11 | 入った |
| 2 | `crates/horizon-agent/src/tools/catalog.rs` `build_definitions` | function | 344 | 49 | 2 | 5 | 1 / 6 | `ToolSessionState` impl、`run_session`、`execute_agent_tool`(各 12) | 40 | 入った |
| 3 | `src/terminal/session.rs` `TerminalSession` | impl | 398 | 48 | 46 | 2 | 0 / 1 | 同じファイルの型(20)、`TerminalCore` impl(17)、`scrollback.rs` の `tests`(15) | — | 1 |
| 4 | `src/workspace/commands.rs` `WorkspaceShell.execute` | method | 204 | 45 | 30 | 2 | 4 / 52 | `WorkspaceShell.new`・`WorkspaceShell` 型(各 20)、`CommandId`(17) | 13 | 7 |
| 5 | `src/workspace/session_lifecycle.rs` `WorkspaceShell` | impl | 490 | 44 | 44 | 0 | 0 / 1 | `commands.rs` の impl(17)、`mod.rs` の impl(15) | 13 | 2 |
| 6 | `crates/horizon-workspace/src/operations.rs` `Workspace` | impl | 485 | 43 | 16 | 2 | 0 / 1 | `lib.rs` の `tests`・`src/workspace/mod.rs` の `tests`(各 17)、`query.rs` の impl(10) | 7 | 入った |
| 7 | `src/workspace/commands.rs` `WorkspaceShell` | impl | 928 | 42 | 42 | 0 | 0 / 1 | `session_lifecycle.rs` の impl(17)、`command_enabled`(15) | 13 | 3 |
| 8 | `crates/horizon-agentd/src/session/connection.rs` `Connection` | impl | 273 | 41 | 21 | 3 | 0 / 1 | `AgentdState` impl・`run_session`(各 19) | 11 | 入った |
| 9 | `src/workspace/mod.rs` `WorkspaceShell.new` | method | 74 | 41 | 21 | 2 | 1 / 27 | `WorkspaceShell` 型(33)、`execute`・`reconcile`(各 20) | 13 | 入った |
| 10 | `src/workspace/mod.rs` `WorkspaceShell` | type | 102 | 40 | 21 | 2 | 113 / 23 | `WorkspaceShell.new`(33)、`execute`・`reconcile`(各 20) | 13 | 入った |

**出た 6 件**と今の順位は、`ToolSessionState` impl(39 コミット、11 位)、`WorkspaceShell.reconcile`(38、12 位)、`TerminalSession.spawn`(34、18 位)、`projection.rs` の `Store` impl(32、20 位)、`Inner`(31、21 位)、`definitions`(1、4,372 位)である。`definitions` は系譜で 42 から 1 になって外れた。残りの 5 件は自分の数も少し増えたが、系譜で大きく増えた候補に抜かれた。

**入った 6 件はすべて系譜による。** 名前で数えると 21、2、16、21、21、21 で、1 回目の 10 位(25)に届かない。系譜の鎖は次のとおりだった。

- `run_session`:`horizon-sessiond/src/session/run.rs`(b293051a の改名)← `horizon-sessiond/src/session.rs`(0d72fc00 の `content_match`)← `horizon-agentd/src/session.rs`(4e7fb81e の改名)。
- `build_definitions`:`definitions`(cf9a2daa の `body_move`)← `src/agent/tools/catalog.rs` ← `src/agent/tools/mod.rs` ← `src/agent/tools.rs` と `src/agent_tools.rs` の `agent_tool_definitions`。
- `Workspace` impl:`src/workspace/operations.rs`(64078019 の改名)← `src/workspace.rs`(abedcf4b の `content_match`)。
- `Connection` impl:`run_session` と同じ 3 つのファイルを通る。
- `WorkspaceShell.new` と `WorkspaceShell` 型:`src/workspace.rs`(7b588c08 の `content_match`)← `shell-gpui/src/workspace.rs`(04d9f0ee の改名)。GPUI への移行(M2〜M5)の頃の履歴が加わった。

テストの unit の上位 10 件は判定しなかった。並べると次のとおりである(コミット / 名前で)。

- `horizon-workspace/src/commands.rs` `tests`(32 / 20)
- `horizon-agent/src/hosting.rs` `tests`(29 / 2)
- `rig/mapping.rs` `rig_messages_from_horizon_events`(27 / 21、`#[cfg(test)]` の関数)
- `persistence/projection/duckdb/mod.rs` `tests`(27 / 17、loc 1 の `mod tests;`)
- `judge/mod.rs` `tests`(26 / 26)
- `frame/test_support.rs` `render_agent_transcript`(26 / 10)
- `config.rs` `tests`(25 / 22)
- `horizon-terminal-core/src/session_loop.rs` `tests`(24 / 19)
- `horizon-workspace/src/commands.rs` `tests.core_commands_have_stable_ids_and_titles`(24 / 14)
- `horizon-agentd/src/session/exploration.rs` `tests`(23 / 17)

1 回目のテストの上位にあった `src/terminal/session.rs` の `tests`(20)と `ToolSessionState.with_root`(20)は、系譜で 1 になって外れた(本体が `scrollback.rs` の `tests` と `ToolSessionBuilder.with_root` に移っていた)。`duckdb/mod.rs` の `mod tests;` は系譜の記録のとおり 17 から 27 になり、上位に残った。

### 判定

残った 4 件は 1 回目の判定を引く。

3. **`TerminalSession` impl — 問題なし(1 回目の 1 番)。** 入れ子の和で、大半は `spawn` のもの(`src/terminal/session.rs:205-420`)。増えた 2 コミットは `src/terminal/mod.rs`・`src/terminal.rs` の頃のもので、理由は変わらない。
4. **`WorkspaceShell.execute` — 問題なし(1 回目の 7 番)。** `CommandId` の dispatcher で、AGENTS.md の「Operations go through the command model」が決めた変更点である(`src/workspace/commands.rs:502-706`)。増えた 15 コミットは `src/workspace.rs` と `shell-gpui/src/workspace.rs` の頃のもので、コマンドを足すたびに 1 本の腕を足す形は移行の前から同じだった。
5. **`session_lifecycle.rs` の `WorkspaceShell` impl — 問題あり(1 回目の 2 番、`reconcile` と同じ原因)。** `reconcile` は上位から外れたが、この impl が同じ原因(ペインの種類ごとの分岐が `reconcile` と `spawn_agent_resume` にある、`session_lifecycle.rs:313-416`、484-494)を指している。
7. **`commands.rs` の `WorkspaceShell` impl — 問題あり(1 回目の 3 番)。** ボードのセッションの処理が約 300 行、コマンドの dispatcher と同じ impl にある(`commands.rs:136-448`)。

入った 6 件を読んだ。

1. **`run_session` — 問題なし。** daemon の 1 セッションの組み立ての場所である(`crates/horizon-agentd/src/session/run.rs:47-296`)。provider の起動、環境の準備、`LiveState` の作成、ツールの実行時の登録、`select!` の事象ループを順に行う。引数は 12 個で `#[allow(clippy::too_many_arguments)]` が付いている(run.rs:46)が、呼び出し元は `spawn.rs:186` の 1 箇所だけで、引数の束の費用はその 2 箇所に閉じている。50 コミットの件名は、skill、grant、proxy、judge、explore、信頼、ボードのように、daemon の機能を足すたびにこの関数を通した履歴である。2026-09-22 の 4fe9f4fd(「Separate agent environment construction from the session loop」)で環境の構築が `SessionEnvironment`(`session/environment`)に出され、その後の 7 コミットは、入力・ツールの結果・寿命の分離と、機能の削除(d7a3046a、2e6a5f32)だった。機能を足すたびに触る形は、すでに解かれた後の履歴である。なお、1 回目の `AgentdState` の判定が指した 2 つの lock を別々に読む箇所(run.rs:79-88 と 112)はこの関数の中にあるが、原因は `AgentdState` の側で、ここでは数えない。
2. **`build_definitions` — 問題なし。** 1 回目に `definitions` の誤りとして指摘した履歴が、系譜で本体を受けた関数に付いた。本体はツールの一覧で、`Definition` を 15 個と `SynchronousTool::*.definition(…)` を並べた `vec![…]` である(`crates/horizon-agent/src/tools/catalog.rs:25-381`)。49 コミットは、ツールの追加・削除と説明文の調整(task の経路の文言など)で、項目はそれぞれ独立していて、1 つを変えても他を変えさせない。入力の形は、cf9a2daa 以後、多くが検証つきの入力型から作られる(`super::input::schema`、`SynchronousTool`)。手書きの `json!` が残るのは `workspace.snapshot`、2 つの mock、`board.read`・`board.comment` の 5 箇所(`catalog.rs:31`、145、163、311、349)で、これは入力型との食い違いの余地だが、一覧という形の費用ではない。
6. **`horizon-workspace` の `operations.rs` の `Workspace` impl — 問題なし。** ワークスペースのモデルの操作(タブ・ペイン・分割・閉じる・移動)を並べた impl で、method は 7〜42 行、系譜で数えても 1 つあたり 9 コミット以下である(`crates/horizon-workspace/src/operations.rs:24-544`)。43 コミットは入れ子の和に、分割の前の履歴が足されたものである。05bcc113(2026-07-03「Split workspace domain modules by responsibility」)で、この impl から `query.rs` と `session.rs` の impl が分かれたが、残ったこの impl の前後の類似度は 0.47 で、分かれた側はどれも前身の半分に届かないので、`body_move` は起きず、`src/workspace.rs` の頃の impl 全体の履歴(初めのプロトタイプから)がこの impl に残った(`unit_successions` の 05bcc113 の行)。co-change の相手が 2 つのテストモジュールなのは、モデルの操作を変えるとモデルとシェルのテストが変わるためである。
8. **`horizon-agentd` の `Connection` impl — 問題なし。** 1 本の接続から見た `AgentdState` への窓口で、型の文書が「thin by design」と書いている(`crates/horizon-agentd/src/session/connection.rs:20-29`)。method は 3〜28 行の委譲で、多いものでも `handle_session_new` の 11 コミットである(`connection.rs:31-323`)。41 コミットは入れ子の和に、`horizon-sessiond/src/session.rs` の頃の履歴が足されたものである。co-change の相手が `AgentdState` と `run_session` なのは、daemon の制御の呼び出しを 1 つ足すと、窓口・状態・セッションの 3 つに 1 つずつ足す形だからである。`reload_provider_config`(connection.rs:215-217)は 1 回目の `AgentdState` の判定の箇所だが、原因は `AgentdState` の側にある。
9. **`WorkspaceShell.new` — 問題あり(1 回目の `TerminalSession.spawn` と同じ原因)。** シェルの構築で、端末からシェルへの 4 本の送信路を作り(`src/workspace/mod.rs:421-424`)、フィールドに入れ(450-453)、受け手の `wire_*` を起動する(465-468)。事象を 1 つ足すと、ここの 3 箇所が 1 行ずつ増える。41 コミットの大半は、移行の M2〜M5 とその後の機能(ボード、プレビュー、モデルの選択)のフィールドの初期化だが、名指しできる費用はこの送信路の部分である。
10. **`WorkspaceShell` 型 — 問題あり(同じ原因)。** シェルの根のフィールドの並び(`src/workspace/mod.rs:302-403`)で、4 本の送信路のフィールドとその説明(383-402)を持つ。40 コミットのうち送信路を足したのは 4 つ(704657ba の exit、91ea6a31 の title、cf63a944 の notify、6b1977e8 の cwd)で、残りは機能ごとに独立したフィールドの追加だった。

## 2. `unit_fan_in_churn`(fan-in × コミットの上位 10 件)

クエリ自体が積(`fan_in_x_commits`)で並べるようになった(系譜の記録 5 節)。「名前で」は `commits_by_name` である。

| # | unit | 種類 | loc | fan-in(ファイル) | 参照の出現 | コミット | 名前で | 積 | 循環 | 1 回目 |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | `crates/horizon-agent/src/contract.rs` `Event` | type | 152 | 89 | 3,079 | 24 | 21 | 2,136 | 3 | 1 |
| 2 | `crates/horizon-agent/src/tools/state.rs` `ToolSessionState` | impl | 223 | 31 | 192 | 39 | 37 | 1,209 | 40 | 2 |
| 3 | `crates/horizon-agentd/src/session/state.rs` `AgentdState` | type | 91 | 30 | 212 | 25 | 9 | 750 | 3 | 入った |
| 4 | `crates/horizon-agent/src/live.rs` `LiveState` | impl | 204 | 34 | 296 | 22 | 17 | 748 | — | 4 |
| 5 | `crates/horizon-agentd/src/session/state.rs` `AgentdState` | impl | 173 | 21 | 98 | 34 | 17 | 714 | 3 | 7 |
| 6 | `crates/horizon-agent/src/contract.rs` `Command` | type | 77 | 41 | 747 | 17 | 15 | 697 | 3 | 3 |
| 7 | `crates/horizon-agent/src/contract/tool_result.rs` `ToolCallResult` | type | 6 | 59 | 497 | 11 | 3 | 649 | 3 | 入った |
| 8 | `crates/horizon-workspace/src/operations.rs` `Workspace` | impl | 485 | 14 | 318 | 43 | 16 | 602 | 7 | 入った |
| 9 | `crates/horizon-agent/src/persistence/event_log/writer.rs` `WriterHandle` | impl | 271 | 27 | 120 | 22 | 13 | 594 | — | 9 |
| 10 | `crates/horizon-agent/src/config.rs` `RigAgentConfig` | type | 92 | 25 | 293 | 23 | 18 | 575 | 3 | 5 |

この表は系譜の記録 5 節の「後」の列と同じである。

**出た 3 件**は、`ToolCallRequest`(64 × 8 = 512、11 位)、`session_lifecycle.rs` の `WorkspaceShell` impl(8 × 44 = 352)、`definitions`(8 × 1)である。`definitions` は系譜で外れ、他の 2 件は自分の数がほぼ同じまま、系譜で増えた候補に抜かれた。

**入った 3 件はすべて系譜による。** 名前で数えた積は、`AgentdState` 型が 30 × 9 = 270、`ToolCallResult` が 59 × 3 = 177、`Workspace` impl が 14 × 16 = 224 で、1 回目の 10 位(336)に届かない。`AgentdState` 型の祖先は 4 段で、`horizon-agentd/src/session.rs`(a5bbe2f6 で作られた)から `horizon-sessiond` を通って今のファイルに届く。`ToolCallResult` の祖先は `contract.rs` の同名の型(9eb4adea で `contract/tool_result.rs` に移った)と、その前の `src/agent/…` の型である。

### 判定

残った 7 件は 1 回目の判定を引く。系譜で増えたコミットの件名を見て、どれも理由は変わらなかった。

1. **`Event` — 問題なし(1 回目の 1 番)。** イベントログの語彙で、網羅的な `match` 5 箇所への伝播をコンパイラが強制する(`contract.rs:273-423`、559 ほか)。
2. **`ToolSessionState` impl — 問題なし(1 回目の 2 番)。** 1 セッションのツールの能力の容器で、fan-in は各ツールの getter の呼び出しである(`tools/state.rs:232-470`)。
4. **`LiveState` impl — 問題なし(1 回目の 4 番)。** fold と永続化の境界で、費用はファイルの中で閉じている(`live.rs:122-345`)。増えた 5 コミットはクレートの分割の前の履歴だった。
5. **`AgentdState` impl — 問題あり(1 回目の 7 番)。** reload が `agent_config` と `providers` を別々の lock で入れ替え(`horizon-agentd/src/session/state.rs:183-193`)、起動が別々に読む(`session/run.rs:79-88`、112)。17 から 34 に増えたコミットは、`horizon-sessiond` の頃の、daemon の機能ごとのフィールドと method の追加で、原因の読み方は変わらない。
6. **`Command` — 問題なし(1 回目の 3 番)。** wire のコマンドと内部の 3 つの variant の同居は意図した形と読める(`contract.rs:249-262`)。
9. **`WriterHandle` impl — 問題なし(1 回目の 9 番)。** 公開する面は小さく安定している(`event_log/writer.rs:70-350`)。増えた 9 コミットは、`src/agent/…` の頃の作成・移動と、`crate` 内への縮小や lint の一斉の修正だった。
10. **`RigAgentConfig` — 問題あり(1 回目の 5 番)。** 接続、ループの調整値、セッションごとの値の 3 種類を 1 つの構造体が持つ(`config.rs:519-610`)。

入った 3 件を読んだ。

3. **`AgentdState` 型 — 問題あり(1 回目の `AgentdState` impl と同じ原因)。** 型の定義そのもので、同じ snapshot から作る `providers` と `agent_config` を別々の `Mutex` のフィールドとして持つ(`horizon-agentd/src/session/state.rs:42`、46。型は 33-123)。費用の在りかは impl の判定と同じで、型と impl の 2 行が同じ原因を指している。fan-in 30 ファイルは、daemon の各部(接続、セッション、ツールの host、ボード)が状態を読む箇所である。
7. **`ToolCallResult` — 問題なし。** 4 つのフィールドの wire のデータ型で(`crates/horizon-agent/src/contract/tool_result.rs:26-31`)、`#[serde(deny_unknown_fields)]` で永続化される。fan-in 59 は、ツールの結果を作る側(各ツールの実装)と読む側(fold、projection、provider の対応)で、契約の型を皆が import する形である。11 コミットは、クレートの分割、JSON の値の扱いの変更、呼び出しの同一性(`occurrence_id`)の追加、結果の意味(`ToolOutcome`)の明示で、どれもフィールドの変更をコンパイラが使う側に強制する。1 回目の `ToolCallRequest` と同じ理由である。
8. **`Workspace` impl — 問題なし。** `unit_churn` の 6 番と同じ候補である。fan-in 14 ファイルは、シェルとテストがモデルの操作を呼ぶ箇所で、公開の method の名前と引数は分割の後ほとんど変わっていない。

## 3. `clone_change_consistency`(片側だけの変更がある組の上位 10 件)

1,502 行のうち、片側だけの変更(`one_sided > 0`)がある行は 574 行(1 回目 505)だった。そのうち両側がテストの unit のものが 467 行(1 回目 405)、片側が unit に解決しないものが 15 行だった。両側がテストの行を除き、同じ 2 つの unit の組(exact と normalized、複数の断片)を 1 つにまとめると 63 組(1 回目 59)になった。まとめるときは、片側と両側の数を組の中の最大にした(unit の数なので、断片によらず同じ値になる)。並びは `one_sided` の降順、同数なら `both_sides` の降順である。

「共存後の片側」は、新しい機構 N2(6 節)を見るための列で、2 つの側のうち新しい方の最初のコミットより後の片側だけのコミットの数である(SQL は変えておらず、書き出した `unit_history` から数えた)。

| # | 側 a | 側 b | 片側(a / b) | 両側 | 共存後の片側(a / b) | 断片の始まりの行(a / b) | 出所 | 1 回目 |
| --- | --- | --- | --- | ---: | --- | --- | --- | --- |
| 1 | `src/terminal/session.rs` `TerminalSession.spawn` | `src/workspace/mod.rs` `WorkspaceShell`(型) | 28 / 34 | 6 | 14 / 34 | 208 / 382 | normalized | 1(28 / 17) |
| 2 | `frame/fold.rs` `apply_agent_event_to_frame` | `tools/explore.rs` `fold_until_terminal` | 19 / 7 | 10 | 7 / 7 | 242 / 671 | exact, normalized | 6(4 / 9) |
| 3 | `src/runtime/mod.rs` `AgentdHandle` impl | 同じファイルの `TerminaldHandle` impl | 25 / 0 | 4 | 11 / 0 | 5 断片(465 / 652 ほか) | exact, normalized | 8(9 / 0) |
| 4 | `projection.rs` `Store.project_event` | `tools/explore.rs` `fold_until_terminal` | 14 / 8 | 9 | 5 / 8 | 103 / 648 | normalized | 2(11 / 8) |
| 5 | `providers/mock.rs` `Provider for MockProvider.start_session` | 同じファイルの `respond_to_user_message` | 19 / 3 | 3 | 2 / 3 | 214 / 456 | normalized | 3(15 / 3) |
| 6 | `src/keymap.rs` `command_for` | `src/terminal/input.rs` `term_key_code` | 17 / 2 | 0 | 17 / 0 | 127 / 188、157 / 196 | normalized | 5(16 / 0) |
| 7 | `frame/fold.rs` `apply_agent_event_to_frame` | `projection.rs` `Store.project_event` | 12 / 6 | 17 | 10 / 6 | 240 / 137 | exact, normalized | 4(4 / 12) |
| 8 | `crates/horizon-agent/src/frame/types.rs` `AgentFrameItem` | `src/agent/model/types.rs` `AgentFrameItem` | 14 / 1 | 0 | 0 / 1 | 20 / 146 | normalized | 入った |
| 9 | `frame/fold.rs` `apply_agent_event_to_frame` | `frame/test_support.rs` `render_agent_transcript`(テスト) | 8 / 5 | 21 | 8 / 5 | 240 / 156 | exact, normalized | 入った |
| 10 | `horizon-agentd/src/session/setup.rs` `tests.real_git`(テスト) | `horizon-agentd/src/worktree.rs`(unit に解決しない) | 11 / 0 | 0 | 11 / 0 | 356 / 56、367 / 560、403 / 450 | exact, normalized | 入った |

**出た 3 件**と今の順位は、`horizon-workspace` の 2 つのテストモジュール(10 / 0、12 位)、`recall_tests` と `duckdb/tests.rs`(9 / 0、14 位)、継続の 2 つの経路(1 / 8、15 位)である。3 件とも自分の数は 1 回目と同じで、系譜で増えた組に抜かれた。

**入った 3 件はすべて系譜による。** 名前で数えると、8 番は片側 6 / 1、9 番は 3 / 1(両側 9)、10 番は 1 / 0 で、1 回目の 10 位(片側 9)に届かない。系譜で、8 番の側 a は `src/agent/…` の頃の 8 コミット、9 番の 2 つの関数は frame の分割の前の履歴、10 番の `tests.real_git` は `horizon-sessiond/src/session.rs` の頃のテストモジュールの履歴(1 → 11)を受け取った。

### 判定

残った 7 件は 1 回目の判定を引く。系譜で片側の数が大きく変わった組(1 番の b、2 番の a、3 番の a)は、増えた分が囲む unit の古い履歴で、1 回目の理由(片側の数は断片の伝播の漏れを示していない)はそのまま当てはまる。

1. **`spawn` の引数と `WorkspaceShell` のフィールド — 問題あり(1 回目の 1 番)。** `(SessionId, X)` を送る 4 本の `UnboundedSender` の型の並びが 2 箇所にある(`session.rs:208-213`、`src/workspace/mod.rs:382-403`)。
2. **`apply_agent_event_to_frame` と `fold_until_terminal` — 問題なし(1 回目の 6 番)。** `Event` の網羅的な `match` の no-op の一覧で、コンパイラが不整合を止める。
3. **`AgentdHandle` と `TerminaldHandle` — 問題あり(1 回目の 8 番)。** `begin_reload`・`stop_and_wait`・橋渡しのスレッドが字句まで同じに 2 つある(`src/runtime/mod.rs:472-486` と 659-673、335-341 と 569-575)。
4. **`project_event` と `fold_until_terminal` — 問題なし(1 回目の 2 番)。** 2 番と同じ機構。
5. **mock provider の 2 つの shutdown の発行 — 問題なし(1 回目の 3 番)。** テスト用の provider の中の 7 行の重複(`providers/mock.rs:222-231`、459-468)。
6. **`command_for` と `term_key_code` — 問題なし(1 回目の 5 番)。** 領域の違う対応表の見かけの一致。
7. **`apply_agent_event_to_frame` と `project_event` — 問題なし(1 回目の 4 番)。** 2 番と同じ機構。

入った 3 件を読んだ。

8. **2 つの `AgentFrameItem` — 問題なし。** 側 a はエージェントの内部の frame(`Event` を fold したもの、`crates/horizon-agent/src/frame/types.rs:15-73`)、側 b はシェルの frame(ACP の更新を fold したもの、`src/agent/model/types.rs:145-165`。モジュール文書が「defined on ACP-native data」と書いている、types.rs:1-4)である。一致したのは `HistoryCleared`・`MemoryDigest`・`MemoryCheckpointMissed`・`ProviderRateLimited`・`Error`・`Exited`・`TurnEnded { reason, model, elapsed }` の並びで、シェルが ACP だけを読み、エージェントの内部の型を使わないという分割(`docs/agent-runtime-split-design.md`)のための写しである。シェルの側は crate の `AgentFrameItem` を参照していない(`src/agent/` から `horizon_agent::frame` を使う箇所は無い)。写しの費用は、表示する項目を 1 つ足すと 2 つの frame と ACP の対応に 1 つずつ足すことで、1 回目の `NamedProviderConfig` と同じく、境界の決まった費用である。片側 14 は、シェルの側の型ができた 8c52fe5a(2026-09-27、「Make the shell an ACP v2 client of horizon-agentd」)より前の、側 a の履歴だけでできていて、2 つが共に在った間の片側の変更は a 0 / b 1(b の 1 はその作成のコミット)だった。
9. **`apply_agent_event_to_frame` と `render_agent_transcript` — 問題あり(小)。** 断片は、監査用の事象を並べた or のパターンである(`crates/horizon-agent/src/frame/fold.rs:240-251`、`crates/horizon-agent/src/frame/test_support.rs:156-166`)。8e057dd7(2026-09-15「Replace board workflow with ordinary tasks and routed session work」)は、両方の関数の `Event::MemorySeeded` の腕の前に、同じ 10 個の事象(`SessionInputSent` から `DeliveryAcknowledged` まで)を足した。fold の側ではその腕は `=> {}` なので無害だが、テスト用の描画の側ではその腕が `lines.push("memory: seeded")` なので(`test_support.rs:166`)、10 個の監査の事象がすべて「memory: seeded」と描かれる。`MemorySeeded` の腕は 4ff478b9(2026-08-17)で描画の側に足されたもので、8e057dd7 は一覧を写すときにその腕の本体を確かめなかった形である。今の呼び出し元(`crates/horizon-agent/src/tests.rs:100`、141)は `MessageCommitted` と `HistoryCleared` しか渡さないので、今はどのテストもこの誤りを見ていない。網羅的な `match` なので variant の足し忘れはコンパイラが止めるが、腕の本体の写し違いは止めない。費用はテスト用の関数の中に限られる。直し方は、監査の事象の腕を `=> {}` にし、`MemorySeeded` の腕を分けることである。片側 8 / 5 は 2 つの関数のほかの腕の変更で、この誤りを指してはいない。
10. **`setup.rs` の `tests.real_git` と `worktree.rs` — 問題あり(小)。** `setup.rs` の real-git のテストモジュールは、`worktree.rs` のテストの補助関数(`scrub_git_env`・`git`・`run_git`・`init_repo`・`commit_file`・`scratch_repo`・閉じたリポジトリの見張り)を写していて、モジュールの注釈が「Mirrors `worktree.rs`'s own real-git test convention」と書いている(`crates/horizon-agentd/src/session/setup.rs:327-333`)。側 b の断片はテストモジュール(`worktree.rs:409-`)と本番の `run_git`(`worktree.rs:58-`)にまたがって、unit に解決しなかった。写しはすでに食い違っている。794159af(2026-07-26「Make fixture git calls hermetic against global git config」)は、テストの git を大域・システムの git 設定から切り離す 2 行(`GIT_CONFIG_GLOBAL`・`GIT_CONFIG_SYSTEM` を `/dev/null`)を両方に足したが、`worktree.rs` の側では `scrub_git_env` の後に無条件に置かれ(`worktree.rs:434-447`)、写しの側では `GIT_` で始まる環境変数を消すループの `if` の中に置かれた(`setup.rs:338-346`)。そのため写しの側は、環境に `GIT_` の変数が 1 つも無いとき(pre-commit の hook の外で `cargo nextest` を直接走らせたときなど)、大域の `core.hooksPath` を読む。その commit の文書が書く、fixture のコミットごとに外部の hook が走って 60 s を超える現象を防ぐ修正が、片方では効かない場合があるということである。費用はテストの中だが、同じ修正を 2 箇所に当てて違う形になった実例である。直し方は、補助関数を 1 つのテスト支援の場所(たとえば `horizon-daemon-testkit`)に置くことである。片側 11 / 0 は、側 b が解決しないので、側 a のテストモジュールの変更の数そのものである。

## 4. `cochange_without_reference`(既定の閾値で上位 10 組)

既定の閾値は `min_shared` 3、`min_coupling_pct` 30、`max_commit_files` 50 である。全 8,424 組(1 回目 2,917)の内訳は次のとおりだった。

- 少なくとも一方がテストの unit:4,810 組(1 回目 1,796)
- 両方がテストの unit:1,348 組(1 回目 498)
- 結合が 100%:462 組(1 回目 414)
- 結合が 100% で共有が 4 以上:74 組(1 回目 33)、5 以上:10 組

並びは結合の降順、同じなら共有の降順、その後はパスと名前の順である。上位 10 組は、結合 100%・共有 5 以上の 10 組とちょうど一致した。

「名前で」は、系譜を使わず `unit_changes` の同一性で数えた共有コミットの数(`max_commit_files` の除外は掛けていないので、クエリの条件より多めの数である)、「リンク」は、共有コミットのうち、両側の unit のどちらにとっても系譜のリンクのコミット(`unit_lineage` の深さ 1 以上の `sha`、つまりその unit が改名・移動・分割で前身から続いたコミット)であるものの数で、クエリと同じく 50 を超えるファイルを変えたコミットと merge は除いた(新しい機構 N1、6 節)。

| # | unit a | unit b | 共有 | a / b のコミット | 結合 | テスト | 名前で | リンク | 1 回目 |
| --- | --- | --- | ---: | --- | ---: | --- | ---: | ---: | --- |
| 1 | `tools/bash/tests.rs` `expect_finished` | `tools/tests.rs` `expect_finished` | 8 | 8 / 8 | 100% | 両方 | 8 | 0 | 1 |
| 2 | `horizon-agent/src/lib.rs` `tests`(`mod tests;`) | `tools/mod.rs` `tests`(`mod tests;`) | 7 | 7 / 7 | 100% | 両方 | 1 | 1 | 入った |
| 3 | `horizon-agentd/src/session/connection.rs` `tests.connection_session_model_reads_the_stored_value_for_a_known_session_only` | `horizon-agentd/src/session/state.rs` `tests.session_directory_reports_the_plain_workspace_root_when_not_isolated` | 7 | 7 / 7 | 100% | 両方 | 3 | 1 | 入った |
| 4 | `horizon-agent/src/config.rs` `NamedProviderConfig` | `horizon-config/src/lib.rs` `RawConfig.resolved_providers` | 5 | 5 / 5 | 100% | — | 5 | 0 | 2 |
| 5 | `horizon-agent/src/config.rs` `NamedProviderConfig` | `horizon-config/src/lib.rs` `ResolvedProviderConfig` | 5 | 5 / 5 | 100% | — | 5 | 0 | 3 |
| 6 | `tools/catalog.rs` `Definition` | `tools/execution.rs` `tool_result_message` | 5 | 5 / 5 | 100% | — | 2 | 2 | 入った |
| 7 | `horizon-workspace/src/query.rs` `Workspace.detached_session_count` | `horizon-workspace/src/session.rs` `Workspace.terminate_session` | 5 | 5 / 5 | 100% | — | 1 | 3 | 入った |
| 8 | `query.rs` `Workspace.session_count`(`#[cfg(test)]`) | `session.rs` `Workspace.terminate_session` | 5 | 5 / 5 | 100% | a | 1 | 3 | 入った |
| 9 | `query.rs` `Workspace.tab_count` | `session.rs` `Workspace.session_is_referenced` | 5 | 5 / 5 | 100% | — | 1 | 3 | 入った |
| 10 | `query.rs` `Workspace.visible_panes` | `session.rs` `Workspace.session_is_referenced` | 5 | 5 / 5 | 100% | — | 1 | 3 | 入った |

**出た 7 件**は次のとおりである。`KeyEventKind` と `TerminalMouseButton`・`TerminalMouseKind`(1 回目 4・5 位)は、系譜でマウスの型の祖先の履歴が加わり(b のコミット 5 → 9)、結合が 55.6% に下がって 3,249・3,250 位になった。`JudgeHandle` と `parse_stage1`(6 位)は、`parse_stage1` の祖先が 1 つ少ないため結合 75% に下がった(893 位)。`JudgeHandle` と `parse_stage2`、`tests.table` の 3 組(7〜10 位)は、数は同じ(共有 4、100%)で、共有 5 以上の組に抜かれた(16・18・19・20 位)。

**入った 7 件はすべて系譜による。** 名前で数えた共有は 1〜3 で、1 回目の上位に入る数(結合 100% で共有 4 以上)に届かない。

### 判定

残った 3 件は 1 回目の判定を引く。

1. **2 つの `expect_finished` — 問題あり(小)(1 回目の 1 番)。** 同じ補助関数が 2 つのテストファイルに写され、`BashCompletion` の variant を足すと両方を直す(`tools/bash/tests.rs:25`、`tools/tests.rs:145`)。
4. **`NamedProviderConfig` と `RawConfig.resolved_providers` — 問題なし(1 回目の 2 番)。** `horizon-agent` が `horizon-config` に依存しないという、AGENTS.md の Configuration の節が決めた境界の写しである。
5. **`NamedProviderConfig` と `ResolvedProviderConfig` — 問題なし(1 回目の 3 番)。** 4 番と同じ。

入った 7 件を読んだ。

2. **2 つの `mod tests;` — 問題なし(信号の誤り)。** どちらも loc 1 の宣言で(`crates/horizon-agent/src/lib.rs:39`、`crates/horizon-agent/src/tools/mod.rs:167`)、本体はそれぞれの `tests.rs` にある。7 つの共有コミットは、どれも 2026-06-30〜07-03 の `src/agent/…` の頃のもので(d1379c69、e25169de、d337d4fb、259016bc、e41cce7e、9c12672e、a6b0c63f)、エージェントのモジュールを作り、分け、整理したときに、2 つの前身のインラインのテストモジュールが一緒に変わったものである。うち 1 つは両側にとって系譜のリンクのコミットだった。名前を残した宣言が、本体がファイルに移る前の履歴を持ち続ける、系譜の記録 6 節の `mod tests;` と同じ形である。
3. **`horizon-agentd` の 2 つのテスト関数 — 問題なし。** どちらも `SessionEntry` を 10 個のフィールドの構造体のリテラルで作って `AgentdState` に入れ、1 つの読み取りを確かめるテストである(`crates/horizon-agentd/src/session/state.rs:381-404`、`crates/horizon-agentd/src/session/connection.rs:388-417`)。7 つの共有コミット(モデルの id、セッションの関係、remoc、モジュールの分割、モデルの表示、接続の寿命、作業の場所)は、どれも `SessionEntry` にフィールドを足すか変えたもので、`SessionEntry` のリテラルは daemon の中に 9 箇所ある。1 回目の `tests.table` と同じく、型の形が変わると各テストの fixture が一緒に変わる形で、コンパイラが伝播を強制する。費用はテストの中にある。
6. **`Definition` と `tool_result_message` — 問題なし。** 7 行の構造体(`crates/horizon-agent/src/tools/catalog.rs:7-14`)と、ツールの結果の受け取りを知らせる 6 行の関数(`crates/horizon-agent/src/tools/execution.rs:209-214`)で、互いを参照せず、一緒に変える理由も無い。5 つの共有コミットは、エージェントの永続化の最初の追加(d1379c69、2 つの前身が同じコミットで作られた)、モジュールの整理(d337d4fb、a6b0c63f。この 2 つが両側にとって系譜のリンクのコミット)、可視性の一斉の縮小(ac33849e、b656cc27)だった。
7. **`detached_session_count` と `terminate_session` — 問題なし。** どちらもワークスペースのモデルの 6〜19 行の method である(`crates/horizon-workspace/src/query.rs:224-229`、`crates/horizon-workspace/src/session.rs:56-74`)。5 つの共有コミットは、`src/workspace.rs` からの移動(abedcf4b)、`query.rs` と `session.rs` への分割(05bcc113)、クレートへの切り出し(64078019)の 3 つのリンクのコミットと、可視性の一斉の変更(ac33849e)、初期の close と terminate の分離(bccbaa24)だった。移動のたびに両方が一緒に「変わった」ことになった。
8. **`session_count` と `terminate_session` — 問題なし。** 7 番と同じ 5 コミットである。`session_count` は `#[cfg(test)]` の 3 行の method(`query.rs:220-222`)。
9. **`tab_count` と `session_is_referenced` — 問題なし。** 7 番と同じ機構で、共有コミットは初めのプロトタイプ(b19e9e32)と 3 つのリンクのコミットと ac33849e だった(`query.rs:311-313`、`session.rs:89-93`)。
10. **`visible_panes` と `session_is_referenced` — 問題なし。** 9 番と同じ(`query.rs:315-321`)。

## 5. `module_cycles`(実際の循環のうち大きい 5 個)

5 個とも 1 回目と同じで、数(循環 31、内側の辺 354、モジュール 166)も同じだった。グラフは SCIP の索引と今の木から作り、系譜を読まない。ソースも変わっていない。

| # | 循環(最小の要素) | クレート | モジュール | 内側の辺 | unit の辺 |
| --- | --- | --- | ---: | ---: | ---: |
| 1 | `crates/horizon-agent/src/judge/chat.rs` | horizon-agent | 40 | 106 | 338 |
| 2 | `src/agent/view/pane.rs` | horizon | 13 | 41 | 258 |
| 3 | `crates/horizon-agent/src/providers/mock.rs` | horizon-agent | 12 | 38 | 122 |
| 4 | `crates/horizon-agentd/src/board_flow/dispatch.rs` | horizon-agentd | 11 | 21 | 50 |
| 5 | `crates/horizon-agent/src/providers/rig/clearing.rs` | horizon-agent | 9 | 25 | 104 |

### 判定

すべて 1 回目の判定を引く。

1. **`judge/*`・`policy.rs`・`tools/*` — 問題あり(小)(1 回目の 1 番)。** tools の段取りの `start_approval_gate` が judge の中にある(`judge/mod.rs:301-345`)。
2. **`src/workspace/*` と `src/agent/view/pane.rs` — 問題なし(1 回目の 2 番)。** 1 つの型の impl をファイルに分けた形と、コマンドへの結び付け。
3. **`registry.rs` と provider — 問題なし(1 回目の 3 番)。** トレイトと工場の同居、親子の参照。
4. **`board_flow/*` と `session/*` — 判断できない(1 回目の 4 番)。** 問いは 9 節に残した。
5. **`rig/completion*` ほか — 問題なし(1 回目の 5 番)。** 親子の参照と、置き場所を外れた補助関数 1 つ。

## 6. 誤報の機構の表

1 回目が名指しした 6 つの機構(M1〜M6)と、今回見つけた 3 つ(N1〜N3)について、系譜で解けたか、今の上位の候補(5 つの信号の 45 件)のうちいくつの数をその機構が説明するか、SQL だけで直せるか(検出する規則)を並べる。「説明する」は、その機構が候補の数を大きくしている、または候補を上位に入れている場合で、判定が `問題あり` の候補でも数え、括弧の中に `問題なし` の数を書いた。

| 機構 | 系譜で解けたか | 今の上位で説明する候補 | SQL だけで直せるか、検出する規則 |
| --- | --- | --- | --- |
| **M1** 入れ子の unit(impl、モジュール)が中のすべての変更を数える | 解けていない(unit の粒度は系譜と別の問題) | 11 件(`問題なし` 7)。`unit_churn` 3・5・6・7・8 番の impl、`unit_fan_in_churn` 2・4・5・8・9 番の impl、`clone_change_consistency` 10 番のテストモジュール | できる。コミットの変更した行を、その行を含む最も内側の unit だけに付ける。`unit_change_sides` は両側の unit の行を、`hunks` は変更の行の範囲を持ち、`units.parent_unit_id` で入れ子が分かるので、hunk の行が子の unit の範囲に入るかどうかで親から除ける。fan-in の側も、親が子の記号を合わせる(クエリの注釈が書いている)ので、同じ規則で「自分の行に定義のある記号だけ」に絞れる |
| **M2** 名前が残り、本体が移った後も履歴を持ち続ける | 一部が解けた。本体が 1 つの unit に移った場合は、`body_move` で残り殻が切れた(`definitions` 42 → 1、`src/terminal/session.rs` の `tests` 20 → 1、`ToolSessionState.with_root` 20 → 1)。本体がファイルに移った場合(`mod tests;`)と、前身の半分に届かない複数の部分に分かれた場合は残る | 3 件(`問題なし` 3)。`unit_churn` 6 番と `unit_fan_in_churn` 8 番の `Workspace` impl(05bcc113、類似度 0.47、受け手なし)、`cochange_without_reference` 2 番の 2 つの `mod tests;`。判定しないテストの上位では `duckdb/mod.rs` の `mod tests;`(27) | SQL で書ける。前の側が後の側に似ておらず、前の側の行が同じコミットで追加された複数の unit や新しいファイルの行の和でおおむね覆われるとき、残り殻を切る。本文は `lineage_sources` から読める。ただしこれは、オーナーが承認した 4 つのリンクの規則の外の、新しい規則である(系譜の記録 6 節が、依頼文の許す規則の外として作らなかったもの) |
| **M3** 契約の型を皆が import し、伝播をコンパイラが強制する | 解けていない。系譜で契約の型のコミットが増え(`Event` 21 → 24、`ToolCallResult` 3 → 11)、`ToolCallResult` は上位に入った | 3 件(`問題なし` 3)。`unit_fan_in_churn` 1・6・7 番(1 回目の 6 番の `ToolCallRequest` は 11 位に下がった) | SQL で印を付けられる。型の unit のコミットのうち、その型に辺を張る unit(`unit_edges` の in)も同じコミットで変わった割合を出す(`edge_cochange` が辺ごとの共有コミットを持つ)。割合が高く、fan-in も高い型は「参照を通って伝播する契約」として分けられる。網羅的な `match` を確かめるなら、`read_ast` で match の腕がその enum の variant の名前を並べている箇所を数える |
| **M4** クローンの片側だけの変更を、断片ではなく囲む unit で数える | 解けていない。系譜で囲む unit の履歴が長くなり、片側の数は大きくなった(1 番の b 17 → 34、3 番の a 9 → 25) | 10 件(`問題なし` 6)。上位 10 組のすべて。読んだかぎり、どの組でも片側の数は断片の伝播の漏れを示していない(9・10 番の写し違いも、片側の数とは別の変更だった) | SQL で書けるが、近似になる。各コミットで、断片の行(HEAD の正規化した行の集合)が、その側の unit の前の側と後の側の本文(`lineage_sources`)で変わったかどうかを見て、変わったコミットだけを数える。hunk の行番号は過去の blob の座標なので、行番号ではなく行の本文で照合する |
| **M5** co-change の組にテストの unit が入る | 解けていない。系譜で組が増え、テストを含む組も増えた(1,796 → 4,810、両方がテスト 498 → 1,348) | 6 件(`問題なし` 3)。`cochange_without_reference` 1・2・3・8 番、`clone_change_consistency` 9・10 番 | できる。`unit_tests` がすでにあり、`#[cfg(test)] mod tests;` で宣言された `tests.rs` の関数にも印が付いていた。クエリで除くか、列で印を付ける |
| **M6** 親子のモジュールの循環、1 つの型の impl をファイルに分けた形 | 関係しない(グラフは系譜を読まない) | 4 件(`問題なし` 3)。`module_cycles` 1 番の `tools/` の中の部分、2・3・5 番 | できる。モジュールとその祖先のモジュール(パスの接頭辞、`modules` のモジュールの unit)の間の辺を落とし、両端が同じ型(`impl_for`)の impl の method である辺を落としてから、強連結成分を作り直す |
| **N1**(新)移動・改名・分割のコミットが、両方の unit の共有コミットになる | 系譜が持ち込んだ。前身をたどると、2 つの unit が一緒に移ったコミット(同じファイルからの分割、クレートの切り出し)が、各々の「変更」として 2 つの履歴に入る | 7 件(`問題なし` 7)。`cochange_without_reference` 2・3・6〜10 番。うち 7〜10 番は共有 5 のうち 3 つがリンクのコミットで、除くと共有が 2 になり `min_shared` 3 を下回る(6 番は 2 つで、除くと 3)。`unit_churn` の上位でも、入った 6 件のコミットの一部(`WorkspaceShell` の M2〜M5、`run_session` の `session.rs` の頃)は移動の前後のコミットだが、それ自体は本当の変更である | できる。`unit_commits` から、その unit の系譜のリンクのコミット(`unit_lineage` の深さ 1 以上の `sha`)を除く。co-change では、両側にとってリンクのコミットであるコミットだけを除く形でもよい |
| **N2**(新)クローンの片側だけの変更に、もう片側がまだ無かった頃のコミットが入る | 系譜が強めた(古い側の履歴が長くなった)。1 回目の数にもあった(1 回目の 8 番の `TerminaldHandle` は 2026-07-29 にできた) | 6 件(`問題なし` 4)。`clone_change_consistency` 1〜5 番と 8 番。8 番は 14 / 1 が 0 / 1 になる。上位の外では 11 番の 2 つの reload(10 → 3)、15 番の継続の経路 | できる。組ごとに、2 つの側の最初のコミットの遅い方より後のコミットだけを数える(`unit_history` と `commits.author_time`、または `commit_order`) |
| **N3**(新、1 回目の本文で観察)改名と衛生作業の一斉の変更が、多くの unit の履歴に入る | 系譜が強めた。系譜で古い一斉の変更(可視性の縮小、lint の修正、語彙の掃除)が今の unit に届く | 8 件(`問題なし` 8)。`unit_fan_in_churn` 4・8・9 番(`LiveState`、`Workspace`、`WriterHandle`)、`cochange_without_reference` 6〜10 番の共有の 1〜2 個(ac33849e、b656cc27) | 一部だけ。`max_commit_files` 50 はファイルの数で切るだけで、それより小さい一斉の変更は残る(unit の変わった Rust のファイルは、b656cc27 で 17、ac33849e で 5。e899e99d は 50 を超えて切られている)。コミットあたりの変更した unit の数と、unit あたりの変更の行の少なさで印を付けることは SQL でできるが、本当に横断する変更と区別するには件名や中身を読む必要がある |

この表の数は、上位の候補のうち、その機構が数や順位を説明するものを数えたもので、機構どうしは重なる(たとえば `Workspace` impl は M1、M2、N3 のすべてに入る)。

## 7. 信号ごとの集計

| 信号 | 問題あり | 問題なし | 判断できない | 入った候補のうち 問題あり |
| --- | --- | --- | --- | --- |
| `unit_churn`(テストでない 10 件) | 4(5、7、9、10) | 6 | 0 | 2 / 6(9・10 番、1 回目と同じ原因) |
| `unit_fan_in_churn`(積の 10 件) | 3(3、5、10) | 7 | 0 | 1 / 3(3 番、1 回目と同じ原因) |
| `clone_change_consistency`(10 組) | 4(1、3、9、10) | 6 | 0 | 2 / 3(9・10 番、新しい原因) |
| `cochange_without_reference`(10 組) | 1(1) | 9 | 0 | 0 / 7 |
| `module_cycles`(5 個) | 1(1) | 3 | 1(4) | —(入った候補なし) |

合わせて 45 件のうち、`問題あり` 13、`問題なし` 31、`判断できない` 1 である(1 回目は 12 / 32 / 1)。信号をまたいで重複を除くと、`問題あり` の原因は 10 個だった。1 回目の 9 個のうち、今回の上位に残ったのが 8 個(継続の 3 つの写しは上位から外れた)、新しいものが 2 個である。

信号ごとの変化を書く。

- **`unit_churn`**:系譜で入った 6 件のうち 4 件は `問題なし` だった。`build_definitions`(データの表)、`run_session`(すでに分解された後の組み立ての場所)、`Workspace` と `Connection` の impl(入れ子の和と、移動の前の履歴)である。`問題あり` の 2 件は `WorkspaceShell` の型と構築で、1 回目の `spawn` と同じ 4 本の送信路を、シェルの側から指した。系譜は 1 回目の誤り(`definitions`)を消したが、代わりに入った候補の多くも、別の理由の誤報だった。
- **`unit_fan_in_churn`**:系譜で入った 3 件は、1 回目の 2 つの型(`Event`・`Command` などの契約の型、`AgentdState`)と同じ種類だった。`ToolCallResult` は契約の型で、`AgentdState` 型は impl と同じ原因の 2 行目である。上位 10 件の中の判定の割合は 1 回目と同じ 3 / 7 だった。
- **`clone_change_consistency`**:系譜で入った 3 件のうち 2 件で、断片を読むと写しの費用が見えた(テスト用の描画の写し違い、テストの補助関数の修正の食い違い)。ただし、どちらも片側の数とは関係が無く、片側の数は囲む unit のほかの変更だった。1 回目と同じく、クローンが本物かどうかと、片側の数の大きさは別のことだった。
- **`cochange_without_reference`**:系譜で入った 7 件はすべて `問題なし` で、2 件はテストの unit、5 件はモデルの移動と分割で一緒に動いた unit だった(N1)。1 回目に `問題なし` だった `KeyEventKind` の 2 組は、系譜でマウスの型の古い履歴が加わり、結合が下がって外れた。系譜は、wire の一律の方針変更という 1 回目の誤報を 1 つ消し、移動を共有コミットにする誤報を持ち込んだ。
- **`module_cycles`**:変わらない。

## 8. オーナーに渡す候補の更新

1 回目の 8 個を、今回の上位に出たかどうかで分けて並べ直した。ソースが変わっていないので、1 回目の判定はどれも有効である。

**今回も上位に出て、確かめられたもの(7 個)。**

1. **`TerminalSession.spawn` と `WorkspaceShell` の 4 本の送信路**(`src/terminal/session.rs:206-214`、`src/workspace/mod.rs:383-402`、421-424、450-453、465-468):今回は `unit_churn` の 9・10 番(`WorkspaceShell.new` と型)と `clone_change_consistency` の 1 番の、3 行が指した。`spawn` 自体は 18 位に下がった。
2. **`WorkspaceShell.reconcile` の中のビューの構築**(`src/workspace/session_lifecycle.rs:313-416`):`reconcile` は 12 位に下がったが、`session_lifecycle.rs` の impl(`unit_churn` 5 番)が同じ原因を指している。
3. **`AgentdState` の 2 つの lock**(`horizon-agentd/src/session/state.rs:42`、46、183-193、`session/run.rs:79-112`):今回は型と impl の 2 行(`unit_fan_in_churn` 3・5 番)が指した。
4. **`RigAgentConfig` の 3 種類の値**(`horizon-agent/src/config.rs:519-610`):`unit_fan_in_churn` 10 番。
5. **`src/workspace/commands.rs` の中のボードのセッションの処理**(`commands.rs:136-448`):`unit_churn` 7 番。
6. **`AgentdHandle` と `TerminaldHandle` の同じ操作**(`src/runtime/mod.rs:472-486` と 659-673、335-341 と 569-575):`clone_change_consistency` 3 番。
7. **judge と tools の循環を閉じる `start_approval_gate`**(`horizon-agent/src/judge/mod.rs:301-345`):`module_cycles` 1 番。

**今回の上位から外れたもの(1 個)。**

8. **自動継続のターンの始め方の 3 つの写し**(`rig/session/continuation.rs:287-303`、`rig/session/turn.rs:325-343`・387-405):`clone_change_consistency` で 15 位に下がった。自分の数は 1 回目と同じ(1 / 8)で、系譜で数が増えた組に抜かれただけである。コードは変わっていないので、1 回目の判定は有効である。

**新しいもの(2 個、どちらも費用はテストの中)。**

9. **テスト用の描画で、監査の事象が「memory: seeded」になる写し間違い**(`crates/horizon-agent/src/frame/test_support.rs:156-166`、もとは 8e057dd7):今はどのテストもこの事象を描かないので、表に出ていない。
10. **real-git のテストの補助関数の写しで、git の大域設定から切り離す修正が条件の内側に入った**(`crates/horizon-agentd/src/session/setup.rs:338-346` と `crates/horizon-agentd/src/worktree.rs:434-447`、794159af):環境に `GIT_` の変数が無いとき、写しの側のテストは大域の `core.hooksPath` を読む。

1 回目の `expect_finished` の写し(`問題あり(小)`)は、1 回目と同じく `cochange_without_reference` の 1 番に出ている。1 回目はこれを渡す一覧に入れていなかったので、ここでも 9・10 番と同じ「テストの中の小さいもの」として扱う。

## 9. 未決の問い

1 回目の問いは残っている。`horizon-agentd` のボードの入口(ツールの host の `board_flow::operate` への委譲と、ACP の `Connection::ensure_board_organizer`、`session/board.rs:116`、`session/connection.rs:113-119`)を `session/` に置くのか、`board_flow/` をボードの唯一の置き場とするのか。前者が意図なら `module_cycles` の 4 番は問題なく、後者なら 2 本の辺は層の逆転である。

今回の判定では、オーナーの意図が要るものは新しく出なかった。

## 10. 記録や README と食い違った点、気づいた点

- **`cochange_without_reference` の組の数。** 系譜を入れた後は 8,424 組で、グラフの記録と 1 回目の 2,917 組は古くなった。系譜の記録はこのクエリの時間(1.02〜1.18 s → 0.23〜0.24 s)を書いているが、組の数の変化は書いていない。組が増えたのに速くなった理由は調べていない。
- **初回の更新の時間。** 42.6 s(`meta.seconds` の合計、blob のファイルは既にある状態、load average 26)で、系譜の記録の 24.5〜24.7 s、README の「about 25 s」より長い。内訳の `blob_nodes` が 25.2 s で、系譜の層は 7.5 s(系譜の記録では 5.5 s)だった。機械の負荷との切り分けはしていない。
- **`unit_successions` の 05bcc113 の行。** `src/workspace/operations.rs` の `Workspace` impl は、前後の類似度 0.47 で「似ていない」側にあるが、`body_move` の受け手が無いので `same_name` のまま残った。系譜の記録 11 節の「前身の行の半分未満しか持たない部分への分割は、どの部分にもつながらない」の、上位の候補での実例である。この場合、分かれた側(`query.rs`・`session.rs` の impl)は前身の履歴を受け取らず、残った側がすべてを持つ。
- **数の一致。** 系譜の記録の fan-in × コミットの上位 10 件(5 節の「後」)、`run_session` 21 → 50、`build_definitions` 2 → 49、`duckdb/mod.rs` の `mod tests` 17 → 27 は、今回の数と一致した。
- **`run.sh sql` の出力。** 書き出しだけの SQL では何も表示せずに終わる(`.output` を戻した後の文が結果を返さないため)。最初の実行で何も出ず、キャッシュと書き出しのファイルができたことで成功を確かめた。層の欠陥ではない。

## 11. 限界

- **読んだのは 1 人である。** 1 回目と同じく、判定は Opus 1 体が 1 回読んだもので、オーナーの意図を知らない。残った候補は 1 回目の判定を引いたので、1 回目の判定の誤りはそのまま引き継いでいる。判定を繰り返したときの安定性は測っていない。
- **信号の根拠は弱い。** 1 回目の 9 節と同じで、今回の集計は 1 つのリポジトリの上位 10 件ずつ(循環は 5 個)である。
- **出入りの比較の条件。** 1 回目と今回でソースは同じだが、テストの判別は今回は `unit_tests` だけで行った。1 回目の判別の細部は記録に無く、クローンの組の数(59 → 63)と内訳の差のうち、どこまでが系譜によるかは切り分けていない。
- **機構の表の数。** 「説明する候補」の数は、読んだかぎりでの数え方で、機構どうしは重なる。SQL で直せるとした規則は書いただけで、組んでも試してもいない。直した後の上位の候補が良くなるかどうかは分からない。
- **新しい 2 つの `問題あり`。** テスト用の描画の写し違いは、今のテストが見ていないことをコードと呼び出し元から確かめただけで、テストを走らせてはいない。real-git のテストの大域設定の件は、コードと 794159af の差分から導いたもので、`GIT_` の変数の無い環境で再現はしていない。
- **作業ツリー。** HEAD 9a3c83e9 のままで、索引・jscpd の報告・キャッシュはその木から作った。
