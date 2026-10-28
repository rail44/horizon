# 現在のコードでの読み判定・3 回目:数え方の規則・帰属・継承の後(2026-10-28)

コードデータベースの SQL 層([設計文書](../code-database-design.md))に、数え方の 5 つの規則(8 つ目の増分、[数え方の記録](code-db-counting-rules-2026-10-27.md))、前置きの doc コメントと空行の帰属(9 つ目、[帰属の記録](code-db-attribution-2026-10-27.md))、クローンの側の規則(11 個目、[クローンの側の記録](code-db-clone-sides-2026-10-27.md))、系譜の継承(12 個目、[継承の記録](code-db-inherited-lineage-2026-10-27.md))が入った後で、5 つの信号の上位の候補を出し直し、読み判定を行った。あわせて、2 回目の `問題あり` 13 件の今の順位と、それを上位から外した規則を、規則を 1 つずつ前の数え方に戻して調べ、誤報の機構の表を更新した。判定と記述だけを行い、ソースコード、SQL、Rust、`check.py` は変えていない。

依頼文によれば、オーナーは 2026-10-28 に、提案を 1 つずつ相談したうえで次のことを決めた。読み判定は 5 つの信号の上位 10 件と大きい循環 5 個を対象にし、前の判定がある候補はその判定を引き、判定の無い候補だけを読む。2 回目の `問題あり` 13 件は今の順位まで追い、上位から外したものは戻す変数を切り替えて規則を特定する。機構の表は、組んだ機構は実験で、組んでいない機構は読んで、今の上位への効き方を書き直す。オーナーが判定のいくつかを抜き取りで確かめ、1 回目の `board_flow` の問いをオーナーに渡す。2 人目の読み手による一致率は取らない。オーナーの言葉は「3のbも入れましょう。6の一致率は不要です。」である。

前提は [設計文書](../code-database-design.md) の 7〜14 個目の増分と Open points、[1 回目の記録](code-db-dogfood-prospective-2026-10-26.md)(以下「1 回目」)、[2 回目の記録](code-db-dogfood-reading-2-2026-10-26.md)(以下「2 回目」。判定の規則、45 件の判定、6 節の機構の表、7 節、8 節、9 節)、数え方の記録、帰属の記録の 2 節、クローンの側の記録の 2・4・6 節、継承の記録の 4 節、`duckdb-ext/sql/README.md`(Parameters の表と Graph の節)である。記録の決定は問い直していない。この確かめ方の呼び名は、オーナーの求めに従い「現在のコードでの読み判定」とする(2 回目の冒頭)。

## 要約

- **Horizon のソースは 1 回目から変わっていない。** 1 回目の 2ebd0ecb から b3b94fc1 まで、`src/`・`crates/`・`preview-plugin/` の差分は無い(`git diff --stat 2ebd0ecb b3b94fc1 -- src crates preview-plugin` が空)。変わったのは `docs/` と `duckdb-ext/` だけで、unit は `duckdb-ext/` の分だけ 9,264 から 9,293 に増えた(どれも上位に入っていない)。前の判定が引く行番号は、すべてそのまま有効である。候補の出入りは、すべて層の側の変化による。
- **判定の数**(`問題あり` / `問題なし` / `判断できない`):`unit_churn` 5 / 5 / 0、`unit_fan_in_churn` 3 / 7 / 0、`clone_change_consistency` 4 / 6 / 0、`cochange_without_reference` 0 / 10 / 0、`module_cycles` 2 / 2 / 1。合わせて 14 / 30 / 1(2 回目は 13 / 31 / 1)。
- **判定の無かった候補は 22 件で、そのうち `問題あり` は 4 件だった。** 新しい原因は 3 つある。(1) 4 つのリストのモーダル(パレット、ビューの選択、セッションの管理、モデルの選択)が、`render.rs` の重ね描きと `modals.rs` の開く・閉じる・取り消すで並行に手書きされていて、振る舞いの修正がモーダルごとに後から写された(1c27f1ab は同じ修正を 4 箇所に当て、0a921404 のパレットの修正は翌日の d39ea089 で残りの 2 つに写された)。`unit_churn` 8 番と `clone_change_consistency` 7 番の 2 つの信号が指した。(2) テスト専用の `Store.append_event` が本番の `agent_events` の INSERT を写している(`問題あり(小)`)。(3) 設定の型 `NamedProviderConfig` が provider のモデル一覧を取りに行く method を持ち、それが `config.rs` と `providers/rig/model_limits.rs` の循環を閉じている(`問題あり(小)`)。
- **2 回目の `問題あり` 13 件のうち 7 件は今も上位 10 件にある。** 外れた 6 件のうち 5 件(`session_lifecycle.rs` と `commands.rs` の `WorkspaceShell` impl、`AgentdState` impl、`AgentdHandle` と `TerminaldHandle`、`tests.real_git` と `worktree.rs`)は `attribution := 'all'` で戻り、1 件(2 つの `expect_finished`)は `include_tests := true` で戻る。M1(最も内側の unit への帰属)と前置きの帰属は同じ 1 つの変数で戻るので、変数では 2 つを分けられない。継承の規則には戻す変数が無い。
- **2 回目の 10 個の原因のうち 6 個は今の上位に出た。** 出なかった 4 個は、`commands.rs` のボードの処理(method の最高は churn の 332 位)、`AgentdHandle` と `TerminaldHandle` の同じ操作(クローンの 39 位)、継続の 3 つの写し(クローンの 47 位)、real-git のテストの補助関数の写し(クローンの 15 位)である。
- **組んだ規則を 1 つずつ戻すと、今の 45 件のうち 36 件の有無か数が変わる。** そのうち候補自身の数が変わるのは 19 件で、残りの 17 件は戻した規則で入ってくる候補に押し出されるだけである。どの規則を戻しても動かないのは 9 件(churn 2〜5、fan-in 1・4・6、クローン 2・5)である。`cochange_without_reference` の上位 10 件は、すべて結合 100%・共有 3〜5 の同点の帯にあり、N1・帰属・M5 のどれを戻しても大半が押し出される。
- **今回の読みで、co-change とクローンの誤報の新しい形を 5 つ見つけた(N4〜N8)。** 移動のコミットが churn に残ること(`SessionId` impl は 5 コミットのうち 4 つが移動)、新しいビューを試作した 3 日間のコミットが組を作ること、第 3 の unit を通した結び付きが参照として数えられないこと、`mod x;` の宣言とそのファイルの item の組、クローンの側が解決しないとテストの印が付かないことである。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の b3b94fc1 に早送りした(`git merge --ff-only b3b94fc1`、変更の無い状態で実行。`origin/main` の先端はその祖先だった)。測定はこのリポジトリの使い捨ての複製(作業ツリーの `target/code-db/repo`、`git clone --no-checkout` の後 b3b94fc1 を checkout)で行い、作業ツリーのファイルと履歴は測定に使っていない。

機械はオーナーの対話用デスクトップで、他の作業と共有している。DuckDB CLI 2.0.0(`~/.local/bin/duckdb2`、d54cbbe3df)、sitting_duck、rust-analyzer、jscpd 5.3.1(refactor audit の固定版)を使い、すべて `nice -n 19` で動かした。空きディスクは作業の間 17〜21 GB だった。

```sh
nice -n 19 duckdb-ext/build.sh
nice -n 19 uv run --locked --script scripts/refactor-audit/run.py setup
nice -n 19 duckdb-ext/sql/run.sh index target/code-db/repo    # jscpd 1.1 s、rust-analyzer scip 275.1 s
rm -rf target/code-db/repo/target/debug                         # rust-analyzer の 2.7 GB
nice -n 19 duckdb-ext/sql/run.sh query target/code-db/repo unit_churn LIMIT=12   # 全履歴のキャッシュの構築を含めて 29.5 s
nice -n 19 duckdb-ext/sql/run.sh sql target/code-db/repo target/code-db/measure/top_<ラベル>.sql [変数]
```

`top_<ラベル>.sql` は、4 つのクエリ(`queries/unit_churn.sql`、`unit_fan_in_churn.sql`、`clone_change_consistency.sql`、`cochange_without_reference.sql`)の本文をそのまま `CREATE TABLE … AS SELECT row_number() OVER () AS rk, * FROM (…)` で包み、`module_cycles` と `module_cycle_edges` のビューと一緒に、`ATTACH` した `target/code-db/measure/m.duckdb` に書き出すものである(生成する Python とともにコミットしていない)。既定のラベルでは文脈の表(`units` にテストの印・モジュールを付けたもの、自己辺を除く `unit_edges`、`unit_history`、`unit_lineage`、`unit_successions`、`commits`、`unit_cochange`、`unit_changes`、`clone_pairs`、`module_edges`)も書き出した。ラベルと変数は次のとおりで、どれもキャッシュの上で 10〜27 s だった。

| ラベル | 変数 | 戻す規則 |
| --- | --- | --- |
| 既定 | なし | — |
| N1 | `cochange_link_commits=true` | 系譜のリンクのコミットを co-change から除く規則 |
| N2 | `clone_before_both=true` | クローンの片側を両側がそろってから数える規則 |
| 帰属 | `attribution=all` | M1(最も内側の unit への帰属)と、前置きの doc コメント・属性と空行の帰属(1 つの変数で両方が戻る) |
| M5 | `include_tests=true` | テストの unit を co-change の組から除く規則 |
| M6 | `cycle_edges=raw` | 構造上の辺をまとめてから循環を求める規則 |
| overlap | `clone_sides=overlap` | クローンの側を重なる unit の集合にする規則(11 個目の増分。既定は `contain`) |
| overlap+帰属 | `clone_sides=overlap attribution=all` | 上の 2 つを同時に |
| 5 つ | N1・N2・帰属・M5・M6 の 5 つの変数 | 2 回目の数え方 |

継承の規則(12 個目の増分)には戻す変数が無い(README の Parameters の表にも Lineage の節にも無い)ので、切り替えていない。README の Graph の節が挙げる 5 つの変数(`attribution := 'all'`、`cochange_link_commits := true`、`clone_before_both := true`、`include_tests := true`、`cycle_edges := 'raw'`)と `clone_sides` は、どれも存在し、効いた。

上位の作り方は 2 回目と、その後の記録と同じである。`unit_churn` はテストでない unit(`unit_tests`)の上位 10 件。`unit_fan_in_churn` と `cochange_without_reference` はクエリの順。`clone_change_consistency` は、片側だけの変更のある行から両側がテストの行を除き、同じ側の組(ファイルと代表の unit。`overlap` では側の unit の集合も)の断片を 1 つにまとめて数を組の中の最大にし、`one_sided` の降順、`both_sides` の降順に並べた。側の unit が解決しないときは、その側はテストとしない。循環は `module_cycles` のモジュールの数の降順で、同じ数なら名前の順である(3 モジュールの循環が 8 個あり、5 番目は名前の順で最初の `horizon-agent/src/config.rs` になる。数え方の記録 2 節と同じ選び方)。

数は次のとおりだった。`unit_churn` 9,293 行(うち `duckdb-ext/` 415)、`unit_fan_in_churn` 2,915 行、`clone_change_consistency` 1,502 行(片側のある行 520)、`cochange_without_reference` 1,255 組、`module_cycles` 24 個(モジュール 94)、コミット 1,632。jscpd は exact 270 組、normalized 1,232 組で、1・2 回目と同じだった。今の unit で履歴が空のものは 3 個だった(`horizon-board/src/event.rs` の `tests`、`TerminaldHandle.broadcast_terminal_color_scheme`、`TerminaldHandle.stop_and_wait`)。

5 つの変数をすべて戻すと、2 回目の 45 件と同じ顔ぶれが同じ上位に戻った(churn・fan-in・co-change・循環は並びも同じ)。数が違うのは、継承の規則が増やしたコミットの分である(たとえばクローンの `Store.project_event` と `fold_until_terminal` の片側 a は 2 回目の 14 に対して 17。継承の記録 4 節が書く、`Store.project_event` を含む組の数の変化)。`cochange_without_reference` の組は 8,738 で、2 回目の 8,424 より多い(`duckdb-ext/` を含む組は 2 つだけで、残りの差は継承の規則によると読めるが、切り分けていない)。

## 判定の規則

2 回目と同じである。`問題あり` は名指しできる保守の費用や危険があり、それを除くリファクタリングがあるもの、`問題なし` は信号が無害な理由で説明できるもの、`判断できない` は判定にオーナーの意図が要るものである。判定はコードとグラフと履歴から行い、信号の大きさは根拠にしていない。読んだ箇所は `ファイル:行` で示す。行は b3b94fc1 のもので、ソースが変わっていないので 1・2 回目の行番号と同じである。

前の判定がある候補は、同じ unit(クローンと co-change は同じ組、循環は同じ辺)について同じ理由で判定したものを引き、引いた判定の行がその記録のコミットから変わっていないことを `git diff` で確かめた(上のとおり差分は無い)。循環は、2 回目の循環の部分集合として残ったものについて、判定が指した辺が今の循環に残っているかを辺の表で確かめた。

---

## 1. `unit_churn`(テストでない unit の上位 10 件)

「名前で」は `commits_by_name`、「祖先」は `ancestors`、「移動」は `move_commits`(系譜のリンクのコミットの数)である。「2 回目」は 2 回目の順位である。

| # | unit | 種類 | loc | コミット | 名前で | 祖先 | 移動 | 2 回目 | 判定 |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| 1 | `crates/horizon-agentd/src/session/run.rs` `run_session` | function | 242 | 51 | 21 | 3 | 3 | 1 | 問題なし(引用) |
| 2 | `crates/horizon-agent/src/tools/catalog.rs` `build_definitions` | function | 344 | 49 | 2 | 5 | 5 | 2 | 問題なし(引用) |
| 3 | `src/workspace/commands.rs` `WorkspaceShell.execute` | method | 204 | 45 | 30 | 2 | 1 | 4 | 問題なし(引用) |
| 4 | `src/workspace/mod.rs` `WorkspaceShell.new` | method | 74 | 41 | 21 | 2 | 1 | 9 | 問題あり(引用) |
| 5 | `src/workspace/mod.rs` `WorkspaceShell` | type | 102 | 40 | 21 | 2 | 1 | 10 | 問題あり(引用) |
| 6 | `src/workspace/session_lifecycle.rs` `WorkspaceShell.reconcile` | method | 206 | 38 | 25 | 2 | 1 | 12 位(1 回目 10) | 問題あり(引用) |
| 7 | `crates/horizon-agent/src/providers/rig/completion.rs` `run_provider_stream` | function | 64 | 35 | 5 | 6 | 5 | 入った | 問題なし |
| 8 | `src/workspace/render.rs` `Render for WorkspaceShell.render` | method | 291 | 34 | 21 | 2 | 1 | 入った | 問題あり |
| 9 | `crates/horizon-agent/src/providers/rig/session/mod.rs` `spawn_rig_session` | function | 146 | 34 | 12 | 6 | 5 | 入った | 問題なし |
| 10 | `src/terminal/session.rs` `TerminalSession.spawn` | method | 99 | 34 | 32 | 2 | 1 | 18 位(1 回目 6) | 問題あり(引用) |

**2 回目から出た 5 件**はすべて impl で、今の順位は `TerminalSession` impl 932 位(3 コミット)、`session_lifecycle.rs` の `WorkspaceShell` impl 2,433 位(1)、`horizon-workspace` の `Workspace` impl 451 位(5)、`commands.rs` の `WorkspaceShell` impl 2,429 位(1)、`Connection` impl 936 位(3)である。`attribution := 'all'` で 5 件とも 2 回目の順位(3、5、6、7、8 位)に戻る。数え方の記録と帰属の記録が書いた、impl が自分の行の変更しか数えなくなった効果である。

### 判定

前の判定を引いたもの。

1. **`run_session` — 問題なし(2 回目 1 節の 1 番)。** daemon の 1 セッションの組み立ての場所で、環境の構築は 4fe9f4fd で外に出された後である(`run.rs:47-296`)。51 は 2 回目の 50 に前置きの帰属の 1 コミットが加わったもので、`attribution := 'all'` で 50 に戻る。
2. **`build_definitions` — 問題なし(2 回目 1 節の 2 番)。** ツールの一覧というデータの表(`catalog.rs:25-381`)。
3. **`WorkspaceShell.execute` — 問題なし(1 回目の 7 番、2 回目の 4 番)。** `CommandId` の dispatcher で、AGENTS.md の「Operations go through the command model」が決めた変更点(`commands.rs:502-706`)。
4. **`WorkspaceShell.new` — 問題あり(2 回目の 9 番、1 回目の `TerminalSession.spawn` と同じ原因)。** 端末からシェルへの 4 本の送信路を作り、フィールドに入れ、受け手を起動する(`src/workspace/mod.rs:421-424`、450-453、465-468)。
5. **`WorkspaceShell` 型 — 問題あり(2 回目の 10 番、同じ原因)。** 4 本の送信路のフィールド(`src/workspace/mod.rs:383-402`)。
6. **`WorkspaceShell.reconcile` — 問題あり(1 回目の 10 番)。** セッションの store の同期と、ペインの種類ごとのビューの構築が 1 つの関数にある(`session_lifecycle.rs:313-416`)。2 回目は上位外(12 位)で、同じ原因を `session_lifecycle.rs` の impl(2 回目の 5 番)が指していた。今は impl が外れ、method 自身が上位に戻った。数え方の記録 2 節が書いた入れ替わりである。
10. **`TerminalSession.spawn` — 問題あり(1 回目の 6 番)。** `(SessionId, X)` を送る 4 本の `UnboundedSender` を引数に取る(`src/terminal/session.rs:206-214`)。4・5 番と同じ原因の 3 行目である。

判定の無かった 3 件を読んだ。

7. **`run_provider_stream` — 問題なし。** provider の 1 ターンのストリームの本体で、要求を組み、確立と各 chunk を締め切りと取り消しつきで待ち、終わりを `attempt.close` に渡す 64 行である(`crates/horizon-agent/src/providers/rig/completion.rs:445-509`)。応答の組み立ては `completion/{attempt,response,outcome}.rs` に出されている(563903a4「Separate provider response assembly from transport waits」)。35 コミットのうち 30 は、a5f6353d(2026-09-18「Support multiple LLM providers…」の `content_match`)より前の OpenAI 専用の前身 `rig_openai_turn_streaming` のもので、件名は max_tokens、履歴の刈り込みの追加と削除、切り詰めの検出、知識の層のような、ターンの機能を足すたびの変更だった。今の名前になってからの 4 コミットは、応答の組み立ての分離、実行結果の明示、中断したツールの束の処理、rig-core 0.44 への移行である。機能を足すたびに触る形は、すでに分解された後の履歴である。`rig_tool_definitions(config.allowed_tool_ids…, config.trusted_project)`(`completion.rs:463-466`)がセッションごとの値を `RigAgentConfig` から読むのは、`RigAgentConfig` の判定の原因で、ここでは数えない。
8. **`Render for WorkspaceShell.render` — 問題あり。** シェルの根の描画で、action の結び付けと、4 つのリストのモーダル(パレット、ビューの選択、セッションの管理、モデルの選択)の重ね描きを持つ(`src/workspace/render.rs:996-1288`)。重ね描きは 4 つとも同じ形の約 40 行で、違うのは id、幅と高さ、取り消しの handler、セッションの管理だけが持つ key context と 2 つの action である(`render.rs:1112-1152`、1153-1193、1194-1245、1246-1287)。この写しは実際に費用を生んだ。1c27f1ab(2026-08-05「Fix board modal: click opens detail instead of closing」)は、行をクリックするとモーダルが閉じる不具合をボードのモーダルで見つけ、同じ潜在的な不具合が他の 3 つにもあったとして、`on_mouse_down(… stop_propagation)` と同じ注釈を 4 箇所に足した(コミットの文書がそう書いている)。モーダルを 1 つ足すと、`modals.rs` の開く・閉じる・取り消すと、ここの重ね描きを写すことになる(cc9a48b6 がモデルの選択を足したとき)。直し方は、id・大きさ・取り消し・リストを引数に取る重ね描きの部品を 1 つ作ることである。`clone_change_consistency` の 7 番も同じ原因を `modals.rs` の側から指している。34 コミットの残りは、移行(M2〜M5)、ウィンドウの装飾、タブ、フォント、IME のような、根の描画に機能を足した変更である。
9. **`spawn_rig_session` — 問題なし。** rig のセッションの起動の場所で、スレッドを立て、projection の store を待たずに読み、履歴を読み、失敗を事象として返し、初期の事象を送り、`SessionLoopState` を走らせる(`crates/horizon-agent/src/providers/rig/session/mod.rs:62-212`)。セッションのループは faeccb05(2026-08-04「bundle run_session_loop state into SessionLoopState, split session.rs」)で 10 個の子モジュールに分けられ、その後の 12 コミットは、信頼の門、知識、記憶、wake、ボード、複数 provider、構成の snapshot、projection の待ちの解消のように、起動の段取りに 1 つずつ足したものだった。ここで `config.trusted_project = trusted_project` と設定の複製を書き換えている(`mod.rs:84-91`)のは、1 回目の `RigAgentConfig` の判定(セッションごとの値をプロセスの設定の複製の書き換えで作る)の箇所で、原因は `RigAgentConfig` の側にある。

## 2. `unit_fan_in_churn`(fan-in × コミットの上位 10 件)

| # | unit | 種類 | fan-in(ファイル) | コミット | 移動 | 積 | 2 回目 | 判定 |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |
| 1 | `crates/horizon-agent/src/contract.rs` `Event` | type | 89 | 24 | 1 | 2,136 | 1 | 問題なし(引用) |
| 2 | `crates/horizon-agentd/src/session/state.rs` `AgentdState` | type | 30 | 26 | 4 | 780 | 3 | 問題あり(引用) |
| 3 | `crates/horizon-agent/src/contract/tool_result.rs` `ToolCallResult` | type | 59 | 12 | 2 | 708 | 7 | 問題なし(引用) |
| 4 | `crates/horizon-agent/src/contract.rs` `Command` | type | 41 | 17 | 1 | 697 | 6 | 問題なし(引用) |
| 5 | `crates/horizon-agent/src/contract.rs` `ToolCallRequest` | type | 64 | 9 | 1 | 576 | 11 位(1 回目 6) | 問題なし(引用) |
| 6 | `crates/horizon-agent/src/config.rs` `RigAgentConfig` | type | 25 | 23 | 1 | 575 | 10 | 問題あり(引用) |
| 7 | `crates/horizon-agent/src/contract.rs` `MessageRole` | type | 46 | 11 | 3 | 506 | 入った | 問題なし |
| 8 | `src/workspace/mod.rs` `WorkspaceShell` | type | 12 | 40 | 1 | 480 | 入った(churn の 10 番) | 問題あり(引用) |
| 9 | `crates/horizon-wire/src/session.rs` `SessionId` | impl | 78 | 5 | 4 | 390 | 入った | 問題なし |
| 10 | `crates/horizon-workspace/src/commands.rs` `CommandId` | type | 13 | 30 | 1 | 390 | 入った | 問題なし |

この表は帰属の記録 2 節の「後」の上位 10 件、継承の記録 4 節の上位 10 件と同じである。**2 回目から出た 5 件**はすべて impl で、今の順位は `ToolSessionState` impl 213 位(31 × 1)、`LiveState` impl 197 位(34 × 1)、`AgentdState` impl 62 位(21 × 5)、`Workspace` impl 91 位(14 × 5)、`WriterHandle` impl 59 位(27 × 4)である。`attribution := 'all'` で 5 件とも上位に戻る(2、4、5、8、9 位)。

### 判定

前の判定を引いたもの。

1. **`Event` — 問題なし(1 回目の 1 番)。** イベントログの語彙で、網羅的な `match` への伝播をコンパイラが強制する(`contract.rs:273-423`)。
2. **`AgentdState` 型 — 問題あり(2 回目 2 節の 3 番)。** 同じ snapshot から作る `providers` と `agent_config` を別々の `Mutex` のフィールドとして持つ(`horizon-agentd/src/session/state.rs:42`、46)。reload は 2 つを別々に入れ替え(`state.rs:183-193`)、起動は別々に読む(`session/run.rs:79-88`、112)。
3. **`ToolCallResult` — 問題なし(2 回目 2 節の 7 番)。** wire のデータ型で、変更はコンパイラが使う側に強制する(`contract/tool_result.rs:26-31`)。
4. **`Command` — 問題なし(1 回目の 3 番)。** wire のコマンドと内部の 3 つの variant の同居は意図した形と読める(`contract.rs:249-262`)。
5. **`ToolCallRequest` — 問題なし(1 回目の 6 番)。** 7 行のデータ型で、fan-in は契約の型を皆が使うことを示すだけである(`contract.rs:871`)。
6. **`RigAgentConfig` — 問題あり(1 回目の 5 番)。** 接続、ループの調整値、セッションごとの値の 3 種類を 1 つの構造体が持つ(`config.rs:519-610`)。
8. **`WorkspaceShell` 型 — 問題あり(2 回目 1 節の 10 番、送信路の原因)。** 2 回目はこの unit を churn で判定した。fan-in 12 ファイルが加わっただけで、判定の理由(`src/workspace/mod.rs:383-402` の 4 本の送信路のフィールド)は変わらない。

判定の無かった 3 件を読んだ。

7. **`MessageRole` — 問題なし。** ログの永続化される wire の enum で、`User`・`Assistant` と、システムが入れる 2 つの variant(`TaskNotification`、`AutoContinue`)を持つ(`crates/horizon-agent/src/contract.rs:649-` 。2 つの variant の文書は「Every consumer that distinguishes "who said this" therefore has to name this variant explicitly」と書く)。fan-in 46 ファイルは、メッセージを作る側と、役割で分岐する側(`providers/rig/conversation/upgrade.rs:287`、`horizon-agentd/src/hub/mapping.rs:258`・276、`src/agent/view/adapter.rs:255`・313 など)で、variant を足すと網羅的な `match` をコンパイラが直させる。11 コミットは、作成と移動の 4 つ(d1379c69、e41cce7e、e899e99d、b11f2d91)、wire 全体に `Unknown` の受け皿を足して後で外した一連の 4 つ(9cff40ba、296e1df4、92951f4e、ba588936。1 回目の `KeyEventKind` の組を作ったのと同じ一連)、2 つの variant の追加(c2b5d9fa、dfaafef3)と、2f45a0f8 の可視性の縮小だった。契約の型の形である。2 つの variant の文書がほぼ同じ文で写されているのは、文書の重複で、保守の費用と呼ぶほどではない。
9. **`horizon-wire` の `SessionId` impl — 問題なし。** UUID の newtype の 3 つの method(`new`、`as_uuid`、`from_uuid`)の 13 行である(`crates/horizon-wire/src/session.rs:26-38`)。5 コミットは、b19e9e32(最初のプロトタイプでの作成)と、4d43b283・e899e99d・b11f2d91・1bca9054(モジュールの分割、エージェントの整理、クレートの分割、`horizon-wire` の切り出し)の 4 つの系譜のリンクのコミットで、`move_commits` 4 である。本体の変更は 1 つも無い。fan-in 78 ファイルはセッションの id を皆が使うことを示し、積はほぼ移動の数でできている(機構 N4、7 節)。
10. **`CommandId` — 問題なし。** ユーザーに見える操作の列挙で、AGENTS.md の「Operations go through the command model」が、操作を足すたびに variant を 1 つ足すことを決めている(`crates/horizon-workspace/src/commands.rs:1-`)。30 コミットの件名は、ほぼすべてコマンドの追加(終了、再読み込み、分割、セッションの管理、ボード、フォントの大きさ、モデルの選択、プレビュー)で、残りは可視性の縮小(7133c643)と改名(4e7fb81e、b293051a、e018669d)だった。fan-in 13 ファイルは、dispatcher(`execute`)、有効判定、表題、キーの対応、パレットで、variant を足すとそれぞれの網羅的な `match` をコンパイラが直させる。1 回目の `execute` の判定と同じ理由である。

## 3. `clone_change_consistency`(片側だけの変更がある組の上位 10 件)

| # | 側 a | 側 b | 片側(a / b) | 両側 | 断片の始まり(a / b) | 出所 | 2 回目 | 判定 |
| --- | --- | --- | --- | ---: | --- | --- | --- | --- |
| 1 | `src/terminal/session.rs` `TerminalSession.spawn` | `src/workspace/mod.rs` `WorkspaceShell`(型) | 14 / 34 | 6 | 208 / 382 | normalized | 1 | 問題あり(引用) |
| 2 | `frame/fold.rs` `apply_agent_event_to_frame` | `projection.rs` `Store.project_event` | 11 / 8 | 18 | 240 / 137 | exact, normalized | 7 | 問題なし(引用) |
| 3 | `src/keymap.rs` `command_for` | `src/terminal/input.rs` `term_key_code` | 17 / 0 | 0 | 127 / 188、157 / 196 | normalized | 6 | 問題なし(引用) |
| 4 | `frame/fold.rs` `apply_agent_event_to_frame` | `tools/explore.rs` `fold_until_terminal` | 7 / 7 | 10 | 242 / 671 | exact, normalized | 2 | 問題なし(引用) |
| 5 | `frame/fold.rs` `apply_agent_event_to_frame` | `frame/test_support.rs` `render_agent_transcript`(テスト) | 8 / 5 | 21 | 240 / 156 | exact, normalized | 9 | 問題あり(小)(引用) |
| 6 | `projection.rs` `Store.project_event` | `tools/explore.rs` `fold_until_terminal` | 5 / 8 | 9 | 103 / 648 | normalized | 4 | 問題なし(引用) |
| 7 | `src/workspace/modals.rs` `WorkspaceShell.open_session_manager` | 同じファイルの `WorkspaceShell.open_palette` | 4 / 3 | 5 | 348 / 520 | normalized | 入った | 問題あり |
| 8 | `src/runtime/routing.rs` `AgentRoutes.register_agent` | 同じファイルの `TerminalRoutes.register_terminal` | 2 / 5 | 2 | 176 / 467 | normalized | 入った | 問題なし |
| 9 | `crates/horizon-agent/src/tools/tests.rs`(unit に解決しない) | `src/board/model.rs` `tests`(テスト) | 0 / 6 | 0 | 187 / 631 ほか 5 断片 | normalized | 入った | 問題なし |
| 10 | `duckdb/append.rs` `Store.append_record_uncommitted` | 同じファイルの `Store.append_event`(テスト) | 2 / 3 | 11 | 114 / 186、125 / 189 | exact, normalized | 入った | 問題あり(小) |

この表は継承の記録 4 節の「後」の上位 10 件と同じ顔ぶれである(10 位の後に `open_view_chooser` と `open_palette`、3 / 2・両側 5 が 11 位で続く)。**2 回目から出た 4 件**の今の順位は、`AgentdHandle` と `TerminaldHandle` 39 位(1 / 0、両側 1)、mock provider の 2 つの shutdown 12 位(2 / 3、3)、`tests.real_git` と `worktree.rs` 15 位(4 / 0)、2 つの `AgentFrameItem` 43 位(0 / 1)である。

### 判定

前の判定を引いたもの。

1. **`spawn` の引数と `WorkspaceShell` のフィールド — 問題あり(1 回目の 1 番)。** 4 本の `UnboundedSender` の型の並びが 2 箇所にある(`session.rs:208-213`、`src/workspace/mod.rs:382-403`)。
2. **`apply_agent_event_to_frame` と `project_event` — 問題なし(1 回目の 4 番)。** `Event` の網羅的な `match` の no-op の腕の一覧(`frame/fold.rs:240-251`、`projection.rs:137`)。
3. **`command_for` と `term_key_code` — 問題なし(1 回目の 5 番)。** 領域の違う対応表の見かけの一致。
4. **`apply_agent_event_to_frame` と `fold_until_terminal` — 問題なし(1 回目の 6 番)。** 2 番と同じ機構。
5. **`apply_agent_event_to_frame` と `render_agent_transcript` — 問題あり(小)(2 回目 3 節の 9 番)。** テスト用の描画で、監査の 10 個の事象が `MemorySeeded` の腕に入り「memory: seeded」と描かれる(`frame/test_support.rs:156-166`、8e057dd7)。
6. **`project_event` と `fold_until_terminal` — 問題なし(1 回目の 2 番)。** 2 番と同じ機構。

判定の無かった 4 件を読んだ。

7. **`open_session_manager` と `open_palette` — 問題あり(`render` と同じ原因)。** `modals.rs` は 4 つのリストのモーダルのそれぞれに、開く(`ListState` を作り、最初の行を選び、`ListEvent` を購読し、focus を移し、`ListModal` に入れる)、閉じる(3 行)、取り消す(2 行)を持つ(`src/workspace/modals.rs:107-155`、163-249、347-425、516-572)。断片はその開く関数の前半である。2 つの関数の履歴に、修正がモーダルごとに後から写された跡がある。0a921404(2026-07-21「palette: keep workspace mode active on cancel」)はパレットの取り消しだけを直し(この組の片側 b)、翌日の d39ea089(「modals: keep workspace mode active when view chooser/session manager cancel」)が同じ直しを他の 2 つに当てた。その後、共通の部分は `select_first_row_on_open`・`restore_focus_after_modal_cancel`・`ListModal` に出されたが、関数の骨組みと `render.rs` の重ね描きは 4 つのままである。これは、片側だけの変更の数が伝播の遅れを実際に含んでいた、3 回の読み判定で初めての例である(ただし 0a921404 が変えた行は取り消しの腕で、jscpd の断片の行 348-360 / 520-532 の外にある)。費用と直し方は `render` の判定(1 節の 8 番)と同じで、1 つの原因として数える。
8. **`register_agent` と `register_terminal` — 問題なし。** 2 つの daemon ごとの経路の表の登録で、鍵を作り、lock を取り、粘着する失敗があれば失敗を送って返し、経路を入れる(`src/runtime/routing.rs:174-196`、463-488)。モジュール文書が「One table per daemon connection, each with its own sticky failure, so a failure of one runtime is visible only to that runtime's panes」と、表を分けることを決めている(`routing.rs:1-4`)。共通の部分(世代つきの鍵 `RouteKey`)はすでに総称にされている。経路の型はそれぞれの領域の形(ACP の inbound と ready、端末の frame の watch と command)である。2 つを一緒に変えたコミットは 67cffe45(作成)と da970fd8(「Scope runtime routes to individual attachments」で両方に同時に入った)で、片側だけのコミットは、端末の側の frame の経路の変更(7ce390cb、266370be、0247b3f3、a4fd9605、2086223e)と、エージェントの側の ACP への移行(c96f685a、8c52fe5a)だった。どれも片方の領域だけの変更で、伝播の漏れではない。この組は継承の規則で上位に入った(継承の記録 4 節:2 / 0、1 → 2 / 5、2。`register_agent` の系譜に a4fd9605 の `inherited` のリンクがある)。
9. **`tools/tests.rs` と `src/board/model.rs` の `tests` — 問題なし(見かけの一致)。** どちらもテストの関数で、`assert_eq!(関数("文字列"), Some(定数))` を並べたものである。側 a は承認の種類の表(`crates/horizon-agent/src/tools/tests.rs:187-210`)、側 b はボードのキーの対応(`src/board/model.rs:631-662`)で、領域が違う。1 回目の 5 番(`command_for` と `term_key_code`)と同じ、識別子を正規化したときの一致である。側 a は断片が `#[test]` の行(関数の前置き)から始まるので、`contain` では関数の `start_line` に含まれず解決しない(クローンの側の記録 6 節の「`contain` は前置きから始まる断片を解決しなかった」と同じ)。解決しない側はテストとされないので、両側がテストの組を除く手順を通り抜けた(機構 N8、7 節)。片側 6 は、側 b のモジュール全体の変更の数である。
10. **`append_record_uncommitted` と `append_event` — 問題あり(小)。** 側 a は本番の 1 レコードの書き込み(`crates/horizon-agent/src/persistence/projection/duckdb/append.rs:112-174`)、側 b は `#[cfg(test)]` の 1 事象の書き込み(`append.rs:182-249`)で、b は a の `agent_events` の INSERT(10 列の列名と値の並び)と `upsert_session`・`project_event` の呼び出しを写している。違いは、連番を表から求めること、`event_at` を `now()` にすること、`session_context` を渡さないことである。SQL の文字列なのでコンパイラは 2 つの食い違いを止めない。履歴では、両側を一緒に変えたコミットが 11 あり(列の追加、タイムスタンプ、projection の有効化など)、スキーマの変更は 2 回払われてきた。片側だけの変更は、a の 8fb31975(本番の書き込みを transaction で包む修正と、その中身を `append_record_uncommitted` に分けたもの)と ba588936、b の可視性の縮小 2 つだった。`append_event` と、それを呼ぶ `append_events` は `append.rs` の外の 49 箇所(テストと計測用の `bench_probe.rs`)から呼ばれ、本番の経路(`append_record`、`append_record_uncommitted`)をテストが直接呼ぶのは `duckdb/tests.rs` の 7 箇所である。projection のテストの多くは本番の INSERT を通らない。費用はテストの中にある。直し方は、テストの補助関数が `Record` を作って `append_record_uncommitted` を呼ぶことである。境界の判定なので、抜き取りの一覧(10 節)に入れた。

## 4. `cochange_without_reference`(既定の閾値で上位 10 組)

既定の閾値は `min_shared` 3、`min_coupling_pct` 30、`max_commit_files` 50 で、テストの unit の組と系譜のリンクのコミットは除かれている(数え方の記録 1 節)。上位 10 組はすべて結合 100% で、共有は 5(1 番)、4(2〜9 番)、3(10 番)である。10 番の後にも、結合 100%・共有 3 の組が続く(11〜14 番は 10 番と同じ `ModelClient` と judge のモジュールの組)。並びは結合、共有の降順、その後はパスと名前の順なので、2〜9 番と 10 番以下の中の順は同点の並べ方である。

| # | unit a | unit b | 共有 | a / b のコミット | 2 回目 | 判定 |
| --- | --- | --- | ---: | --- | --- | --- |
| 1 | `src/agent/view/mod.rs` `AgentView.render_composer_hints` | `src/agent/view/units.rs` `stop_button` | 5 | 5 / 5 | 入った | 問題なし |
| 2 | `crates/horizon-agent/src/hosting.rs` `ProviderSummary` | `crates/horizon-agentd/src/providers.rs` `named_provider_configs` | 4 | 4 / 4 | 入った | 問題なし |
| 3 | `horizon-workspace/src/persistence.rs` `From<ViewKind> for ViewKindState.from` | `horizon-workspace/src/types/kind.rs` `ViewKind.title` | 4 | 4 / 4 | 入った | 問題なし |
| 4 | `persistence.rs` `From<ViewKindState> for ViewKind.from` | `types/kind.rs` `ViewKind.title` | 4 | 4 / 4 | 入った | 問題なし |
| 5 | `persistence.rs` `ViewKindState` | `types/kind.rs` `ViewKind` | 4 | 4 / 4 | 入った | 問題なし |
| 6 | `persistence.rs` `ViewKindState` | `types/kind.rs` `ViewKind.title` | 4 | 4 / 4 | 入った | 問題なし |
| 7 | `src/agent/view/mod.rs` `placeholder` | `src/agent/view/units.rs` `task_lines` | 4 | 4 / 4 | 入った | 問題なし |
| 8 | `src/agent/view/mod.rs` `placeholder` | `src/agent/view/units.rs` `turn_chip` | 4 | 4 / 4 | 入った | 問題なし |
| 9 | `src/workspace/commands.rs` `WorkspaceShell.control_plane_terminate` | `src/workspace/session_events.rs` `WorkspaceShell.handle_terminal_exited` | 4 | 4 / 4 | 入った | 問題なし |
| 10 | `crates/horizon-agent/src/judge/client.rs` `ModelClient` | `crates/horizon-agent/src/judge/mod.rs` `client`(`mod client;`) | 3 | 3 / 3 | 入った | 問題なし |

この表は、数え方の記録 2 節と帰属の記録 2 節が判定せずに並べた上位と、ほぼ同じ顔ぶれである。**2 回目の 10 組はすべて出た。** テストの unit を含む 4 組(1・2・3・8 番。`expect_finished` の組を含む)は、テストを除く規則で順位が無くなった。`NamedProviderConfig` の 2 組(4・5 番)は、型の doc コメントだけを変えたコミット(66d2113d)が前置きの帰属で型の変更になり、結合 83.3% で 90・91 位になった(帰属の記録 2 節)。`Definition` と `tool_result_message`(6 番)は、リンクのコミット 2 つが除かれて共有 3 になり、73 位である。`horizon-workspace` の 4 組(7・9・10 番、8 番はテスト)は、リンクのコミット 3 つが除かれて共有 2 になり、`min_shared` を下回って順位が無い。

### 判定

すべて判定の無かった組である。

1. **`render_composer_hints` と `stop_button` — 問題なし。** エージェントのビューの、入力欄の下の行(モデルの chip と送信の案内、`src/agent/view/mod.rs:304-320`)と、実行中のターンの「Stop」ボタン(`src/agent/view/units.rs:91-100`)で、互いを参照せず、一緒に変える理由も無い。共有 5 は、どれも 2026-10-11〜13 の、新しいエージェントのビューを試作のプレビューとして描き直していた間のコミット(24653bcc「…as six previews」、d046662b、a71fea93、232c21a1「…add agent-33/34/35 variations」、2a641a56)で、採用して今のビューに置き換えた 4a572454 は両側にとってリンクのコミットなので除かれている。試作の各回がビューの部品を一斉に描き直した跡である(機構 N5)。
2. **`ProviderSummary` と `named_provider_configs` — 問題なし。** `ProviderSummary` は `_horizon/list_providers` が返す wire の型(`crates/horizon-agent/src/hosting.rs:115-`)、`named_provider_configs` は設定ファイルの解決結果を `NamedProviderConfig` に写す変換(`crates/horizon-agentd/src/providers.rs:7-31`)である。共有 4 は、複数の provider への対応(a5f6353d)、モデルの別名の置き換えと作り直し(6f42b611、8a59ba1c)、`default_model`・`small_model` の組(fb873434)で、設定の形の変更が、設定ファイル → `NamedProviderConfig` → wire の 3 つの表現に伝わったものである。`horizon-agent` は `horizon-config` に依存しないという AGENTS.md の Configuration の節の境界の写しで、1 回目の 2・3 番(`NamedProviderConfig` と `RawConfig.resolved_providers`、`ResolvedProviderConfig`)と同じ理由である。
3. **`From<ViewKind> for ViewKindState.from` と `ViewKind.title` — 問題なし。** `ViewKindState` は `ViewKind` の永続化の写しで、型の文書が「Persisted counterpart of [`ViewKind`]; add a variant here alongside a new `ViewKind` for every future first-party view」と書く(`crates/horizon-workspace/src/persistence.rs:150-170`)。2 つの `From` は網羅的な `match` で、`ViewKind` に variant を足すと、`From` の両方と `title` をコンパイラが直させる(`persistence.rs:534-552`)。共有 4 は、ビューの種類を足した 4 つのコミット(4e550ae8 のテーマ設定、8bdc116e のボード、4979dfce のプレビュー、34223101 のボードの 2 つのペインへの分割)である。`title` と `from` は互いを参照しないが、同じ enum の網羅的な `match` どうしである(機構 M3 と N6)。
4. **`From<ViewKindState> for ViewKind.from` と `ViewKind.title` — 問題なし。** 3 番と同じ。
5. **`ViewKindState` と `ViewKind` — 問題なし。** 3 番と同じ。2 つの型は互いを参照せず、`From` の impl を通してだけつながる。
6. **`ViewKindState` と `ViewKind.title` — 問題なし。** 3 番と同じ。
7. **`placeholder` と `task_lines` — 問題なし。** 入力欄の placeholder の文言を状態で選ぶ 7 行の関数(`src/agent/view/mod.rs:336-342`)と、背景のタスクを 1 行ずつ並べる 8 行の関数(`src/agent/view/units.rs:121-128`)である。共有 4 は 1 番と同じ試作の各回だった。
8. **`placeholder` と `turn_chip` — 問題なし。** 7 番と同じ(`turn_chip` は `units.rs:59-66`)。
9. **`control_plane_terminate` と `handle_terminal_exited` — 問題なし。** CLI の制御面からの終了(`src/workspace/commands.rs:908-922`)と、シェルが終了したときの処理(`src/workspace/session_events.rs:271-285`)で、どちらも「復元中なら何もしない、モデルの `terminate_session` を呼ぶ、`reconcile` する」の 3 段である。終了の本体はモデルの `Workspace::terminate_session` にあり、2 つはそれを呼ぶ別々の入口である。`blocks_mutation` の確かめは、シェルの変更の入口の多くにある(`src/workspace/` の `recovery.rs` 以外の 6 ファイルに 24 行)。共有 4 は、制御面の作成(43ac4021)、レイアウトの永続化(6e43e282)、シェルの終了がセッションを終える変更(704657ba)、空のワークスペースを正当な状態にした変更(9bda1b01)と 5ce74c00 で、シェルの入口すべてに及んだ変更だった。
10. **`ModelClient` と `mod client;` — 問題なし(信号の誤り)。** `judge/mod.rs:50` の `mod client;` は宣言で、中身は `judge/client.rs` にある(`ModelClient` はそのトレイト、`client.rs:50-56`)。共有 3 は、judge の作成(f7a93ed9)、判断モデルへの置き換えで chat の judge を消したコミット(cad2cfab)、small_model の chat の judge を戻したコミット(ff10f1ef)で、どちらの unit もファイルごと消えて戻った(`unit_history` は added、removed、added)。宣言と、そのファイルの item は、参照の辺を持たないので「参照の無い co-change」になる(機構 N7)。11〜14 番(`ModelClient` と `mod parse;`・`mod prompt;`・`parse.rs` の 2 つの関数)も同じ出来事である。

## 5. `module_cycles`(実際の循環のうち大きい 5 個)

| # | 循環(最小の要素) | モジュール | 内側の辺 | unit の辺 | 2 回目 | 判定 |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 1 | `crates/horizon-agent/src/judge/handle.rs` | 25 | 64 | 187 | 1 番(40)の部分 | 問題あり(小)(引用) |
| 2 | `crates/horizon-agentd/src/board_flow/operations.rs` | 9 | 16 | 33 | 4 番(11)の部分 | 判断できない(引用) |
| 3 | `crates/horizon-agent/src/transcript/diff.rs` | 7 | 13 | 30 | 上位外(8、6 位) | 問題なし |
| 4 | `crates/horizon-agent/src/providers/rig/clearing.rs` | 5 | 9 | 21 | 5 番(9)の部分 | 問題なし(引用) |
| 5 | `crates/horizon-agent/src/config.rs` | 3 | 4 | 11 | 上位外(3、13 位) | 問題あり(小) |

この 5 個は数え方の記録 2 節の「後の大きい 5 個」と同じである。2 回目の 2 番(`src/workspace/*` と `pane.rs`、`問題なし`)と 3 番(`registry.rs` と provider、`問題なし`)は、構造上の辺をまとめる規則で 2 モジュールと 3 モジュールの循環に縮み、上位から外れた(数え方の記録 2 節)。

### 判定

前の判定を引いたもの。辺の表で、判定が指した辺が今の循環に残っていることを確かめた。

1. **`judge/*`・`policy.rs`・`tools/*` — 問題あり(小)(1 回目の 1 番)。** judge から tools への辺は、`start_approval_gate`(`judge/mod.rs:301-345`)の `ToolCompletion`・`ToolSessionState`・`live_frame_for_session` と、`builtin_tool_description` の `Definition`・`definitions`、`JudgeHandle.start` の `ToolCompletion` で、1 回目が指した辺そのものである。
2. **`board_flow/operations.rs` と `session/*` — 判断できない(1 回目の 4 番)。** session から board_flow への辺は、1 回目が指した 2 本(`session/board.rs:105` の `BoardHost for AgentdBoardHost.operate` から `operate`、`session/connection.rs:113` の `ensure_board_organizer` から `organizer_session`)だけだった。問いは 11 節に書いた。
4. **`rig/clearing.rs`・`completion.rs`・`conversation/*`・`mapping.rs` — 問題なし(1 回目の 5 番)。** 親子の参照(`completion/*`)はまとめられて消え、残った循環を閉じる辺は、1 回目が指した `conversation/upgrade.rs` の `restore_tool`(`upgrade.rs:148-`)から `completion.rs:640` の `replay_safe_tool_arguments` への 1 本だった。

判定の無かった 2 個を読んだ。

3. **`transcript/diff.rs` と `transcript/tool_call/*`・`receipt.rs` — 問題なし。** `diff.rs` は、行の差分の再構成(`DiffLine`、`reconstruct_line_diff`、`crates/horizon-agent/src/transcript/diff.rs:75-`)と、セッション全体のファイルの変更の集計(`aggregate_changes`、`diff.rs:40-64`)の 2 つを持ち、モジュール文書がそう書いている(`diff.rs:1-3`)。`tool_call/util.rs` の `line_diffstat` が前者を使い、後者が `tool_call/view.rs` の `ToolCallView` と `receipt.rs` の `CallClass` を使うので、2 つの向きの辺ができる。`tool_call/view.rs` と `classify.rs` の間は、view が分類を呼び、分類が view の型(`ToolCallKind`、`FileEffect`)を使う、型と組み立てを同じファイルに置いた形である。どれも 1 つの view-model のモジュール(`transcript`、そのモジュール文書が「structural transcript view-model」と書く)の中の辺で、一緒の変更を強いる仕組みは見当たらない。`aggregate_changes` を `receipt.rs` の集計の隣に移せば `diff.rs` を通る辺は切れるが、名指しできる費用は無い。
5. **`config.rs`・`providers/rig/model_limits.rs`・`model_catalog.rs` — 問題あり(小)。** `model_limits.rs` と `model_catalog.rs` から `config.rs` への辺(`RigAgentConfig`、`ProviderKind`)は、provider の adapter が設定を読む普通の向きである。循環を閉じるのは逆向きの 1 本で、設定の型 `NamedProviderConfig` の method `list_model_ids`(`crates/horizon-agent/src/config.rs:414-431`)が、API キーを環境変数から読み、`crate::providers::rig::list_model_ids`(`providers/rig/model_limits.rs:203-226`、`GET {base}/models` を投げてプロセスの間キャッシュする)を呼ぶ。設定のモジュールは、解決した値を持つ場所である(AGENTS.md の Configuration の節は、`horizon-agent` が解決した provider の値を普通の引数として受け取ると書く)。そこにネットワークの取得が入り、設定のモジュールが rig の adapter に依存している。呼び出し元は daemon の 1 箇所(`crates/horizon-agentd/src/session/connection.rs:88`)だけである。method の文書は、`resolved` と同じ解決の順(種類の環境変数、項目の `base_url`、種類の既定)を使うためにここに置いたと読める。直し方は、解決した URL(`discovery_base_url`)だけを設定の側に残し、取得を provider の側に置くことである。judge の循環と同じ、置き場所を外れた 1 つの関数が循環を閉じる形で、費用は小さい。置き場所の意図が読み取りと違えば `問題なし` になる判定なので、抜き取りの一覧(10 節)に入れた。

## 6. 2 回目の `問題あり` 13 件の追跡

順位は各信号の上位の作り方での順位(churn はテストでない unit の中、クローンは組をまとめた後)、括弧はその信号の数(churn はコミット、fan-in は積、クローンは片側 a / b、循環はモジュールの数)である。「戻す」は、規則を 1 つずつ前の数え方に戻したときに上位 10 件に戻る変数と、そのときの順位である。

| 2 回目 | 候補 | 1 回目 | 2 回目 | 今 | 上位から外した規則(戻した順位) |
| --- | --- | --- | --- | --- | --- |
| churn 5 | `session_lifecycle.rs` の `WorkspaceShell` impl | churn 2(44)、fan-in 8 | 5(44) | 2,433 位(1) | `attribution := 'all'` → 5 位(44)。数え方の記録で M1 により 24 コミット・24 位、帰属の記録で 1 コミットになった |
| churn 7 | `commands.rs` の `WorkspaceShell` impl | churn 3(42) | 7(42) | 2,429 位(1) | `attribution := 'all'` → 7 位(42)。数え方の記録で M1 により 16 コミット |
| churn 9 | `WorkspaceShell.new` | 上位外 | 9(41) | **4**(41) | — |
| churn 10 | `WorkspaceShell` 型 | 上位外 | 10(40) | **5**(40)、fan-in **8** | — |
| fan-in 3 | `AgentdState` 型 | 上位外 | 3(750) | **2**(780) | — |
| fan-in 5 | `AgentdState` impl | fan-in 7(357) | 5(714) | 62 位(105) | `attribution := 'all'` → 5 位(714)。帰属の記録で 24 → 5 コミット |
| fan-in 10 | `RigAgentConfig` | fan-in 5(450) | 10(575) | **6**(575) | — |
| クローン 1 | `TerminalSession.spawn` と `WorkspaceShell` | 1(28 / 17) | 1(28 / 34) | **1**(14 / 34) | — |
| クローン 3 | `AgentdHandle` と `TerminaldHandle` | 8(9 / 0) | 3(25 / 0) | 39 位(1 / 0、両側 1) | `attribution := 'all'` → 7 位(11 / 0、両側 4)。`clone_sides := 'overlap'` だけでは戻らない(method の組に分かれ、最高 28 位の `register_agent` と `register_terminal` 3 / 0、両側 3)。2 つを同時に戻すと 10 位(`begin_reload` の組 10 / 0、両側 3。impl が境の行で側の集合に入り、`all` で中の変更をすべて数える)。`clone_before_both := true` は 44 位(2 / 0) |
| クローン 9 | `apply_agent_event_to_frame` と `render_agent_transcript` | 上位外 | 9(8 / 5) | **5**(8 / 5) | — |
| クローン 10 | `setup.rs` の `tests.real_git` と `worktree.rs` | 上位外 | 10(11 / 0) | 15 位(4 / 0) | `attribution := 'all'` → 8 位(11 / 0)。数え方の記録で M1 により 6 / 0・11 位、帰属の記録で 14 位 |
| co-change 1 | 2 つの `expect_finished` | 1 | 1 | 順位なし(テストの unit の組は除かれる) | `include_tests := true` → 1 位(共有 8、8 / 8) |
| 循環 1 | `judge/*`・`policy.rs`・`tools/*` | 1(40) | 1(40) | **1**(25) | — |

太字は今も上位 10 件にあるもので、**13 件のうち 7 件**である。外れた 6 件は、5 件が `attribution := 'all'`、1 件が `include_tests := true` で戻る。

- **2 つの既知の件は、同じ実験で確かめられた。** `AgentdState` impl は `attribution := 'all'` で 5 位に戻り、そのとき同じ原因の型は 3 位(750)になる(既定では型が 2 位)。`AgentdHandle` と `TerminaldHandle` は、`contain` では側が 2 つの impl に解決し、最も内側への帰属で impl には自分の行の変更しか付かないので 1 / 0 になる。`attribution := 'all'` で戻り、`overlap` では戻らないことが、この説明と合う。ただし今の順位は 39 位で、依頼文と継承の記録 4 節の 41 位と 2 つ違う。継承の記録の後に `duckdb-ext/` の unit と履歴が増えたことのほかに理由があるかは、切り分けていない。
- **M1 と前置きの帰属は、変数では分けられない。** README の Parameters の表のとおり `attribution` は `innermost` と `all` の 2 値で、前置きの帰属は `innermost` の一部である。どちらの規則が外したかは、8・9 個目の増分の記録(数え方の記録 2 節、帰属の記録 2 節)の順位で書いた。
- **継承の規則には戻す変数が無い。** 13 件のうち継承の規則で数が変わったものは、継承の記録 4 節によればクローンの組では無い(`AgentdHandle` と `TerminaldHandle` は `contain` で impl が側なので変わらない、と同節が書く)。
- **5 つの変数をすべて戻すと、13 件すべてが 2 回目の上位に戻る**(`AgentdHandle` と `TerminaldHandle` は 3 位ではなく 4 位で、片側は 25 / 0、両側 4)。

### 原因ごとに、どの信号が届くか

2 回目 8 節の 10 個の原因(と `expect_finished`)について、原因の unit(原因が書かれている unit)が、各信号のどの順位にあるかを並べた。順位は既定の数え方の全体での最高の順位で、上位 10 件の外も数える。テストの unit は churn のテストでない一覧に入らないので、全体の順位を括弧で書いた。co-change と循環は、その unit を含む組や循環があるかどうかで、その組や循環が原因を指しているとは限らない(読んだのは上位の候補だけである)。

| 原因 | 原因の unit | churn | fan-in × churn | クローン | co-change | 循環 |
| --- | --- | --- | --- | --- | --- | --- |
| 1. 4 本の送信路 | `TerminalSession.spawn`、`WorkspaceShell` 型、`WorkspaceShell.new` | 4、5、10 | 8 | 1 | 480 位(`execute` と `new`、結合 50%) | `src/workspace/mod.rs` が 2 モジュールの循環(22 位)に入る。原因の辺ではない |
| 2. `reconcile` のビューの構築 | `reconcile`、`spawn_agent_resume` | 6 | 15 | 片側のある組なし | なし | なし |
| 3. `AgentdState` の 2 つの lock | `AgentdState` 型、`reload_provider_config` | 18 | 2 | なし | 329 位(`Provider.for_entry` と `reload_provider_config`) | `state.rs` が 3 モジュールの循環(8 位)に入る |
| 4. `RigAgentConfig` の 3 種類の値 | `RigAgentConfig` 型 | 25 | 6 | なし | 1,026 位(`RawConfig` と) | `config.rs` の循環(5 位)の要素。判定した辺は別 |
| 5. `commands.rs` のボードの処理 | `commands.rs:143-447` の 7 つの method | 332(`open_board_task_session`、6) | 1,464 | 23 位(`open_board_organizer` と `spawn_agent_lookup_attach`、1 / 1) | なし | なし |
| 6. `AgentdHandle` と `TerminaldHandle` の同じ操作 | 2 つの impl と `begin_reload`・`stop_and_wait`・`register_*` | 353(`register_agent`、6) | 316(`AgentdHandle` impl) | 39 | 92 位(`register_agent` と `AgentRoutes.send_agent`、83%) | なし |
| 7. `start_approval_gate` | 同名の関数 | 464 | 1,117 | なし | なし | **1** |
| 8. 継続の 3 つの写し | `handle_continuation_check`、`handle_truncation_recovery`、`handle_memory_checkpoint` | 196 | 810 | 46・47 | なし(テストを含めても 3,378 位) | `continuation.rs` が 3 モジュールの循環(7 位)に入る。原因の辺ではない |
| 9. テスト用の描画の写し違い | `render_agent_transcript`(テスト) | (全体 19 位) | 128 | **5** | なし(テストを含めると 603 位) | なし |
| 10. real-git のテストの補助関数 | `tests.real_git`、`worktree.rs` の `tests.git` | (全体 1,051 位) | なし | 15 | なし | なし |
| `expect_finished` | 2 つの同名の関数(テスト) | (全体 292・293 位) | なし | 片側のある組なし | テストを含めると 1 位 | なし |

信号ごとに見ると、churn と fan-in × churn は、原因が 1 つの大きな unit の中や、多くの箇所から使われる型にあるもの(1〜4)に届き、原因が小さい method の組み合わせにあるもの(5〜8)には届かない。クローンは、原因が写しであるもの(1、6、8、9、10)に届くが、順位はそれぞれの unit の変更の数で決まり、6・8・10 は上位の外にある。co-change は、どの原因にも上位では届かない。循環は 7 だけに届く。

## 7. 誤報の機構の表

2 回目 6 節の表を、実装の状態の列を足して書き直した。「今の上位で説明する候補」は、今の 45 件について数える。組んだ機構は実験で数え、その規則を前の数え方に戻したとき候補の上位 10 件への有無か数が変わるものを「説明する」とした。そのうち、候補自身の数が変わるもの(数)と、戻した規則で入ってくる候補に押し出されるだけのもの(押し出し)を分けた。組んでいない機構は読んで数えた。

| 機構 | 実装の状態 | 今の上位で説明する候補 | 戻す変数、または規則の形 |
| --- | --- | --- | --- |
| **M1** 入れ子の unit が中の変更を数える、と前置きの帰属 | 組んだ(8・9 個目の増分。最も内側への帰属、前置きの doc コメント・属性、空行) | 25 件(数 6、押し出し 19)。churn 1(51 → 50)が数、6〜10 が押し出し(impl 5 つが入る)。fan-in 2(26 → 25)、3(12 → 11)、5(9 → 8、11 位)が数、7〜10 が押し出し。クローン 9(0 / 6 → 0 / 7、16 位)が数、7・8・10 が押し出し。co-change 2(共有 4 → 3、143 位)が数、4〜10 が押し出し | `attribution := 'all'`。2 つの規則は 1 つの変数で戻る |
| **M2** 名前が残り本体が移った後も履歴を持つ | 組んでいない(系譜の記録 6 節のとおり、承認された規則の外) | 0 件(読んだかぎり、今の 45 件に残り殻は無い) | 2 回目 6 節の形のまま |
| **M3** 契約の型を皆が使い、伝播をコンパイラが強制する | 組んでいない | 10 件(読み)。fan-in 1・3・4・5・7・10(`Event`、`ToolCallResult`、`Command`、`ToolCallRequest`、`MessageRole`、`CommandId`)、co-change 3〜6(`ViewKindState` と `ViewKind`、`From` の網羅的な `match`)。このほか、クローン 2・4・6 の判定も同じ網羅的な `match` を理由にしている | 相談の項目(下) |
| **M4** クローンの片側を断片ではなく囲む unit で数える | 試した(11 個目の増分の `overlap`)。10 件のうち 9 件の機構ではなかったので、既定は `contain` に戻した | 実験(`overlap`):3 件(数 1、押し出し 2)。クローン 9(側 a がテストの関数に解決し、両側がテストの組として除かれる)が数、8(11 位)・10(13 位)が押し出し。読み(断片の行の変更だけを数える 2 回目の意味):10 組とも片側の数は囲む関数の変更の数で、伝播の遅れを含んでいたのは 7 番だけ、それも断片の外の行(3 節) | `clone_sides := 'overlap'`。2 回目の意味の規則(断片の行の本文が変わったコミットだけを数える)は組んでいない |
| **M5** co-change の組にテストの unit が入る | 組んだ(8 個目) | 8 件(押し出し 8)。co-change 3〜10(44〜51 位)。戻すとテストの組(1 位の `expect_finished` など)が入る | `include_tests := true` |
| **M6** 親子のモジュールの循環、1 つの型の impl をファイルに分けた形 | 組んだ(8 個目) | 5 件(数 4、押し出し 1)。循環 1(25 → 40)、2(9 → 11)、3(7 → 8、6 位)、4(5 → 9)が数、5(3、13 位)が押し出し | `cycle_edges := 'raw'` |
| **N1** 移動・改名・分割のコミットが共有コミットになる | 組んだ(8 個目。co-change だけ。churn はリンクのコミットを数えたまま `move_commits` で示す) | 8 件(数 4、押し出し 4)。co-change 1(170 位)、7・8(254・255 位)、9(200 位)が数、4・5・6・10 が押し出し | `cochange_link_commits := true` |
| **N2** クローンの片側に、もう片側がまだ無かった頃のコミットが入る | 組んだ(8 個目) | 8 件(数 5、押し出し 3)。クローン 1(14 → 28)、3(b 0 → 2)、4(7 → 19)、6(5 → 17)、7(b 3 → 4、11 位)が数、8・9・10 が押し出し | `clone_before_both := true` |
| **N3** 改名と衛生作業の一斉の変更が多くの unit の履歴に入る | 組んでいない | 4 件(読んだ候補のうち、2 コミット以上がこれだったもの)。fan-in 7 `MessageRole`(11 のうち 4、wire の受け皿の追加と削除)、10 `CommandId`(30 のうち 4)、churn 8 `render`(34 のうち 2)、9 `spawn_rig_session`(34 のうち 2) | 相談の項目(下) |
| **境の行による所属**(11 個目) | `overlap` だけで起きる。既定の `contain` では起きない | 既定の上位では 0 件。`overlap` の上位 10 件では、入る 3 件のうち 2 件(`panic_message` の組 17 / 0 は前の関数 `fold_until_terminal` の閉じ括弧の行で、`runtime/agent.rs` と `terminal.rs` の `spawn` の組 9 / 0 のうち 5 は `Op` の最後の variant の行で入った。クローンの側の記録 2 節と同じ) | `clone_sides` の既定が `contain` である限り効かない |
| **囲む unit の改名の切れ目**(11 個目で見つけ、12 個目の継承で閉じた) | 組んだ(継承の規則)。戻す変数は無い | 実験できない。継承の記録 4 節によれば、今の上位ではクローン 2 番の数(10 / 6、17 → 11 / 8、18)とクローン 8 番の有無(2 / 0、1 → 2 / 5、2 で入った)を説明する。残りは、前の名前の頃に変更の無かった unit で、今の履歴が空の 3 個(`TerminaldHandle.stop_and_wait`、`TerminaldHandle.broadcast_terminal_color_scheme`、`horizon-board/src/event.rs` の `tests`)。`stop_and_wait` は原因 6 の断片の中の method である | なし |
| **merge が作った blob**(13・14 個目) | 組んだ(blob の層は merge を最初の親と比べる) | 0 件。比較の snapshot だけに効き、`unit_changes`・系譜・信号は merge を見ない(14 個目の増分で行の hash が変わらなかった) | `blob_include_merges := false` |
| **N4**(新)移動のコミットが churn に残る | 組んでいない(数え方の記録 1 節のとおり、churn はリンクのコミットを数え、`move_commits` で示す) | 1 件(読み)。fan-in 9 `SessionId` impl(5 コミットのうち 4 がリンク、残りは作成)。churn の 7・9 番(35 のうち 5、34 のうち 5)、2 番(49 のうち 5)にも含まれるが、それ自体が順位を決めてはいない | `unit_churn` の `commits - move_commits`(列はすでにある) |
| **N5**(新)新しいビューを試作した間のコミットが組を作る | 組んでいない | 3 件(読み)。co-change 1・7・8(共有がすべて 2026-10-11〜13 の試作の各回) | — |
| **N6**(新)第 3 の unit を通した結び付き | 組んでいない | 5 件(読み)。co-change 3〜6(`From` の impl を通して)、9(どちらも `Workspace::terminate_session` を呼ぶ入口) | 長さ 2 の参照(共通の参照元や参照先)を数える形 |
| **N7**(新)`mod x;` の宣言とそのファイルの item | 組んでいない | 1 件(読み)。co-change 10(11〜14 番も同じ出来事) | 宣言の module の unit と、そのモジュールに属する unit を組から除く |
| **N8**(新、候補の作り方)解決しない側にテストの印が付かない | クエリではなく、2 回目から記録が使ってきた候補の作り方 | 1 件。クローン 9(側 a の断片が `#[test]` の行から始まり解決しない) | 候補の作り方の問題。`clone_change_consistency` の出力に側の unit が無い |

この表の数は、上位の候補のうち、その機構が数や順位を説明するものの数で、機構どうしは重なる。重なりは次のとおりである。

- クローン 8・9・10 は N2、帰属、`overlap` のどれを戻しても動く。クローン 7 は N2 と帰属。
- co-change 4〜10 は N1、帰属、M5 のどれを戻しても動く。co-change 1 は N1 だけ、2 は帰属だけ、3 は M5 だけで動く。この 10 組は結合 100%・共有 3〜5 の同点の帯にあるので、組を増やすどの規則でも押し出される。
- 組んだ機構のどれを戻しても動かない候補は、churn 2〜5、fan-in 1・4・6、クローン 2・5 の 9 件である。

組んだ機構を戻すと上位に入ってくるもの(今の上位に無いもの)は、6 節の 13 件のほか、帰属で churn の impl 5 つと fan-in の impl 5 つ、N2 で mock provider の組・2 つの `AgentFrameItem`・2 つの reload・継続の写し(10 位)、M5 でテストを含む co-change の組、M6 で 2 回目の循環 2・3 番である。2 回目と 1 回目がこのうちの多くを `問題なし` と判定した。

### 相談の項目:M3 と N3 の印の列(組んでいない)

2 回目 6 節の表が書いた形を、印の列として書き直した。どちらも順位を変えず、候補の横に印を出すだけの形である。組むかどうか、どのクエリに付けるかは相談の項目で、今回は組んでいない。

- **M3 の印**:型の unit について、そのコミットのうち、その型に辺を張る unit(`unit_edges` の in)も同じコミットで変わったものの割合を出す(`edge_cochange` が辺ごとの共有コミットを持つ)。割合が高く fan-in も高い型を「参照を通って伝播する契約」として印を付ける。網羅的な `match` を確かめるなら、`read_ast` で match の腕がその enum の variant の名前を並べている箇所を数える。今回の候補では、fan-in の 6 件と co-change の 3〜6 番がこの印の対象になる。
- **N3 の印**:コミットごとに、変えた unit の数と、unit あたりの変えた行の少なさを出し、多くの unit を少しずつ変えたコミットに印を付ける。`max_commit_files` 50 はファイルの数で切るだけで、それより小さい一斉の変更は残る。本当に横断する変更(wire の方針の変更など)と区別するには、件名や中身を読む必要がある。

## 8. 信号ごとの集計

| 信号 | 問題あり | 問題なし | 判断できない | 入った候補のうち 問題あり |
| --- | --- | --- | --- | --- |
| `unit_churn`(テストでない 10 件) | 5(4、5、6、8、10) | 5 | 0 | 3 / 5(6・10 番は 1 回目の判定、8 番は新しい原因) |
| `unit_fan_in_churn`(積の 10 件) | 3(2、6、8) | 7 | 0 | 1 / 5(8 番、2 回目の判定) |
| `clone_change_consistency`(10 組) | 4(1、5、7、10) | 6 | 0 | 2 / 4(7・10 番、新しい原因) |
| `cochange_without_reference`(10 組) | 0 | 10 | 0 | 0 / 10 |
| `module_cycles`(5 個) | 2(1、5) | 2 | 1(2) | 1 / 2(5 番、新しい原因) |

「入った候補」は 2 回目の上位に無かった候補で、1 回目の上位にあったもの(churn の `reconcile`・`TerminalSession.spawn`、fan-in の `ToolCallRequest`)を含む。合わせて 45 件のうち、`問題あり` 14、`問題なし` 30、`判断できない` 1 である(2 回目 13 / 31 / 1、1 回目 12 / 32 / 1)。信号をまたいで重複を除くと、`問題あり` の原因は 9 個だった。2 回目の 10 個のうち 6 個と、新しい 3 個である。新しい原因のうち 1 つ(リストのモーダルの写し)は、churn とクローンの 2 つの信号が別々の unit から指した。

信号ごとの変化を書く。

- **`unit_churn`**:impl が 5 つ外れ、method が入った。入った 5 件のうち 3 件が `問題あり` で、2 件は前の回に判定済みの原因(`reconcile`、`spawn`)を method 自身で指した。impl から method への入れ替わりは、数え方の記録 2 節と帰属の記録 2 節が見込んだとおりで、今回、新しい原因(`render` のモーダルの写し)も 1 つ見つかった。残る `問題なし` の 5 件は、組み立ての場所(`run_session`、`spawn_rig_session`、`run_provider_stream`)、データの表(`build_definitions`)、設計が決めた dispatcher(`execute`)で、どれも機能を足すたびに 1 行ずつ増える場所だった。
- **`unit_fan_in_churn`**:impl が 5 つ外れ、型が入った。上位 10 件のうち 6 件が契約の型(M3)、1 件が移動だけでできた積(N4)だった。`問題あり` の 3 件は、どれも前の回の原因である。
- **`clone_change_consistency`**:新しく入った 4 組のうち 2 組で写しの費用が見えた。7 番は片側の数が伝播の遅れを実際に含んでいた初めての例で、10 番は本番とテストの SQL の写しである。9 番は候補の作り方(N8)で入った見かけの一致だった。
- **`cochange_without_reference`**:2 回目の 10 組はすべて出て、入った 10 組もすべて `問題なし` だった。誤報の形は、試作の間の一斉の描き直し(N5)、永続化の写しの enum と `From`(M3・N6)、境界の写し(1 回目と同じ)、2 つの入口が同じモデルの操作を呼ぶ形(N6)、`mod x;` の宣言(N7)である。上位 10 組は同点の帯にあり、規則を 1 つ戻すだけで大半が入れ替わる。
- **`module_cycles`**:構造上の辺をまとめた後の 5 個のうち、新しく入った 2 個の 1 つで、置き場所を外れた 1 つの関数が循環を閉じる形(`config.rs`)を見つけた。judge の循環と同じ形である。

## 9. オーナーに渡す候補の更新

2 回目 8 節の 10 個と `expect_finished` を、今回の上位に出たかどうかで分けて並べ直した。ソースが変わっていないので、前の判定はどれも有効である。

**今回も上位に出たもの(6 個)。**

1. **`TerminalSession.spawn` と `WorkspaceShell` の 4 本の送信路**(`src/terminal/session.rs:206-214`、`src/workspace/mod.rs:383-402`、421-424、450-453、465-468):churn 4・5・10 番、fan-in 8 番、クローン 1 番の 5 行が指した。
2. **`WorkspaceShell.reconcile` の中のビューの構築**(`session_lifecycle.rs:313-416`):churn 6 番。今回は method 自身が指した。
3. **`AgentdState` の 2 つの lock**(`horizon-agentd/src/session/state.rs:42`、46、183-193、`session/run.rs:79-112`):fan-in 2 番(型)。
4. **`RigAgentConfig` の 3 種類の値**(`horizon-agent/src/config.rs:519-610`):fan-in 6 番。
5. **judge と tools の循環を閉じる `start_approval_gate`**(`horizon-agent/src/judge/mod.rs:301-345`):循環 1 番。
6. **テスト用の描画で監査の事象が「memory: seeded」になる写し間違い**(`frame/test_support.rs:156-166`):クローン 5 番。

**今回の上位から外れたもの(5 個)。**

7. **`src/workspace/commands.rs` の中のボードのセッションの処理**(`commands.rs:136-448`):impl が churn の 2,429 位になった。原因の method の最高は 332 位(`open_board_task_session`、6 コミット)である。原因が impl の中身の組み合わせにあり、method の数では指せない(数え方の記録 2 節)。
8. **`AgentdHandle` と `TerminaldHandle` の同じ操作**(`src/runtime/mod.rs:472-486` と 659-673、335-341 と 569-575):クローンの 39 位。
9. **自動継続のターンの始め方の 3 つの写し**(`rig/session/continuation.rs:287-303`、`rig/session/turn.rs:325-343`・387-405):クローンの 46・47 位(1 / 0)。N2 を戻すと 10 位(1 / 8)に入る。
10. **real-git のテストの補助関数の写しで、git の大域設定から切り離す修正が条件の内側に入った**(`horizon-agentd/src/session/setup.rs:338-346` と `worktree.rs:434-447`):クローンの 15 位。
11. **2 つの `expect_finished` の写し**(`tools/bash/tests.rs:25`、`tools/tests.rs:145`):テストの組なので co-change に順位が無い。

**新しいもの(3 個)。**

12. **4 つのリストのモーダルの写し**(`src/workspace/render.rs:1112-1287` の 4 つの重ね描き、`src/workspace/modals.rs` の 4 組の開く・閉じる・取り消す):churn 8 番とクローン 7 番。修正がモーダルごとに後から写された跡がある(1c27f1ab、0a921404 → d39ea089)。
13. **テスト専用の `Store.append_event` による本番の INSERT の写し**(`duckdb/append.rs:112-174` と 182-249):クローン 10 番。費用はテストの中。
14. **設定の型のモデル一覧の取得**(`horizon-agent/src/config.rs:414-431`):循環 5 番。置き場所を外れた 1 つの関数。

## 10. オーナーの抜き取り用の一覧

数分で確かめられるものを、`問題あり` と `問題なし` を混ぜ、`判断できない` をすべて含めて選んだ。

1. **`src/workspace/render.rs:1112-1287` — 問題あり。** 4 つのモーダルの重ね描きが同じ形の約 40 行ずつで、1c27f1ab が同じ修正(`stop_propagation` と同じ注釈)を 4 箇所に当てた。
2. **`src/workspace/modals.rs:347-425` と 516-572(`open_session_manager`・`open_palette` とその閉じる・取り消す)— 問題あり(1 と同じ原因)。** 0a921404 がパレットだけを直し、翌日の d39ea089 が同じ直しを他の 2 つに当てた。
3. **`crates/horizon-agent/src/persistence/projection/duckdb/append.rs:112-174` と 182-249 — 問題あり(小)。** テスト専用の書き込みが本番の 10 列の INSERT を写し、49 箇所の呼び出しがこちらを通る(本番の経路を直接呼ぶテストは 7 箇所)。境界の判定で、テストの補助関数の写しとして `問題なし` と読む余地もある。
4. **`crates/horizon-agent/src/config.rs:414-431` — 問題あり(小)。** 設定の型の method がネットワークでモデル一覧を取り、`config.rs` と rig の adapter の循環を閉じている。`resolved` と同じ解決の順を使うためにここに置いた意図なら `問題なし` になる。
5. **`crates/horizon-agent/src/providers/rig/completion.rs:445-509` — 問題なし。** 1 ターンのストリームの 64 行で、35 コミットの大半は OpenAI 専用だった前身の頃の、機能を足すたびの変更である。
6. **`crates/horizon-wire/src/session.rs:26-38` — 問題なし。** fan-in 78 × 5 コミットの 5 は、作成と 4 回の移動で、本体の変更は無い。
7. **`crates/horizon-workspace/src/persistence.rs:150-170`、534-552 — 問題なし。** `ViewKindState` は `ViewKind` の永続化の写しで、型の文書がそう決め、`From` の網羅的な `match` がコンパイラに伝播を強制する。
8. **`crates/horizon-agentd/src/session/board.rs:105` と `session/connection.rs:113-119`(循環 2 番)— 判断できない。** 次の節の問い。

## 11. 未決の問い

1 回目 5 節の 4 番と 2 回目 9 節の問いは残っている。`horizon-agentd` のボードの入口、つまりボードのツールの host の `board_flow::operate` への委譲(`session/board.rs:105` の `BoardHost for AgentdBoardHost.operate`)と、ACP の `Connection::ensure_board_organizer` の `board_flow::organizer_session` の呼び出し(`session/connection.rs:113-119`)を `session/` に置くのか、`board_flow/` をボードの唯一の置き場とするのか、である。

- **入口は `session/` に置く、が意図の場合。** 2 本の辺は入口から機能への委譲で、状態を戻す依存ではない。循環 2 番(9 モジュール)は問題が無く、`module_cycles` はこの 9 モジュールの循環を出し続ける(誤報として残る)。
- **`board_flow/` が唯一の置き場、が意図の場合。** 2 本の辺は層の逆転で、2 つの入口を `board_flow/` に移すことがリファクタリングになる。今の辺の表から 2 本を除いて数え直すと、`board_flow/operations.rs`、`session/board.rs`、`session/connection.rs` が循環から外れ、`session/` の中の 6 モジュール(`spawn.rs`、`run.rs`、`environment.rs`、`exploration.rs`、`panic.rs`、`resume.rs`)の循環が残る(`spawn` → `run` → `environment` → `exploration` → `spawn` と、`run` → `panic` → `resume` → `spawn`)。この 6 モジュールの循環は判定していない。この数え直しは辺の表を手で追ったもので、SQL で確かめてはいない。

今回の判定では、ほかにオーナーの意図が要るものは出なかった。境界の判定(10 節の 3・4)は、読み取りと違う意図があれば判定が変わるものとして、抜き取りの一覧に入れた。

## 12. 記録や README と食い違った点、気づいた点

- **`AgentdHandle` と `TerminaldHandle` の順位。** 今の既定で 39 位(1 / 0、両側 1)で、継承の記録 4 節と依頼文の 41 位と違う。数は同じである。継承の記録の後に増えた `duckdb-ext/` の unit と履歴が順位に効いたかどうかは調べていない。
- **5 つの変数をすべて戻した数。** 2 回目の 45 件の顔ぶれと並びは再現するが、`cochange_without_reference` は 8,738 組(2 回目 8,424)、クローンの `Store.project_event` と `fold_until_terminal` の片側 a は 17(2 回目 14)など、数が違う。継承の規則に戻す変数が無いためである。数え方の記録 1 節の「5 つを与えると 2 回目の記録の数がすべて再現する」は、継承の規則が入った後は成り立たない。
- **`cochange_without_reference` の組の数。** 既定で 1,255 組で、継承の記録 4 節の 1,254 組と 1 つ違う。`duckdb-ext/` の unit を含む組が 1 つある。
- **`run_session` の 51。** 帰属の記録 2 節の「後」と同じで、`attribution := 'all'` で 50 になる。前置きの帰属で 1 コミット増えた。
- **クエリの欠陥は見つからなかった。** クローン 9 番が両側がテストの組を除く手順を通り抜けたのは、候補の作り方(解決しない側をテストとしない)によるもので、クエリの欠陥ではない。ただし `clone_change_consistency` は、側が解決しない場合に、その断片がテストのモジュールの中にあるかどうかを返さないので、候補の作り方でそれを補う手段が無い。
- **churn の表のテストの unit。** `render_agent_transcript` は全体の churn で 19 位(26 コミット)だが、テストの unit なのでテストでない一覧には入らない。原因 9 に churn が届くかどうかは、テストを一覧に入れるかどうかで変わる。

## 13. 限界

- **読んだのは 1 人である。** 判定は Opus 1 体が 1 回読んだもので、オーナーの意図を知らない。前の判定を引いた候補は、前の判定の誤りをそのまま引き継ぐ。オーナーの決定により、2 人目の読み手による一致率は取っていない。
- **信号の根拠は弱い。** 1 つのリポジトリの上位 10 件ずつ(循環は 5 個)の集計である。
- **規則は 1 つずつ戻した。** 機構の重なりは、1 つずつ戻した結果を並べて書いたもので、2 つを同時に戻したのは `overlap` と帰属の組だけである。M1 と前置きの帰属は 1 つの変数なので、分けた効き方は 8・9 個目の記録の数に頼った。継承の規則は切り替えられないので、その効き方は継承の記録の数に頼った。
- **「押し出し」の数え方。** 規則を戻して候補が上位から外れても、候補自身の数が変わらなければ「押し出し」とした。co-change の上位 10 組は同点の帯にあるので、押し出しの数は帯の幅に大きく左右される。
- **循環の 5 番目。** 3 モジュールの循環が 8 個あり、名前の順で選んだ。辺の数で選べば `continuation.rs` の循環(辺 5)になり、それは判定していない。
- **新しい機構 N4〜N8。** どれも今回読んだ候補から名指したもので、SQL で数えてはいない。N6 の規則の形(長さ 2 の参照)が誤報だけを除くかは分からない。
- **判定の確かめ方。** `render` と `modals.rs` の写しの費用はコミットの差分と文書から、`append_event` は呼び出しの数と差分から、`config.rs` の循環は辺の表とコードから導いた。どれもテストを走らせて確かめてはいない。
- **作業ツリー。** 測定は b3b94fc1 の複製で行い、索引・jscpd の報告・キャッシュ・書き出しはその木から作った(`target/code-db/` の下、コミットしていない)。
