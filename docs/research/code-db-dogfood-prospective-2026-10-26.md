# コードデータベースの信号を実コードで読んで判定した記録(2026-10-26)

コードデータベースの SQL 層([設計文書](../code-database-design.md))が出す 5 つの信号について、Horizon の今のコードで上位の候補を出し、候補の裏のコードを読んで、本当にリファクタリングの対象かどうかを判定した。各信号がこのコードベースで役に立つのか、雑音なのかを、オーナーが抜き取りで確かめられるようにするのが目的である。判定と記述だけを行い、ソースコードと SQL は変えていない。

オーナーは 2026-10-26 にこう依頼した。「実際にdoghoodして、仕組みの品質を検証できるようにしてもらえますか?」。依頼側が示した形は、worker がコードを読んで判定し、オーナーが抜き取りで確かめる前向きのレビューで、オーナーはこれを承認した。

前提は [設計文書](../code-database-design.md)、`duckdb-ext/sql/README.md`、[グラフの記録](code-db-graph-2026-10-26.md)(以下「グラフの記録」)、[調査の記録](code-quality-analysis-trends-2026-10-24.md) の「主要な知見」である。判定はこれらの記録の範囲で行い、記録の決定は問い直していない。

## 要約

- **判定の数**(候補ごとに `問題あり` / `問題なし` / `判断できない`):
  - `unit_churn` 10 件:4 / 6 / 0
  - `unit_fan_in_churn` 10 件:3 / 7 / 0
  - `clone_change_consistency` 10 件:3 / 7 / 0
  - `cochange_without_reference` 10 件:1 / 9 / 0
  - `module_cycles` 5 件:1 / 3 / 1
- 信号をまたいで同じ原因を指す候補がある。`TerminalSession.spawn` の 4 本の送信路は `unit_churn` と `clone_change_consistency` の両方に、`WorkspaceShell` の `session_lifecycle.rs` の impl は `unit_churn` と `unit_fan_in_churn` の両方に、`definitions` の見かけの churn は 2 つの信号に出た。重複を除くと、`問題あり` の原因は 9 個だった。
- 候補の指し方が誤っていた原因で多かったのは、次の 4 つである。(1) unit の同一性が名前で続き、本体が別の unit に移った後も履歴を引き継ぐこと(`definitions`、`mod tests;` 宣言)。(2) 入れ子の unit が中身の変更をすべて数えること(impl の上位)。(3) 網羅的な `match` がコンパイラに強制する同時変更。(4) クレートの境界のためにわざと写した型と、テストの fixture。
- 信号ごとに分けると、`unit_churn` と `unit_fan_in_churn` は、ファイルや型の「どこを読むか」は当てたが、指した unit そのものが対象でない場合が多かった。`clone_change_consistency` の片側だけの変更の数は、断片ではなく、断片を囲む unit の変更を数えていた。そのため、当たった 3 件も、変更の数とは別の理由で当たっていた。`cochange_without_reference` の上位 10 件は、6 件がテストの unit を含み、残りは設計どおりの写しか、wire 全体への一律の変更だった。

## 環境と手順

作業は worker の作業ツリーで行った。作業ツリーのブランチはまず `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 2ebd0ecb に合わせた(`git reset --hard 2ebd0ecb`、作業ツリーに変更が無い状態で実行した)。HEAD は `2ebd0ecb3846f14aa3f7095fce3906fc6b381d97` である。

機械はオーナーの対話用デスクトップで、他の作業と共有している。DuckDB CLI 2.0.0(`~/.local/bin/duckdb2`、d54cbbe3df)、sitting_duck v1.16.0、rust-analyzer 1.99.1(58cf8f9 2026-10-13)、jscpd 5.3.1(refactor audit の固定版)を使った。

生成物の作り方。生成物はすべて `target/code-db/` の下にあり、コミットしていない。

```sh
nice -n 19 duckdb-ext/build.sh                      # 拡張 2 つ(作業ツリーに無かったので作った)
nice -n 19 rust-analyzer scip . --output target/code-db/index.scip   # 255 s
nice -n 19 uv run --locked --script scripts/refactor-audit/run.py setup
nice -n 19 duckdb-ext/sql/jscpd.sh . target/code-db/jscpd src crates preview-plugin/src
```

jscpd は exact が 270 組、normalized が 1,232 組を報告した。

層の読み込みは、README の順に `.read` するスクリプト `target/code-db/scratch/load.sql` で行った。中身は `SET VARIABLE repo = '<作業ツリーの絶対パス>';` の後に、`00_load.sql`、`views/files.sql`、`views/nodes.sql`、`views/blobs.sql`、`lang/rust.sql`、`views/units.sql`、`views/history.sql`、`views/unit_changes.sql`、`views/clones.sql`、`views/scip.sql`、`views/graph.sql`、`refresh.sql` を読むものである。続けて `target/code-db/scratch/export.sql` を読んだ。これは 5 つのクエリ(`queries/unit_churn.sql`、`unit_fan_in_churn.sql`、`clone_change_consistency.sql`、`cochange_without_reference.sql`、`module_cycles.sql`)を変数の既定値のまま CSV に書き出し、文脈用に `units`(テストの印、モジュール、クレート付き)、`unit_edges`(自己辺を除く)、`unit_references`(表示名付き)、`unit_cochange`、`unit_commits`、`module_cycles`、`module_cycle_edges`、`module_edges`、`clone_pairs`、`unit_changes`(コミットの件名付き)を Parquet に書き出すものである。

```sh
~/.local/bin/duckdb2 -unsigned -bail -no-agent \
  -c ".read target/code-db/scratch/load.sql" -c ".read target/code-db/scratch/export.sql"
```

時間は次のとおりだった。

- 初回(空のキャッシュから)は、読み込み・refresh・書き出しを合わせて 78 s かかった。`meta.seconds` では refresh の合計が 60.0 s で、内訳は `blob_nodes` 35.3 s、`blob_units` 6.3 s、`hunks` 4.3 s、`file_changes` 4.0 s、`nodes` 3.4 s、`blob_files` 2.1 s、`scip` 2.0 s などである。
- 書き出した各クエリは 0.1〜1.1 s だった。
- 2 回目のセッション(読み込みと差分の refresh、`unit_dependencies` を 2 回)は合わせて約 2 s だった。

README の初回 20〜26 s より長かったが、その原因は切り分けていない。

数は、グラフの記録と同じだった。`module_cycles` は 31 個、`module_cycle_edges` は 354 本、`cochange_without_reference` は 2,917 組、`unit_fan_in_churn` は 2,915 行である。refresh の meta は、ファイル 533、unit 9,264、blob 6,594、`scip_occurrences` 301,687 行だった。

候補の文脈は、書き出した表から作った(`target/code-db/scratch/out/a.duckdb`、コミットしていない)。

- 呼び出し元(in)と呼び出し先(out)は `unit_edges` の深さ 1 で数えた。代表として 2 件(`TerminalSession.spawn` は in 1・out 21、`WorkspaceShell.reconcile` は in 19・out 60)を `queries/unit_dependencies.sql` でも数え、同じ数になることを確かめた。
- co-change の相手は、`unit_cochange` から入れ子の組を除き、共有コミットの多いものを取った。
- 循環に入っているかどうかは `module_cycles.members` で見た。

## 判定の規則

判定は 3 種類にした。

- `問題あり`:名指しできる保守の費用や危険があり、それを除くリファクタリングがあるもの。
- `問題なし`:信号が無害な理由で説明できるもの。
- `判断できない`:判定にオーナーの意図が要るもの。何を知る必要があるかを書いた。

判定はコードとグラフから行い、信号の大きさは根拠にしていない。理由は機構として書いた。何が何と一緒に変わり、なぜか。誰が何に依存しているか。ここを変えると他に何を変えなければならないか。読んだ箇所は `ファイル:行` で示す。行は HEAD のものである。

---

## 1. `unit_churn`(テストでない unit の上位 10 件)

並びはクエリのとおりで、コミット数の降順、同数なら `loc` の降順である。`in`/`out` は深さ 1 の辺の数、`循環` はその unit のモジュールが属する循環の大きさである。

| # | unit | 種類 | loc | コミット | in / out | 主な co-change の相手(共有コミット) | 循環 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `src/terminal/session.rs` `TerminalSession` | impl | 398 | 46 | 0 / 1 | 同じファイルの `TerminalSession` 型(19)、`tests`(15)、`src/terminal/mod.rs` `TerminalView`(14) | — |
| 2 | `src/workspace/session_lifecycle.rs` `WorkspaceShell` | impl | 490 | 44 | 0 / 1 | `commands.rs` の `WorkspaceShell` impl(17)、`mod.rs` の `WorkspaceShell` 型(15) | 13 |
| 3 | `src/workspace/commands.rs` `WorkspaceShell` | impl | 928 | 42 | 0 / 1 | `session_lifecycle.rs` の impl(17)、`horizon-workspace` の `command_enabled`(15) | 13 |
| 4 | `crates/horizon-agent/src/tools/catalog.rs` `definitions` | function | 3 | 42 | 10 / 2 | `tools/state.rs` の `ToolSessionState`(11)、`Inner`(10) | 40 |
| 5 | `crates/horizon-agent/src/tools/state.rs` `ToolSessionState` | impl | 223 | 37 | 0 / 1 | `Inner`(24)、`definitions`(11) | 40 |
| 6 | `src/terminal/session.rs` `TerminalSession.spawn` | method | 99 | 32 | 1 / 21 | `TerminalSession` 型(14)、`horizon-terminal-core` の `TerminalUpdate`(7) | — |
| 7 | `src/workspace/commands.rs` `WorkspaceShell.execute` | method | 204 | 30 | 4 / 52 | `horizon-workspace` の `command_enabled`・`CommandId`(各 14) | 13 |
| 8 | `crates/horizon-agent/src/tools/state.rs` `Inner` | type | 158 | 29 | 39 / 9 | `ToolSessionState`(24)、`ToolSessionState.with_root`(20) | 40 |
| 9 | `crates/horizon-agent/src/persistence/projection/duckdb/projection.rs` `Store` | impl | 300 | 27 | 0 / 1 | `contract.rs` の `Event`・`event_kind`、`rig/mapping.rs` の `rig_messages_from_horizon_events`(各 12) | 4 |
| 10 | `src/workspace/session_lifecycle.rs` `WorkspaceShell.reconcile` | method | 206 | 25 | 19 / 60 | `commands.rs` の impl(14)、`execute`(11) | 13 |

テストの unit の上位 10 件は判定しなかった。並べると次のとおりである。

- `judge/mod.rs` `tests`(26)
- `config.rs` `tests`(22)
- `policy.rs` `tests`(21)
- `rig/mapping.rs` `rig_messages_from_horizon_events`(21、`#[cfg(test)]` の関数)
- `horizon-workspace/src/commands.rs` `tests`(20)
- `src/terminal/session.rs` `tests`(20)
- `tools/state.rs` `ToolSessionState.with_root`(20、`#[cfg(test)]`)
- `horizon-terminal-core/src/session_loop.rs` `tests`(19)
- `horizon-agentd/src/session/exploration.rs` `tests`(17)
- `persistence/projection/duckdb/mod.rs` `tests`(17、loc 1)

テストの上位 50 件のうち 35 件はモジュールの unit で、入れ子のために中のテストの変更をすべて数えている。最後の `duckdb/mod.rs` の `tests` は、今は `#[cfg(test)] mod tests;` の 1 行の宣言である(`duckdb/mod.rs:144-145`)。17 コミットは、2f4105f6(2026-08-02「Move duckdb projection tests to external files」)より前の、インラインのモジュールの履歴だった。

### 判定

1. **`TerminalSession` impl — 問題なし。** 46 コミットのうち 32 は `spawn` のもので(`spawn` は 6 番で別に判定した)、残りの 24 個の method は 1〜11 コミットずつである。そのほとんどは `send_*`(`session.rs` の impl 内)のような、1 つの `TerminalCommand` を送るだけの 3〜14 行の転送だった。impl という単位に、`spawn` と別の費用は見当たらない。読んだ箇所は `src/terminal/session.rs:205-420`。
2. **`session_lifecycle.rs` の `WorkspaceShell` impl — 問題あり(10 番と同じ原因)。** 44 コミットの大半は `reconcile`(25)、`spawn_agent_resume`(10)、`spawn_terminal_resume`(7)のものである。このうち `reconcile` と `spawn_agent_resume` は、ボードの 2 種類のペインを並べて分岐する同じ型の `match` を持っている(`session_lifecycle.rs:484-494`、`commands.rs:158-171`、`reconcile` の 347-396)。ビューの種類のペインを 1 つ足すと、この impl の中の複数の箇所を変えることになる。リファクタリングは 10 番と同じである。読んだ箇所は `src/workspace/session_lifecycle.rs:150-650`。
3. **`commands.rs` の `WorkspaceShell` impl — 問題あり。** このファイルのモジュール文書は、中身を「`execute`/`execute_control_plane` と `control_plane_*`」と書いている(`src/workspace/commands.rs:1-4`)。しかし impl には、ボードのセッションを取り込み、組織し、開く処理が約 300 行ある(`adopt_board_session`・`refresh_board_sessions`・`open_board_organizer`・`open_board_task_session`・`open_board_task_thread`、`commands.rs:136-448`)。ボードの機能を変えるコミットが、コマンドの dispatcher と同じファイル・同じ impl に入り、42 コミットのうち `execute` 以外の分の多くがそれである。リファクタリングは、ボードのセッションの処理を、`session_creation.rs` のように独立したファイルへ移すことである。
4. **`definitions` — 問題なし(信号の誤り)。** 今の本体は `catalog().to_vec()` の 3 行である(`crates/horizon-agent/src/tools/catalog.rs:16-18`)。42 コミット(2026-07-04〜2026-09-26)は、ツールの一覧を `vec![...]` で直接返していた頃の同名の関数の履歴だった。一覧は cf9a2daa(2026-09-26「Derive built-in tool schemas from validated input types」)で `build_definitions`(`catalog.rs:25`)に移り、それ以後の `definitions` の変更は 0 である。同一性が `(path, kind, qualified_name)` なので、名前が残り本体が移ると、古い履歴が残った名前に付く。
5. **`ToolSessionState` impl — 問題なし。** 1 セッションのツールが使う能力(ルート、grant、bash の cwd、recall、skills、ネットワークの proxy、judge、exploration、board)を入れる容器である。method のほとんどは 3〜10 行の getter と、grant を承認する method である(`tools/state.rs:232-470`)。co-change の相手が `catalog.rs` なのは、「ツールを 1 つ足す」ことが「一覧に 1 行足し、要る能力を 1 つ足す」ことだからで、機能の形どおりの同時変更である。
6. **`TerminalSession.spawn` — 問題あり。** `spawn` は、端末からシェルへ知らせる事象ごとに 1 本の `UnboundedSender` を引数に取る。`exit_tx`・`title_tx`・`notify_tx`・`cwd_tx` の 4 本で、どれも `SessionId` 付きである(`src/terminal/session.rs:206-214`、フィールドは 165-185)。事象を 1 つ足すと、4 箇所を変えることになる。(a) このフィールドと引数。(b) `WorkspaceShell` のフィールドとチャネルの作成(`src/workspace/mod.rs:383-402`、421-424、450-453)。(c) `install_terminal_session` での複製と受け渡し(`session_lifecycle.rs:547-563`)。(d) 受け手の `wire_*`(`session_events.rs:80-171`)。履歴では exit(2026-07-17)、title(09-10)、notify(09-15)、cwd(10-19)がこの形で足された。32 コミットの残りは、sessiond・terminald・wire の移行に伴う変更である。リファクタリングは、端末からシェルへの事象を 1 つの enum にして 1 本の送信路で送ることである。
7. **`WorkspaceShell.execute` — 問題なし。** `CommandId` ごとの dispatcher で、AGENTS.md が「Operations go through the command model」として、ここを通ることを決めている(`src/workspace/commands.rs:502-706`)。コマンドを 1 つ足すと、`CommandId`・`core_commands`・`command_enabled`(`horizon-workspace/src/commands.rs`)と、ここの 1 本の腕を変える。co-change の相手はその組で、設計どおりの変更点である。腕のほとんどは 1〜5 行の委譲だった。ただし 2 つの reload(`ReloadAgentRuntime`・`ReloadTerminalRuntime`)は、何を捨てるかの判断を腕の中に 20 行ずつ持つ。
8. **`Inner` — 問題なし。** 5 番の容器の中身で、能力ごとに 1 つのフィールドがある(`tools/state.rs:73-230`)。能力を 1 つ足すと、フィールド・builder の `with_*`(`state/builder.rs`)・getter を足す。29 コミットはこの追加で、増える一方の容器だが、各フィールドは独立していて、他のフィールドを変えさせない。文書コメントでは「本番の構築箇所は `run_session` だけ」という説明がフィールドごとに繰り返されていた(8 回)。これは注釈の重複で、リファクタリングの対象ではない。
9. **`projection.rs` の `Store` impl — 問題なし。** 中心は `project_event` で、`Event` の全 variant を網羅的に `match` する(`projection.rs:62-139`)。`Event` に variant を足すと、ここに no-op の腕を 1 つ足すことをコンパイラが強制する。co-change の相手が `Event`・`event_kind` なのはこのためで、意図した確認表である。
10. **`WorkspaceShell.reconcile` — 問題あり。** この関数は 2 つのことをする。セッションの store をモデルに合わせることと、ペインごとのビューを作ることである。後者は、ペインの種類ごとの `if / else if` の連鎖として、関数の中に直接書かれている。種類は terminal、agent、ThemeSettings、BoardList、BoardThread、Preview である(`src/workspace/session_lifecycle.rs:313-416`)。BoardList と BoardThread の 2 本の腕は、同じ 2 つの購読(`OpenTask*` と `BoardSessionsRefreshed`)・`watch_board_pane`・挿入を繰り返している(347-396)。ビューの種類を 1 つ足すと `reconcile` を変えることになり、in 19(9 ファイル)の呼び出し元をもつ関数が、ビューの機能のたびに触られる。リファクタリングは、ペインの種類からビューを作る処理を、関数(または種類ごとの構築表)として `reconcile` の外に出すことである。

## 2. `unit_fan_in_churn`(fan-in × コミットの上位 10 件)

クエリ自体は、`fan_in_files` の降順、同数ならコミットの降順で並べる。依頼文に従い、積(fan-in × コミット)で並べ直した。

| # | unit | 種類 | loc | fan-in(ファイル) | 参照の出現 | コミット | 積 | 循環 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `crates/horizon-agent/src/contract.rs` `Event` | type | 152 | 89 | 3,079 | 21 | 1,869 | 3 |
| 2 | `crates/horizon-agent/src/tools/state.rs` `ToolSessionState` | impl | 223 | 31 | 192 | 37 | 1,147 | 40 |
| 3 | `crates/horizon-agent/src/contract.rs` `Command` | type | 77 | 41 | 747 | 15 | 615 | 3 |
| 4 | `crates/horizon-agent/src/live.rs` `LiveState` | impl | 204 | 34 | 296 | 17 | 578 | — |
| 5 | `crates/horizon-agent/src/config.rs` `RigAgentConfig` | type | 92 | 25 | 293 | 18 | 450 | 3 |
| 6 | `crates/horizon-agent/src/contract.rs` `ToolCallRequest` | type | 7 | 64 | 1,103 | 6 | 384 | 3 |
| 7 | `crates/horizon-agentd/src/session/state.rs` `AgentdState` | impl | 173 | 21 | 98 | 17 | 357 | 3 |
| 8 | `src/workspace/session_lifecycle.rs` `WorkspaceShell` | impl | 490 | 8 | 28 | 44 | 352 | 13 |
| 9 | `crates/horizon-agent/src/persistence/event_log/writer.rs` `WriterHandle` | impl | 271 | 27 | 120 | 13 | 351 | — |
| 10 | `crates/horizon-agent/src/tools/catalog.rs` `definitions` | function | 3 | 8 | 10 | 42 | 336 | 40 |

クエリの並び(fan-in の降順)の上位は、ほとんど変わらない型だった。`SessionId`(fan-in 107、コミット 1)、`Event`(89、21)、`SessionId` の impl(78、1)、`ToolCallId`(68、1)、`SessionId.new`(65、1)などである。積で並べると、それらは下がる。

### 判定

1. **`Event` — 問題なし。** エージェントのイベントログの語彙で、永続化される wire の型である(`contract.rs:273-423`)。variant を足すと、網羅的な `match` 5 箇所に腕を足すことをコンパイラが強制する。5 箇所は `contract.rs:559` の `event_kind`、`frame/fold.rs:250`、`projection.rs:137`、`tools/explore.rs:671`、`horizon-agentd/src/hub/mapping.rs:479` で、2 クレートにまたがる。21 コミットは variant の追加である。fan-in 89 は variant を作る側と読む側で、契約の型を皆が import するのは設計どおりである。
2. **`ToolSessionState` impl — 問題なし。** `unit_churn` の 5 番と同じ理由である。fan-in 31 ファイルは、各ツールが自分の能力の getter を呼ぶ箇所だった。
3. **`Command` — 問題なし。** wire のコマンドと、内部だけで使う 3 つの variant(`ApplySessionModel`・`ToolCallReissued`・`ToolCallsSettled`、`#[serde(skip)]`、`contract.rs:249-262`)が 1 つの enum に同居している。直列化が失敗することはテストが確かめている(`contract.rs:1150-1153`、`config/selection.rs:93`)。網羅的に `match` する側(`providers/mock.rs:62-63`)は、受け取らない内部の variant にも腕を持つ。これは小さい費用で、同居は意図した形と読める。15 コミットはコマンドの追加である。
4. **`LiveState` impl — 問題なし。** fold と永続化の境界である(`live.rs:122-345`)。17 コミットのうち 7 つは、クレートの分割、改名 3 回、衛生作業、`pub` の縮小、remoc hub の削除という、リポジトリ全体の機械的な掃除だった。残りは、永続化の意味の変更(コミットしてから fold する、失敗を閉じる側に倒す)である。履歴を受け取るコンストラクタは 3 段の入れ子になっていて、文脈の引数が 1 つ増えるたびに 1 段増えた(`live.rs:204-260`)。ただし、その費用は 1 ファイルの中で閉じている。
5. **`RigAgentConfig` — 問題あり。** 1 つの構造体が 3 種類の値を持っている(`config.rs:519-610`)。(a) provider への接続(`kind`・`api_key_env`・`model`・`base_url`)。(b) 常に定数で、テストが小さい値を入れるためだけにフィールドになっている調整値(`iteration_cap`・`doom_loop_window`・`stream_flush_*`・`repository_instructions_cap_chars`・`max_output_tokens`・`clearing_threshold_pct`、各フィールドの注釈がそう書いている)。(c) セッションごとの値(`allowed_tool_ids`・`trusted_project`・`decision`)。(c) は、プロセス全体の設定を複製してから書き換えて作られる。書き換える箇所は `registry.rs:117`、`providers/rig/mod.rs:126`、`rig/session/turn.rs:541`、`rig/session/mod.rs:89`、`rig/session/environment.rs:26` で、`trusted_project` と `allowed_tool_ids` は 2 箇所ずつで書き換えられている。セッションの判断が 1 つ増えるたびに provider の設定にフィールドが増え、fan-in 25 ファイル(テストの構築の多く)が影響を受ける。18 コミットの件名も、ツールの制限、信頼、継続判定、圧縮のように、provider とは別の機能が並ぶ。リファクタリングは、接続、ループの調整値、セッションの引数を別の型に分けることである。
6. **`ToolCallRequest` — 問題なし。** 7 行のデータ型で、6 コミットしか変わっていない(`contract.rs:871`)。fan-in 64 は、皆が使う契約の型であることを示すだけである。
7. **`AgentdState` impl — 問題あり。** プロセス全体の状態で、16 個のフィールドがそれぞれ別の `Mutex` を持つ(`horizon-agentd/src/session/state.rs:40-122`)。設定の reload は、`agent_config` と `providers`(後者は前者から作る)を、別々の lock で順に入れ替える(`state.rs:183-193`)。セッションを起動する `run_session` は、`providers` を lock して `start_session` を呼び(`session/run.rs:79-88`)、その後で `agent_config` を別に lock して複製する(`run.rs:112`)。reload はどちらの側でも `lifecycle` の lock を取らない(`session/connection.rs:215-217`。`lifecycle` を取るのは `handle_session_new` の登録の部分と、スレッドの終了だけである)。そのため、起動が reload と重なると、新しい registry と古い `agent_config`(judge の `base_url` など)の組み合わせでセッションが始まりうる。重なる窓は狭いが、同じ snapshot から作った 2 つの値を 2 つの lock で持つことが原因である。リファクタリングは、2 つを 1 つの snapshot として 1 つの lock(または 1 つの差し替え可能な参照)で持つことである。17 コミットは、ボード・信頼・複数 provider のように、daemon の機能のたびにフィールドが増えた履歴だった。
8. **`session_lifecycle.rs` の `WorkspaceShell` impl — 問題あり。** `unit_churn` の 2 番と同じ候補で、fan-in は 8 ファイルである。
9. **`WriterHandle` impl — 問題なし。** 公開する面は `open*`・`failure`・`append`・`flush` だけで、小さく安定している(`event_log/writer.rs:70-350`)。13 コミットのうち 5 つは機械的な掃除で、残りは中の耐久性の意味の変更(失敗の保持、起動の差分化)だった。fan-in 27 の側は変わっていない。
10. **`definitions` — 問題なし(信号の誤り)。** `unit_churn` の 4 番と同じである。fan-in 8 は今の小さい関数のもので、コミット 42 は移る前の本体のものなので、積は別々の unit の値を掛けている。

## 3. `clone_change_consistency`(片側だけの変更がある組の上位 10 件)

クエリの 1,502 行(exact 270、normalized 1,232)のうち、片側だけの変更(`one_sided > 0`)がある行は 505 行だった。内訳は次のとおりである。

- 両側がテストの unit:405 行
- 片側が unit に解決しない:15 行
- `same_unit`:0 行

両側がテストの組を除き、同じ 2 つの unit の組(exact と normalized、複数の断片)を 1 つにまとめると、59 組になった。その上位 10 組を、`one_sided` の降順、同数なら `both_sides` の降順で並べる。

| # | 側 a | 側 b | 片側(a / b) | 両側 | 断片の行 | 出所 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `src/terminal/session.rs` `TerminalSession.spawn` | `src/workspace/mod.rs` `WorkspaceShell`(型) | 28 / 17 | 4 | a 208-213 / b 382-403 | normalized |
| 2 | `persistence/projection/duckdb/projection.rs` `Store.project_event` | `tools/explore.rs` `fold_until_terminal` | 11 / 8 | 9 | a 103-130 / b 648-670 | normalized |
| 3 | `providers/mock.rs` `Provider for MockProvider.start_session` | 同じファイルの `respond_to_user_message` | 15 / 3 | 3 | a 214-230 / b 456-466 | normalized |
| 4 | `frame/fold.rs` `apply_agent_event_to_frame` | `projection.rs` `Store.project_event` | 4 / 12 | 8 | a 240-251 / b 137 | exact, normalized |
| 5 | `src/keymap.rs` `command_for` | `src/terminal/input.rs` `term_key_code` | 16 / 0 | 0 | a 127-141, 157-166 / b 188-204 | normalized |
| 6 | `frame/fold.rs` `apply_agent_event_to_frame` | `tools/explore.rs` `fold_until_terminal` | 4 / 9 | 8 | a 242-251 / b 671 | exact, normalized |
| 7 | `horizon-workspace/src/persistence.rs` `tests`(テスト) | `horizon-workspace/src/tests.rs`(unit に解決しない) | 10 / 0 | 0 | a 921-931 / b 944-959 | normalized |
| 8 | `src/runtime/mod.rs` `AgentdHandle` impl | 同じファイルの `TerminaldHandle` impl | 9 / 0 | 3 | 5 断片:465-486 / 652-673 ほか | exact, normalized |
| 9 | `duckdb/query.rs` `recall_tests`(テスト) | `duckdb/tests.rs`(unit に解決しない) | 9 / 0 | 0 | a 1080-1093 / b 824-838 | normalized |
| 10 | `rig/session/continuation.rs` `SessionLoopState.handle_continuation_check` | `rig/session/turn.rs` `SessionLoopState.handle_truncation_recovery` | 1 / 8 | 0 | a 287-303 / b 327-343 | normalized |

クエリは、変更をその断片を囲む unit の変更として数える(クエリの見出しにそう書いてある)。そのため、「片側だけ」の数は、断片そのものが片側だけ変わった回数ではない。読んだかぎり、上位 10 組のどれも、片側の数は囲む unit のほかの部分の変更だった。

### 判定

1. **`spawn` の引数と `WorkspaceShell` のフィールド — 問題あり(`unit_churn` の 6 番と同じ原因)。** normalized が拾ったのは、`(SessionId, X)` を送る 4 本の `UnboundedSender` の型の並びである。それが `spawn` の引数(`session.rs:208-213`)と、シェルのフィールド(`src/workspace/mod.rs:382-403`)に重複している。重複そのものは本物で、事象を足すたびに両方へ 1 行ずつ足す。ただし片側 28 / 17 は、`spawn` と `WorkspaceShell` 型のほかの変更の数で、伝播の漏れを示してはいない。
2. **`project_event` と `fold_until_terminal` — 問題なし。** どちらも `Event` を網羅的に `match` し、no-op の variant を並べている(`projection.rs:103-137`、`explore.rs:640-671`)。新しい variant を一方だけに足すとコンパイルが通らないので、不整合な変更は起こりえない。両方に、12 個の variant を 1 行に並べた同じ行(`projection.rs:137`、`explore.rs:671`)がある。同じ一覧を各所に貼った跡で、rustfmt が整形していない。ただし保守の危険ではない。
3. **mock provider の 2 つの shutdown の発行 — 問題なし。** `StateChanged(Terminated)` と `Exited { reason: "shutdown" }` を順に送る 7 行が、2 箇所にある(`providers/mock.rs:222-231`、459-468)。テスト用の provider の中の小さい重複で、片側 15 は `start_session` の `match` のほかの腕の変更だった。
4. **`apply_agent_event_to_frame` と `project_event` — 問題なし。** 2 番と同じ機構で、no-op の腕の一覧である(`frame/fold.rs:240-251`、`projection.rs:137`)。
5. **`command_for` と `term_key_code` — 問題なし(見かけのクローン)。** 識別子を正規化すると `"文字列" => Some(定数)` の対応表どうしが一致したが、領域が違う(コマンドの kebab 名から `CommandId`、キー名から termwiz の `KeyCode`。`src/keymap.rs:126-166`、`src/terminal/input.rs:182-204`)。一緒に変える理由は無い。ただ、片側 16 の中身を見ると、`command_for` は新しいコマンドのたびに 1 行足される表で、`_ => None` があるのでコンパイラは足し忘れを止めない。HEAD では `CommandId` の 36 個の variant がすべて表にあった。これはクローンの信号とは別の観察である。
6. **`apply_agent_event_to_frame` と `fold_until_terminal` — 問題なし。** 2 番・4 番と同じである。
7. **`horizon-workspace` の 2 つのテストモジュール — 問題なし。** 両側ともテストのコードである。側 b の断片は unit をまたいでいるので解決せず、コミットが 0 になり、片側 10 は側 a の変更の数そのものになっている。
8. **`AgentdHandle` と `TerminaldHandle` — 問題あり。** 2 つの daemon のクライアントは、設計どおり別々の型である(AGENTS.md の `runtime/`)。その上で、2 つは次のものを字句まで同じに持っている。
   - `begin_reload` と `stop_and_wait`(`src/runtime/mod.rs:472-486` と 659-673)。
   - コマンドを crossbeam から tokio へ移す橋渡しのスレッド(`register_agent` の 335-341 と `register_terminal` の 569-575)。
   - 3 つの文言を付けて失敗を説明する `request(...).map_err(describe)` の形(359-395 と 605-614)。

   端末側の `begin_reload` の文書は、戻り値の約束を「[`AgentdHandle::begin_reload`] を見よ」と書いている(655-656)。2 つが同じであるべきことの表れである。片側 9 は、エージェント側に操作が増えた分だった。reload の約束を直すと 2 箇所を直すことになる。リファクタリングは、生存の 2 つの操作と橋渡しを、両方が共有する `control` の側(または共通の補助関数)へ移すことである。
9. **`recall_tests` と `duckdb/tests.rs` — 問題なし。** 7 番と同じで、テストどうしの組の一方が解決しなかったものである。
10. **継続の 2 つの経路 — 問題あり。** 自動で次のターンを始める手順は、次の 3 つである。`StateChanged(Running)` を送る。`MessageRole::AutoContinue` のメッセージを確定する。`Prompt::input(ConversationInputKind::Continuation, …)` でターンを走らせる。この手順が 3 箇所に写されている。継続の判定(`rig/session/continuation.rs:287-303`)、切り詰めからの回復(`rig/session/turn.rs:325-343`)、メモリの checkpoint の催促(`turn.rs:387-405`)である。`persistence/event_log/turn.rs:49` は `AutoContinue` のメッセージでターンの境界を判定する。そのため、この並びはイベントログとの約束で、1 箇所だけ変えると、その経路のターンの記録が他とずれる。継続の判定は 2026-10-20 に、切り詰めからの回復を写して足された。4 つ目の経路の `run_cap_summary_turn`(`turn.rs:530-551`)は、`Running` と `Continuation` の入力を送るが、`AutoContinue` のメッセージを確定しない。これが意図した違いかどうかは、コードからは分からなかった。リファクタリングは、3 つの経路が 1 つの「自動継続のターンを始める」method を呼ぶようにすることである。

## 4. `cochange_without_reference`(既定の閾値で上位 10 組)

既定の閾値は `min_shared` 3、`min_coupling_pct` 30、`max_commit_files` 50 である。全 2,917 組の内訳は次のとおりだった。

- 少なくとも一方がテストの unit:1,796 組
- 両方がテストの unit:498 組
- 結合が 100%:414 組
- 結合が 100% で共有が 4 以上:33 組

並びは結合の降順、同じなら共有の降順、その後はパスの名前の順である。したがって、6〜10 位は、同点の 33 組の中から名前順で選ばれたものにすぎない。

| # | unit a | unit b | 共有 | a / b のコミット | 結合 | テスト |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `tools/bash/tests.rs` `expect_finished` | `tools/tests.rs` `expect_finished` | 8 | 8 / 8 | 100% | 両方 |
| 2 | `horizon-agent/src/config.rs` `NamedProviderConfig`(型) | `horizon-config/src/lib.rs` `RawConfig.resolved_providers` | 5 | 5 / 5 | 100% | — |
| 3 | `horizon-agent/src/config.rs` `NamedProviderConfig` | `horizon-config/src/lib.rs` `ResolvedProviderConfig` | 5 | 5 / 5 | 100% | — |
| 4 | `horizon-terminal-core/src/types/key_event.rs` `KeyEventKind` | `types/mouse.rs` `TerminalMouseButton` | 5 | 5 / 5 | 100% | — |
| 5 | `types/key_event.rs` `KeyEventKind` | `types/mouse.rs` `TerminalMouseKind` | 5 | 5 / 5 | 100% | — |
| 6 | `judge/handle.rs` `JudgeHandle` | `judge/parse.rs` `parse_stage1` | 4 | 4 / 4 | 100% | b |
| 7 | `judge/handle.rs` `JudgeHandle` | `judge/parse.rs` `parse_stage2` | 4 | 4 / 4 | 100% | b |
| 8 | `rig/session/state.rs` `tests.table` | `registry.rs` `tests.named_entry` | 4 | 4 / 4 | 100% | 両方 |
| 9 | `rig/session/state.rs` `tests.table` | `horizon-agentd/src/session/connection.rs` `tests.list_providers_reports_both_entries_and_marks_the_unavailable_one` | 4 | 4 / 4 | 100% | 両方 |
| 10 | `rig/session/state.rs` `tests.table` | `horizon-config/src/lib.rs` `RawNamedProviderConfig` | 4 | 4 / 4 | 100% | a |

### 判定

1. **2 つの `expect_finished` — 問題あり(小)。** 同じ補助関数が、2 つのテストファイルに写されている(`tools/bash/tests.rs:25`、`tools/tests.rs:145`)。どちらも `BashCompletion` を網羅的に `match` するので、variant を足すと両方を直すことになり、8 コミットはすべて一緒だった。費用はテストの中に限られる。リファクタリングは、補助関数を 1 つのテスト支援の場所に置くことである。
2. **`NamedProviderConfig` と `RawConfig.resolved_providers` — 問題なし。** `horizon-agent` は `horizon-config` に依存しないと、意図して決められている。AGENTS.md の Configuration に「`horizon-agent` takes resolved named provider values as plain arguments」とある。そのため、provider の項目は `horizon-config` の `ResolvedProviderConfig`(`lib.rs:185-190`)と `horizon-agent` の `NamedProviderConfig`(`config.rs:374-392`)に写され、参照の辺は生じない。項目を足すと両方が変わるのは、この境界の決まった費用である。`DEFAULT_PROVIDER_NAME` の文字列も 2 箇所にあり、「一致しなければならない」と注釈がある(`config.rs:505-509`)。
3. **`NamedProviderConfig` と `ResolvedProviderConfig` — 問題なし。** 2 番と同じである。
4. **`KeyEventKind` と `TerminalMouseButton` — 問題なし。** 5 つの共有コミットは、terminal-core の切り出しと、wire の enum すべてに一律に `Unknown` の catch-all を足し、後で外した一連の変更だった(10eae865、9cff40ba、296e1df4、92951f4e、73f0c372)。wire 全体の方針を全部の型に当てた変更で、隠れた結合ではない。
5. **`KeyEventKind` と `TerminalMouseKind` — 問題なし。** 4 番と同じである。
6. **`JudgeHandle` と `parse_stage1` — 問題なし。** 4 つの共有コミットは、judge の導入・強制・作り直しという、同じ `judge/` モジュールの機能のコミットだった。`parse_stage1` は、`parse_stage1_result` を包む `#[cfg(test)]` の関数である(`judge/parse.rs:33-35`)。
7. **`JudgeHandle` と `parse_stage2` — 問題なし。** 6 番と同じである。
8. **`tests.table` と `tests.named_entry` — 問題なし。** 両方とも、`NamedProviderConfig` を構造体のリテラルで作るテストの fixture である(`rig/session/state.rs:318-338`、`registry.rs:237-250`)。4 つの共有コミットは、どれも provider の設定の作り直し(2026-09-18、09-20 が 2 回、10-16)だった。設定の形が変わると、各クレートのテストの fixture が一緒に変わる。
9. **`tests.table` とエージェント daemon のテスト — 問題なし。** 8 番と同じである。
10. **`tests.table` と `RawNamedProviderConfig` — 問題なし。** 2 番の写しの境界を、テストの fixture が越えて追っているものである。

## 5. `module_cycles`(実際の循環のうち大きい 5 個)

見かけの 3 個(`horizon-sandbox-helper.rs`、`tests/frame_watch.rs`、`tests/e2e.rs`。どれも 2 モジュール)は、上位 5 個には入っていない。

| # | 循環(最小の要素) | クレート | モジュール | 内側の辺 | unit の辺 |
| --- | --- | --- | --- | --- | --- |
| 1 | `crates/horizon-agent/src/judge/chat.rs` | horizon-agent | 40 | 106 | 338 |
| 2 | `src/agent/view/pane.rs` | horizon | 13 | 41 | 258 |
| 3 | `crates/horizon-agent/src/providers/mock.rs` | horizon-agent | 12 | 38 | 122 |
| 4 | `crates/horizon-agentd/src/board_flow/dispatch.rs` | horizon-agentd | 11 | 21 | 50 |
| 5 | `crates/horizon-agent/src/providers/rig/clearing.rs` | horizon-agent | 9 | 25 | 104 |

### 判定

1. **`judge/*`・`policy.rs`・`tools/*`(40 個)— 問題あり(小)。** 40 個のうち 33 個は `tools/` の下である。`tools/mod.rs` が各ツールに dispatch し、各ツールが親の型(`ToolSessionState`・`ToolCompletion`)を使うので、親と子が互いを参照する。これは Rust のモジュールでよくある形で、無害である。循環を `tools` の外へ広げているのは、次の 2 つの向きの辺である。
   - tools から judge:5 本・11 の unit の辺(`tools/mod.rs` と `tools/execution.rs` が `start_approval_gate`・`ApprovalCandidate` を、`tools/state.rs` が `JudgeHandle` を使う)。
   - judge から tools:4 本・7 の unit の辺。

   後者の中心は `judge/mod.rs` の `start_approval_gate` である(`judge/mod.rs:301-345`)。この関数は judge の中にあるが、中でしていることは tools の側の段取りである。tools の git の事前判定を呼び、`ToolSessionState` から judge の handle を取り、`tools::live_frame_for_session` からフレームを読み、結果を `tools::ToolCompletion` で返す。judge の説明文の取得(`judge/mod.rs:413-418`)も tools の一覧を読む。この形では、judge をテストするには tools の状態が要り、`ToolCompletion` の変更が `judge/handle.rs` に及ぶ。`policy.rs` と `tools` の間(policy から tools へ 14、tools から policy へ 3)も同じ型で、分類の関数が tools の状態と一覧を読んでいる。リファクタリングは、承認の段取りを tools の側に置き、judge が自分の判定の型だけを返すようにすることである。
2. **`src/workspace/*` と `src/agent/view/pane.rs`(13 個)— 問題なし。** workspace の 12 個のファイルは、どれも `WorkspaceShell` という 1 つの型の impl の一部で、ファイル間の辺は同じ型の method どうしの呼び出しである(例:`commands.rs` から `session_lifecycle.rs` へ 16 本の unit の辺、`render.rs` から `navigation.rs` へ 8 本)。1 つの impl をファイルに分ければ、この循環は必ず生じる。外の要素 `agent/view/pane.rs` は、`workspace` が定義する GPUI の action `RunCommand` を dispatch する(`src/agent/view/pane.rs:14`、100)。これは AGENTS.md が決めた「ボタンはコマンドへの結び付け」の形で、逆向きの辺は workspace がペインを作る辺である。
3. **`registry.rs` と provider(12 個)— 問題なし。** `registry.rs` は `Provider` トレイトと `SessionHandle` を定義し(`registry.rs:10`、29)、同時に具体的な provider を作る(`MockProvider::new` は 106、`rig::Provider::for_entry` は 118・135)。トレイトと工場が同じモジュールにあるので、各 provider と registry が互いを参照する。provider を足すときに registry を変えるのはどの形でも同じで、循環が変更を増やしてはいない。残りは `rig/mod.rs` と `rig/session/*` の親子の参照である。
4. **`board_flow/*` と `session/*`(11 個)— 判断できない。** board_flow は session の起動・再開・接続を使い(`operations.rs` から `spawn.rs`・`resume.rs`・`connection.rs` へ)、session の側から board_flow へは 2 本の辺がある。1 本は ACP の要求を受ける `Connection::ensure_board_organizer` の `board_flow::organizer_session` の呼び出し(`session/connection.rs:113-119`)、もう 1 本はボードのツールの host の `board_flow::operate` への委譲(`session/board.rs:116`)である。どちらも入口から機能への委譲で、状態を戻す依存ではない。判定には、ボードの入口(ツールの host と ACP の処理)を `session/` に置くのか、`board_flow/` をボードの唯一の置き場とするのかという、オーナーの意図が要る。前者が意図なら、この循環は問題ない。後者なら、2 本の辺は層の逆転である。
5. **`rig/completion*`・`conversation/*`・`clearing.rs`・`mapping.rs`(9 個)— 問題なし。** `completion.rs` と `completion/{attempt,response,outcome}.rs` の間は親子の参照である。`completion` と `conversation` の間を閉じる非テストの辺は、`conversation/upgrade.rs:181` が `completion.rs:640` の `replay_safe_tool_arguments` を呼ぶ 1 本だった。`mapping.rs` から外への辺は、すべて `#[cfg(test)]` の `rig_messages_from_horizon_events` のものである(`test_unit_edges` = 辺の数)。補助関数が 1 つ置き場を外れているだけで、一緒の変更を強いる仕組みは見当たらない。

## 6. 信号ごとの集計

| 信号 | 問題あり | 問題なし | 判断できない |
| --- | --- | --- | --- |
| `unit_churn`(テストでない 10 件) | 4(2、3、6、10) | 6 | 0 |
| `unit_fan_in_churn`(積の 10 件) | 3(5、7、8) | 7 | 0 |
| `clone_change_consistency`(10 組) | 3(1、8、10) | 7 | 0 |
| `cochange_without_reference`(10 組) | 1(1) | 9 | 0 |
| `module_cycles`(5 個) | 1(1) | 3 | 1(4) |

信号ごとに、誤りに共通していたことを書く。

- **`unit_churn`**:誤りの 6 件のうち 3 件は impl・容器の型(`TerminalSession`、`ToolSessionState`、`Inner`)で、入れ子の和や、能力を足すたびに伸びる容器だった。1 件は同一性の取り違え(`definitions`)、2 件は設計どおりの変更点(`execute`、`project_event` の impl)だった。当たった 4 件は、どれも `src/workspace/` と `src/terminal/session.rs` のシェルの配線で、「機能を 1 つ足すと N 箇所が変わる」箇所だった。テストの unit は、上位 50 件のうち 35 件がモジュールで、入れ子による水増しが特に強かった。
- **`unit_fan_in_churn`**:誤りの 7 件のうち 4 件(`Event`、`Command`、`ToolCallRequest`、`WriterHandle`)は契約の型か安定した面で、fan-in が大きいのは設計上皆が import するためだった。churn も variant の追加で、コンパイラが伝播を強制している。`LiveState` と `WriterHandle` のコミットの 4〜7 割は、改名と衛生作業の一斉変更だった。当たった 3 件のうち 2 件(`RigAgentConfig`、`AgentdState`)は、「違う寿命の値を 1 つの構造体に入れている」型だった。
- **`clone_change_consistency`**:片側の数は断片を囲む unit の変更の数で、10 組のどれでも、断片の伝播の漏れを示していなかった。誤りの 7 件のうち 3 件は、`Event` の網羅的な `match` の no-op の一覧で、コンパイラが不整合を止める。2 件はテストどうしの組の一方が unit に解決しなかったもの、1 件は対応表どうしの見かけの一致、1 件はテスト用の provider の小さい重複だった。当たった 3 件は、クローンが本物で、片側の数とは無関係に、読めば写しの費用が分かった。
- **`cochange_without_reference`**:上位 10 組のうち 6 組がテストの unit を含んでいた。テストでない 4 組は、設計で決めたクレートの境界の写し(2 組)と、wire の全型への一律の方針変更(2 組)だった。結合 100% の同点が多く(共有 4 以上で 33 組)、上位 10 件の後半は名前順で決まっていた。
- **`module_cycles`**:3 個は、Rust の親子のモジュールの相互参照と、1 つの型の impl をファイルに分けた形、トレイトと工場の同居で説明できた。当たった 1 個(judge と tools)は、循環を閉じる 2 つの向きの辺(`start_approval_gate` と `ToolCompletion`)を読むと、置き場所の問題が具体的に見えた。

## 7. オーナーに最初に渡す候補(8 個)

信号をまたいで重複を除き、`問題あり` のうち、機構がはっきりしていて費用が今もかかっているものから並べる。

1. **`TerminalSession.spawn` と `WorkspaceShell` の 4 本の送信路**(`src/terminal/session.rs:206-214`、`src/workspace/mod.rs:383-402`):端末からシェルへの事象を 1 つ足すたびに、セッションのフィールドと引数、シェルのフィールドとチャネル、`install_terminal_session`、`wire_*` の 4 箇所が変わる。
2. **`WorkspaceShell.reconcile` の中のビューの構築**(`src/workspace/session_lifecycle.rs:313-416`):ペインの種類ごとの分岐が、同期の処理の中に直接書かれている。ボードの 2 本の腕はほぼ同じで、ボードのペインの `match` は他の 2 箇所にもある。ビューの種類を 1 つ足すと、呼び出し元 19 の関数を変えることになる。
3. **`AgentdState` の 2 つの lock**(`horizon-agentd/src/session/state.rs:183-193`、`session/run.rs:79-112`):同じ snapshot から作る `providers` と `agent_config` を、reload は別々に入れ替え、起動は別々に読む。重なると、新しい registry と古い設定が組み合わさる。
4. **`RigAgentConfig` の 3 種類の値**(`horizon-agent/src/config.rs:519-610`):セッションごとの値が、プロセスの設定の複製を書き換えて作られ、`trusted_project` と `allowed_tool_ids` は 2 箇所ずつで書き換えられる。セッションの判断が増えるたびに、provider の設定が伸びる。
5. **自動継続のターンの始め方の 3 つの写し**(`rig/session/continuation.rs:287-303`、`rig/session/turn.rs:325-343`・387-405):イベントログ(`persistence/event_log/turn.rs:49`)が読む事象の並びを 3 箇所が別々に持っている。4 つ目の似た経路(`turn.rs:530-551`)は、すでに `AutoContinue` を送っていない。
6. **`src/workspace/commands.rs` の中のボードのセッションの処理**(`commands.rs:136-448`):ファイルの文書は dispatcher と `control_plane_*` と書いているが、約 300 行のボードの処理が同じ impl にあり、ボードの変更がコマンドのファイルに入る。
7. **`AgentdHandle` と `TerminaldHandle` の同じ操作**(`src/runtime/mod.rs:472-486` と 659-673、橋渡しのスレッド 335-341 と 569-575):reload の約束が 2 箇所にあり、片方の文書がもう片方を参照している。
8. **judge と tools の循環を閉じる `start_approval_gate`**(`horizon-agent/src/judge/mod.rs:301-345`):tools の側の段取りが judge の中にあり、judge が tools の状態と `ToolCompletion` に依存するので、judge を tools から切り離してテストも変更もできない。

## 8. 記録や README と食い違った点、気づいた点

- **同一性の、もう一方向の切れ方。** README の制限は「改名や移動は新しい履歴を始め、クエリはそれを追わない」と書いている。逆の場合、つまり名前が残り本体が別の unit に移った場合には、古い履歴が残った名前に付く。今回の実例は 2 つある。`definitions` は 42 コミットを引き継いだが、本体は `build_definitions` に移っていた。`duckdb/mod.rs` の `mod tests;` の宣言(loc 1)は 17 コミットを引き継いだが、本体は `tests.rs` に移っていた。この場合については README に記述が無い。
- **`unit_fan_in_churn.sql` の並び。** クエリは `fan_in_files` の降順で並べる。依頼文の「fan-in × commits」は、書き出してから計算した。
- **数の一致。** 循環 31・内側の辺 354・`cochange_without_reference` 2,917・`unit_fan_in_churn` 2,915 は、グラフの記録と一致した。
- **初回の refresh の時間。** 60 s で、README の 20〜26 s の 2〜3 倍だった。原因(機械の負荷など)は切り分けていない。2 回目は約 2 s だった。
- **CLI の CSV の NULL。** CLI の `.mode csv` は NULL を文字列 `NULL` で書く。そのため、`read_csv` で読み戻すときは `nullstr='NULL'` が要った。層の欠陥ではなく、今回の書き出し方の注意点である。

## 9. 限界

- **読んだのは 1 人である。** 判定は Opus 1 体が 1 回読んだもので、オーナーの意図を知らない。調査の記録は、LLM による品質の判定が、本来は関係の無い摂動で変わることと、人の評価との一致が低いことを記している(CodeJudgeBench、CoReEval)。この記録の判定も、その限界の内にある。判定を繰り返したときの安定性は測っていない。
- **信号の根拠は弱い。** 調査の記録によると、Rust で妥当性が検証された静的メトリクスは見つかっていない。churn の根拠は 1 つの研究グループの結果で、fan-in × churn(Unstable Interface)、co-change without reference(Modularity Violation)、Package Cycle は Java/C/C++ のファイルの単位で測られたものである。クローンの不整合な変更は Rust では研究されていない。今回の集計は 1 つのリポジトリの上位 10 件ずつ(循環は 5 個)で、信号の良し悪しの一般的な証拠ではない。
- **同一性。** unit の同一性は `(path, kind, qualified_name)` なので、改名・移動で履歴が切れ、名前が残ると履歴が誤って続く(8 節)。判定ではコミットの件名と git の履歴で確かめたが、確かめたのは上位の候補だけである。
- **閾値と同点。** `cochange_without_reference` の上位は、閾値と、同点のときの名前順に強く依存する。
- **文脈の取り方。** 呼び出し元・呼び出し先は `unit_edges` の深さ 1 で、`local` の記号、トレイト越しの呼び出し、マクロの中はグラフの記録の制限を引き継ぐ。
- **reload の競合。** `AgentdState` の競合は、コードを読んで導いたもので、再現はしていない。
- **作業ツリー。** 作業ツリーは HEAD 2ebd0ecb のままで、索引・jscpd の報告・キャッシュはその木から作った。`origin/main` の新しいコミットは見ていない。
