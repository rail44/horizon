# unit の系譜をビューにした記録(2026-10-26)

コードデータベースの SQL 層の 7 つ目の増分である。今の各 unit を、ファイルの改名、unit の改名と移動、名前を残したまま本体が移った場合をまたいで、前身の unit につなぎ、履歴に基づく信号が名前ではなくコードを追うようにした。SQL と `check.py`、`run.sh` だけを変え、Rust は変えていない。観察だけを記し、推奨は書かない。

前提は [設計文書](../code-database-design.md) の「Open points」の最初の項目(unit の同一性は改名で新しい履歴を始め、名前を残して本体が移ると古い履歴を残す)と、2 つの dogfood の記録 [後ろ向き](code-db-dogfood-retrospective-2026-10-26.md)・[前向き](code-db-dogfood-prospective-2026-10-26.md) が当たった同一性の誤りである。後ろ向きの記録では、旧監査の台帳のうち判定時点の名前で書かれた項目が基準の木に対応しなかった。前向きの記録では、`crates/horizon-wire/src/session.rs` の `SessionId`(fan-in 107、コミット 1。新しいクレートに切り出されたため)、`tools/catalog.rs` の `definitions`(3 行、42 コミット。cf9a2daa で本体が `build_definitions` に移った)、`duckdb/mod.rs` の `mod tests;`(1 行、17 コミット。2f4105f6 でテストが `tests.rs` に移った)が誤った候補になり、fan-in × churn の上位 10 件のうち 6 件がコミット 1 だった。

## オーナーの判断

依頼文によれば、導入の前に依頼側が判断点を示し、オーナーは「理解できました。導入するにあたって判断点は残っていますっけ?」と尋ね、示された 2 つの推奨に「どちらも推奨でよいです」と答えた(2026-10-26)。2 つは次のとおりである。

1. **履歴は名前ではなく本体を追う。** unit が名前を残したまま、同じコミットで本体を別の unit に移したとき、古い履歴は本体を受け取った unit が引き継ぐ。名前を残した側(以下「残り殻」)は、そのコミットから新しい系譜を始める。
2. **系譜は多対多で、churn はすべての祖先にわたる異なるコミットを数える。** 分割では両方の後継が前身の履歴を受け取り、統合では後継が和を受け取る。数は `count(DISTINCT sha)` なので二重には数えず、多重度は列で見えるようにする。

このほかの決め方(7 節)は、依頼文が定めていない点を作業側で決めたものである。

作業は worker の作業ツリーで行った。作業ツリーのブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 14b60400 に早送りした(`git merge --ff-only 14b60400`、変更の無い状態で実行。`origin/main` の先端はその祖先だった)。測定はこのリポジトリの 14b60400 の使い捨ての複製 2 つ(`target/code-db/clone`、`target/code-db/clone2`)で行い、作業ツリーのファイルと履歴は測定に使っていない。機械は前回までと同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している(作業中の load average は 10〜20、スワップを 10 GB 使っていた)。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0、sitting_duck は v1.16.0、rust-analyzer は 1.99.1、jscpd は旧監査の固定版 5.3.1 である。

## 主要な観察

- **類似度は行の集合の Jaccard 係数にした。** 同じ候補の組で比べると、閾値 0.5 で、名前の最後の部分が違う unit の間の `content_match` は、行の集合で 598 本、トークンの多重集合で 9,186 本だった。既知の 2 例(`SessionId`、`definitions` → `build_definitions`)はどちらの尺度でも閾値 0.5 を越えた(2 節)。
- **閾値 0.5 で、リンクは `same_name` 14,044、`file_rename` 3,293、`content_match` 3,506、`body_move` 127 本だった。** `ambiguous` は `content_match` の 18.5 %、`body_move` の 11.8 % である。閾値 0.7 では `content_match` 2,789(`ambiguous` 4.6 %)、`body_move` 61(3.3 %)になった(3 節)。
- **今の unit 9,264 個のうち 3,245 個でコミットの数が変わった。** 増えたのが 3,187 個、減ったのが 58 個である。増えた unit の増分の中央値は 1、90 パーセンタイルは 5、最大は 47 だった。系譜に祖先を持つ unit は 3,203 個、分割で履歴を分け合う unit は 127 個である(4 節)。
- **fan-in の順で並べた上位 10 件のうちコミット 1 の行は、6 件から 2 件になった。** fan-in × コミットの順(このクエリの新しい並び)では、前後とも上位 10 件にコミット 1 の行は無い(5 節)。
- **既知の 3 例のうち、`SessionId` と `definitions` / `build_definitions` は解けた。`mod tests;` は解けなかった。** `definitions` は 42 → 1、`build_definitions` は 2 → 49、`SessionId` の impl は 1 → 5、型は 1 → 2 になった。`duckdb/mod.rs` の `mod tests` は 17 → 27 で、本体が 1 つの unit ではなくファイル `tests.rs` に移ったため、依頼文の `body_move` の規則では切れない(6 節)。
- **旧監査の台帳の、基準の木に対応しなかった名前のうち、系譜をたどって基準の時点の unit に届いたのは 2 件(`tokenize`、`paint_terminal`)だった。** 残りの 6 件は基準の後に新しく書かれた関数で、HEAD では名前のまま今の unit にある(6 節)。
- **更新の時間と大きさは 8 節に書いた。**

## 1. 組んだもの

| ファイル | 変更 |
| --- | --- |
| `views/lineage.sql`(新規、`views/unit_changes.sql` の後) | `commit_order`、`lineage_sources`、`derived_unit_successions`、`unit_successions`、`unit_lineage_segments`、`unit_lineage`、`unit_history`、`unit_lineage_counts` |
| `views/unit_changes.sql` | `unit_change_sides`(両側のすべての unit とその行、変更の種類)を足し、`unit_changes` をその射影にした(行は前と同じ) |
| `refresh.sql` | 系譜の層(`cache.commit_order`、`cache.unit_successions`、`cache.lineage_derived`)、スキーマ版 4、`meta` の 4 列と `seconds.lineage` |
| `cache.sql` | `commit_order` と `unit_successions` の写し |
| `queries/` | `unit_churn`(列 `commits_by_name`、`ancestors`、`successors_at_split`)、`unit_fan_in_churn`(列 `fan_in_x_commits` とその順)、`clone_change_consistency`、`unit_dependencies`、`unit_cochange` の注釈が `unit_history` を読む |
| `views/graph.sql` | `unit_commits` が `unit_history` を読む(`unit_cochange`、`edge_cochange`、`cochange_without_reference` はその上) |
| `retro/signals.sql` | `fan_in` の信号を fan-in の順で並べ直す(クエリの並びが積の順になったため) |
| `run.sh`、`check.py` | 読む順に `views/lineage.sql` を足した。系譜の fixture、更新の検査・測定の項目 |
| `README.md` | Lineage の節、読む順、配置、パラメータ、キャッシュ、検査 |

### 継承の関係(`unit_successions`)

同じコミットの中で、前身の同一性(コミットの前の側)と後継の同一性(後の側)を結ぶ。同一性は `unit_changes` と同じ `(path, kind, qualified_name)` である。`link` は次の 4 つである。

- `same_name`:その場で変更された unit。前の側と後の側の類似度を `similarity` に入れる。同一性の自分のコミットはもともと履歴なので、系譜をたどるときには使わない。
- `file_rename`:ファイルが改名され(`file_changes.old_path` → `path`、gix の改名検出、類似度 50 %)、種類と限定名が両側で同じ unit。変更されたかどうかによらずファイルのすべての unit に引く(変更されていない unit の `similarity` は 1)。純粋な改名(hunk も blob の行も無い)では、旧パスの直前の変更の新しい側の unit を使う。
- `content_match`:同じコミットの `removed` の unit と `added` の unit で、本文が似ているもの。
- `body_move`:`modified` の unit で、前の側が後の側に似ておらず、同じコミットの `added` の unit が前の側に似ているもの。前の側から追加された unit に引き、名前を残した側(残り殻)には前身を与えない(`same_name` も `file_rename` も引かない)。

類似度の閾値は `lineage_threshold`(既定 0.5、gix の改名の既定と同じ)で、`body_move` の「似ていない」も同じ閾値の未満とした。`content_match` と `body_move` は、同じ種類の unit どうしか、関数とメソッドの組(impl の中へ、または外への移動で種類が変わる)だけを比べ、閾値以上の候補にはすべて引く(多対多)。`ambiguous` は、前身か後継がそのコミットで複数のリンクを持つか、どちらかの側の上位 2 つの候補の差が 0.1 未満のリンクに付く。modified の unit の前の側にとっては、自分の後の側も候補の 1 つとして数える。

### 系譜と履歴

`unit_lineage_segments` は、今の各 unit から継承の関係(`same_name` 以外)を再帰でさかのぼる。深さ 0 は unit 自身である。コミット c のリンクでたどり着いた同一性は、c より前の自分のコミットを渡す。`body_move` で系譜が切れた同一性は、切れたコミットから後の自分のコミットだけを渡す(`from_sha`)。位置は `commit_order`(git log の順を逆にした番号、1 が最も古い。`git_commits` は 1 本のスレッドからその順に行を出す)で比べる。

`unit_history(unit_id, sha, change_kind, depth)` は、系譜のどれかの同一性に触れた `unit_changes` のコミットを重複なしで並べる。`unit_lineage_counts` は、自分以外の同一性の数(`ancestors`)と、系譜のリンクの前身がそのコミットで持つ後継の数の最大(`successors_at_split`、分割が無ければ 1)である。

### ライブで読んだときの費用と、`unit_changes` の形を変えたこと

最初の形では、継承の関係の導出が `unit_changes` と `blob_units` を別々に読み、ライブではどちらの読みも blob を全部構文解析し直した。ライブの `derived_unit_successions` は 18.2 s、17.7 GB、`unit_churn` は 41.2 s、43.8 GB(依頼前の `unit_churn` は 14〜20 s、8〜9 GB)になった。`unit_changes` の文を `unit_change_sides`(両側のすべての unit と行、変更の種類)として書き直し、`unit_changes` をその射影にし、導出は `unit_change_sides` だけを読むようにした。あわせて `unit_churn` は、`unit_history` と `unit_lineage_counts` を同じ文の中で、系譜と `unit_changes` を 1 回ずつ読む形で書き直した(ビューを 2 つ読むと、それぞれがもう 1 度導出を読む)。その後のライブの値は、`derived_unit_successions` が 13.6 s、8.4 GB、`unit_churn` が 18.0 s、17.2 GB である。`unit_changes` の行は前と同じで(このリポジトリで依頼前の SQL の 40,924 行と行ごとに一致、fixture の行の検査、`refresh` の比較、`commits_by_name` が依頼前の `unit_churn` のコミット数と 9,264 行すべてで一致)、どちらのキャッシュの上でも、この差は効かない。

## 2. 類似度の尺度

依頼文は、正規化した本文の行の集合とトークンの多重集合の両方を測り、既知の例をより少ない誤ったリンクで解く方を残すよう求めていた。試作では、同じ候補の組(同じコミットの `removed` の前の側 × `added` の後の側、関数とメソッドを同じ種類とみなす)に、次の 2 つを計算した。

- 行の集合:各行を `normalize_ws` し、空行を落とした集合。
- トークンの多重集合:各行の `[A-Za-z0-9_]+` と記号 1 文字ずつ(`[^\sA-Za-z0-9_]`)の多重集合。Jaccard は共通部分を各トークンの個数の小さい方の和、和集合を両方の個数の和から共通部分を引いたものとした。

| | 行の集合 0.5 | 行の集合 0.7 | トークン 0.5 | トークン 0.7 |
| --- | ---: | ---: | ---: | ---: |
| 閾値以上の組 | 3,506 | 2,789 | 12,413 | 4,978 |
| うち名前の最後の部分が同じ | 2,908 | 2,617 | 3,227 | 3,009 |
| うち名前の最後の部分が違う | 598 | 172 | 9,186 | 1,969 |
| 名前の最後の部分と種類が同じ組のうちリンクされたもの | 2,905 / 3,631 | 2,617 / 3,631 | 3,198 / 3,732 | 2,992 / 3,732 |

既知の例では、`definitions` の前の側と `build_definitions`(cf9a2daa)は行の集合で 0.603、トークンで 0.516、`SessionId` の 6 つの unit(1bca9054、`crates/horizon-agent/src/contract.rs` から)はどちらでも 1.0 だった。どちらの尺度でも 0.5 で解けるが、0.7 ではどちらも `definitions` を解かない。トークンの多重集合は、記号と予約語が共通するだけの別の関数どうしでも 0.5 を越えやすく、名前の違う組が 15 倍あった。名前の違う組のすべてが誤りではない(本当の改名もある)が、行の集合で 0.5 から 0.6 の名前の違う組を抜き出して読むと、同じひな形から書いたテスト関数どうし(`tests.reconstruct_line_diff_handles_a_pure_delete` と `…_mixed_change` など)が多く、その多くに `ambiguous` が付いていた。無作為に抜き出した名前の違う組 40 本のうち、0.7 以上の 12 本は、名前から見て多くが改名だった(`AgentToolPermission` → `ToolPermission`、`read_agent_event_log` → `read`、`AgentdProcess` → `SessiondProcess` など)。`control_input_state` → `command_palette_state`、`read` → `read_text` のように名前だけでは判断できない組もあり、本文は読んでいない。行の集合を残した。

この比較の値は、実装の前の試作(同じ候補の組を作る別の SQL)のもので、行の集合の側は実装の `unit_successions` の `content_match` の数(3,506、うち名前の違う組 598)と一致した。

## 3. リンクの数

| 閾値 | リンク | 本数 | `ambiguous` | 割合 | 名前の最後の部分が違う |
| --- | --- | ---: | ---: | ---: | ---: |
| 0.5 | `same_name` | 14,044 | 0 | | |
| 0.5 | `file_rename` | 3,293 | 0 | | |
| 0.5 | `content_match` | 3,506 | 650 | 18.5 % | 598 |
| 0.5 | `body_move` | 127 | 15 | 11.8 % | 107 |
| 0.7 | `same_name` | 14,110 | 0 | | |
| 0.7 | `file_rename` | 3,292 | 0 | | |
| 0.7 | `content_match` | 2,789 | 128 | 4.6 % | 172 |
| 0.7 | `body_move` | 61 | 2 | 3.3 % | 45 |

0.7 で `same_name` が 66 本増え、`body_move` が 66 本減ったのは、0.5 では `body_move` だった modified の unit が、0.7 では受け手との類似度が足りず、その場の変更に戻ったためである。`file_rename` が 1 本減ったのは逆の向きで、6f464e42 で改名された `crates/horizon-board/src/wire.rs` の `tests` の前後の類似度 0.583 が、0.7 では「似ていない」になり、移動と見なされたためである(「似ていない」の基準も同じ閾値なので、閾値を上げると移動と見なされる unit も出る)。`body_move` の種類の内訳(0.5)は、関数 → 関数 69、メソッド → メソッド 34、モジュール → モジュール 10、型 5、impl 4、メソッド → 関数 4、関数 → メソッド 1 だった。モジュールの 10 本は、インラインのテストモジュールが別のファイルのテストモジュールにまとめて移った場合で(`src/terminal/session.rs` の `tests` → `src/terminal/scrollback.rs` の `tests`、0.825 など)、残り殻の `mod tests` の履歴はそのコミットで切れた(`src/terminal/session.rs` の `tests` は 20 → 1)。

## 4. コミットの数の変化

`unit_churn` の `commits`(系譜)と `commits_by_name`(依頼前の数)の比較。今の unit 9,264 個のうち 3,245 個で違った。

| 変化 | unit |
| --- | ---: |
| 減った(-41〜-1) | 58 |
| +1 | 1,654 |
| +2 | 625 |
| +3〜5 | 692 |
| +6〜10 | 163 |
| +11〜20 | 44 |
| +21〜47 | 9 |

増えた 3,187 個の増分は、四分位で 1 / 1 / 3、90 パーセンタイル 5、99 パーセンタイル 13、平均 2.35 だった。減った 58 個は残り殻で、最大は `definitions`(42 → 1)、次が `src/terminal/session.rs` の `tests`(20 → 1)と `ToolSessionState.with_root`(20 → 1、本体が `ToolSessionBuilder.with_root` に移り、そちらは 1 → 22)だった。増えた側の上位は `build_definitions`(2 → 49)、`run_provider_stream`(5 → 35)、`run_session`(21 → 50)、`Workspace` の impl(16 → 43)、`crates/horizon-agent/src/hosting.rs` の `tests`(2 → 29)、`spawn_rig_session`(12 → 34)、`paint_terminal`(1 → 23)である。

## 5. fan-in × churn の上位 10 件

依頼前の `unit_fan_in_churn` は fan-in の順で並び、上位 10 件のうち 6 件がコミット 1 だった(`SessionId` の型と impl、`SessionId.new`、`ToolCallId`、`OccurrenceId`、`ToolCallIdentity`)。同じ fan-in の順で、系譜のコミット数にすると、コミット 1 は 2 件(`OccurrenceId`、`ToolCallIdentity`)になった。`SessionId` の型は 2、impl は 5、`SessionId.new` は 2、`ToolCallId` は 3 である。

クエリは fan-in × コミット(`fan_in_x_commits`)の順に変えた。この順の上位 10 件は次のとおりで、前(依頼前の同一性で積の順に並べたもの)も後も、コミット 1 の行は無い。

| 順位 | 前:unit(fan-in、コミット) | 後:unit(fan-in、コミット) |
| ---: | --- | --- |
| 1 | `contract.rs` `Event`(89、21) | `contract.rs` `Event`(89、24) |
| 2 | `tools/state.rs` `ToolSessionState` impl(31、37) | `tools/state.rs` `ToolSessionState` impl(31、39) |
| 3 | `contract.rs` `Command`(41、15) | `session/state.rs` `AgentdState` 型(30、25) |
| 4 | `live.rs` `LiveState` impl(34、17) | `live.rs` `LiveState` impl(34、22) |
| 5 | `config.rs` `RigAgentConfig`(25、18) | `session/state.rs` `AgentdState` impl(21、34) |
| 6 | `contract.rs` `ToolCallRequest`(64、6) | `contract.rs` `Command`(41、17) |
| 7 | `session/state.rs` `AgentdState` impl(21、17) | `contract/tool_result.rs` `ToolCallResult`(59、11) |
| 8 | `session_lifecycle.rs` `WorkspaceShell` impl(8、44) | `horizon-workspace/src/operations.rs` `Workspace` impl(14、43) |
| 9 | `event_log/writer.rs` `WriterHandle` impl(27、13) | `event_log/writer.rs` `WriterHandle` impl(27、22) |
| 10 | `tools/catalog.rs` `definitions`(8、42) | `config.rs` `RigAgentConfig`(25、23) |

前向きの記録で「別々の unit の値を掛けている」とされた `definitions` は、後の上位から外れた(fan-in 8、コミット 1)。

## 6. 既知の例

| 例 | 前 | 後 | 解けたか |
| --- | --- | --- | --- |
| `crates/horizon-wire/src/session.rs` `SessionId`(型) | 1 | 2 | 解けた。1bca9054 の `content_match`(`crates/horizon-agent/src/contract.rs` から、1.0) |
| 同 `SessionId`(impl) | 1 | 5 | 解けた。祖先 4 つ:`contract.rs`(1bca9054)、`src/session/mod.rs`(b11f2d91、`body_move`)、`src/workspace/types.rs`(e899e99d)、`src/workspace.rs`(4d43b283) |
| 同 `SessionId.new` | 1 | 2 | 解けた(`contract.rs` から) |
| `tools/catalog.rs` `definitions` | 42 | 1 | 解けた(cf9a2daa の `body_move` で系譜が切れた) |
| `tools/catalog.rs` `build_definitions` | 2 | 49 | 解けた。祖先は `definitions`(cf9a2daa、0.603)、`src/agent/tools/catalog.rs`(b11f2d91、改名)、`src/agent/tools/mod.rs`(a6b0c63f)、`src/agent/tools.rs` と `src/agent_tools.rs` の `agent_tool_definitions` |
| `duckdb/mod.rs` `mod tests;` | 17 | 27 | 解けなかった |

`SessionId` の型の祖先は `contract.rs` の 1 つだけである。型が `contract.rs` に現れたコミット(b11f2d91)では、元の `src/session/mod.rs` の型は変更も削除もされず(写しで、その型は 64078019 で `crates/horizon-workspace/src/session_id.rs` に移った)、`content_match` の対象にならなかった。同じコミットで `src/session/mod.rs` の impl は変更され、その前の側が `contract.rs` の impl と 0.5 で似ていて、後の側とは似ていなかったので `body_move` になった。impl の祖先がさらに古い `src/workspace.rs` まで届くのはこのためである。

`mod tests;` が解けなかった理由。2f4105f6 で、`duckdb/mod.rs` のインラインの `mod tests { … }` は `mod tests;` の 1 行になり、中の 34 個のテスト関数は `tests.rs` の最上位の関数になった。`tests.rs` というファイルは unit ではなく(ファイルは暗黙のモジュールで unit を持たない)、前の側の `tests` に似た 1 つの追加された unit が無いので、`body_move` は起きない。関数の側は 1 つずつ `content_match` でつながった(このコミットの `content_match` は 45 本、うち名前の最後の部分が同じもの 37 本、`ambiguous` 12 本)。`mod tests` の 17 → 27 の増分は、ファイル自身の改名(b11f2d91、e899e99d)と、さらに前の `src/agent_duckdb_state.rs` からの `content_match`(d337d4fb)で届いた、同じインラインのモジュールの古い履歴である。依頼文の規則のうち、これを切れるものは無かった。追加された unit の和やファイルを受け手にする規則、受け手なしで残り殻を切る規則は、依頼文が許す規則の外で、作っていない。

旧監査の台帳の、後ろ向きの記録で基準の木(e3c619a0。範囲の上では同じ題の e223bc20 を基準の位置に使った)に対応しなかった項目。後ろ向きの記録は対応しなかった項目を 9 件と書いているが、その表に並ぶのは 8 件で、`reviews.json` の `preserve` のうちこの 8 つの名前の項目も 8 件だった。

| 台帳の項目 | HEAD の unit | 系譜で基準の時点の unit に届くか |
| --- | --- | --- |
| `policy.rs` `plan_prepared_call` | 名前のまま | 届かない。cf9a2daa の `body_move`(0.733)で `plan_tool_call` に届くが、`plan_tool_call` の最初の変更は基準の 104 コミット後だった |
| `conversation/mod.rs` `ConversationHistory.apply` | 名前のまま | 届かない(祖先なし) |
| `tools/bash/shell.rs` `tokenize` | 名前のまま | 届く。f120f173 の `content_match` で `tools/bash/git.rs` の `tokenize` |
| `tools/completion.rs` `ToolCompletion.live_request` | 名前のまま | 届かない(祖先なし) |
| `tools/execution.rs` `execute_synchronous` | 名前のまま | 届かない(祖先なし) |
| `session/completion.rs` `publish_tool_update` | 名前のまま | 届かない(祖先なし) |
| `src/terminal/paint.rs` `paint_terminal` | 名前のまま | 届く。1b15a6a5 の `content_match` で `src/terminal/mod.rs` の `paint_terminal`、さらに 04d9f0ee で `shell-gpui/src/terminal/mod.rs` |
| `src/workspace/modals.rs` `WorkspaceShell.fetch_picker` | 名前のまま | 届かない(祖先なし) |

後ろ向きの記録が「範囲の中でファイルを移った関数への判定とみなせる」とした 2 件が、系譜で届いた。残りは基準の後で新しく書かれた関数で、HEAD では名前のまま今の unit に対応する。

## 7. 依頼文が定めていなかった決め方

- 類似度の対象は unit の行(`start_line` から `end_line`)で、同じ同一性の unit が 1 つの blob に複数あれば(2 つの `impl Foo`)その行をすべて合わせる。行は `hash()` で比べる。
- `content_match` と `body_move` は、同じ種類か、関数とメソッドの組だけを比べる。impl とメソッドのように入れ子の関係にある別の種類どうしが、本文の包含で閾値を越えるのを避けるためである。
- `ambiguous` の「上位 2 つの候補」は、共通の行を 1 行以上持つすべての組で、閾値未満の組も数える。modified の unit の前の側にとっては、自分の後の側も候補である。
- `file_rename` は、改名されたファイルの、両側に同じ種類と限定名で現れるすべての unit に引く。純粋な改名では、旧パスの直前の変更(範囲の中で hunk を持つもの)の新しい側の unit を使う。範囲の外(`since` より前)にしか変更が無ければ引かない。
- 残り殻の同じコミットの `file_rename` も引かない(改名と本体の移動が同じコミットのとき)。
- 祖先は、リンクのコミットより前の自分のコミットだけを渡す。リンクのコミット自身は、後継の側の変更(`added` など)として数える。
- `commit_order` は `git_commits` が出す順(git log の順)とした。`git_commits` は親を出さないので、トポロジカルな順を SQL で作れない。永続キャッシュでは、追加されたコミットを前からあるものの後ろに番号づける。
- `unit_history` の `change_kind` と `depth` は、そのコミットに触れた同一性のうち最も浅いものの値とした。
- `ancestors` は自分自身の同一性を除く(往復の移動で同じ同一性に戻った場合)。`successors_at_split` は系譜のリンクのうち最大の後継の数で、リンクが無ければ 1 である。
- 永続キャッシュは継承の関係をコミットごとに持ち、履歴の作り直し(amend など)では消さない。コミットの継承の関係は、そのコミットと祖先だけで決まるからである。作り直すのは履歴の鍵、unit の導出、`lineage_key` が変わったときで、そのときは blob のファイルも書き出し直す(本文を読むため)。
- `unit_dependencies` の churn も `unit_history` にした(依頼文の 6 つに無いが、注釈が「`unit_churn` と同じ」と書いていたため)。`unit_fan_in_churn` には並びの鍵の列 `fan_in_x_commits` を足した。
- `retro/signals.sql` の `fan_in` の信号は、クエリの並びではなく fan-in の順で並べ直した。クエリの並びをそのまま順位に使うと、`fan_in` が積の順になるためである。歴史の信号(`unit_churn` など)は系譜のコミットを数え、正解(`retro/changes.sql`)は `unit_changes` の同一性のままである。

## 8. 更新の時間と大きさ

`check.py measure-refresh` を、同じ複製の上で、依頼前の SQL(14b60400)と後の SQL で 2 回ずつ実行した(段階ごとに新しいプロセス、`nice -n 19`)。

| 段階 | 前 | 後 | 後の系譜の層 |
| --- | ---: | ---: | ---: |
| 初回(空のキャッシュから) | 18.5〜19.0 s、6.7〜6.8 GB | 24.5〜24.7 s、6.6〜6.9 GB | 5.50〜5.52 s |
| 変更なし | 0.18〜0.22 s | 0.26〜0.28 s | 0.03 s |
| 1 ファイルを編集 | 0.29〜0.31 s | 0.30〜0.35 s | 0.02 s |
| 10 ファイルを編集 | 0.36〜0.37 s | 0.39〜0.56 s | 0.03 s |
| 3 ファイルの 1 コミット | 0.37〜0.38 s | 0.55〜0.64 s | 0.18〜0.27 s |
| HEAD の amend(履歴の作り直し) | 2.64〜2.68 s | 2.80〜3.47 s | 0.22〜0.29 s |
| SCIP の索引の変更 | 0.53〜0.66 s | 0.56〜0.57 s | 0.02 s |

依頼文が引いた記録の値は、初回が約 20 s、変更なしが 0.23〜0.27 s である。初回の差 5.5〜5.7 s は系譜の層で、1,613 コミットの継承の関係(20,970 行)を作る時間である。新しいコミットでは、そのコミットだけを導出する(0.18〜0.27 s。`unit_changes` 全体を一度計算してから絞るため、変更の大きさにはあまりよらない)。amend では、履歴は作り直されるが、継承の関係は amend されたコミットの分だけ導出された。変更なしでは、挿入の偽の条件で計算が畳まれ、0.03 s だった。

キャッシュのファイルは、前が 622.8 MiB、後が 620.8 MiB だった(どちらも削除した行を詰めないファイルの、この測定の終わりの大きさ)。キャッシュの上のクエリは、前と後で、`unit_churn` 0.14〜0.16 → 0.18〜0.20 s、`clone_change_consistency` 0.17〜0.18 → 0.22 s、`unit_fan_in_churn` 0.13〜0.15 → 0.16 s、`unit_dependencies` 0.19〜0.21 → 0.21〜0.24 s、`unit_cochange` 0.26〜0.34 → 0.24〜0.25 s、`cochange_without_reference` 1.02〜1.18 → 0.23〜0.24 s だった(更新の時間を含む)。`cochange_without_reference` が速くなった理由は調べていない。

セッションキャッシュ(`cache.sql`)の作成は 1 回の測定で 16.6 s、最大 8.9 GB で、そのうち継承の関係の写しが 5.0 s だった(依頼前の README の値は 17〜20 s)。ライブ(キャッシュなし)の値は 1 節に書いた。

## 9. 検査

`check.py fixture` の系譜の fixture は、履歴の fixture に 7 つのコミットを足す。`src/ops.rs` の追加、`git mv` で `src/geometry.rs` に改名して `build_table` を 1 行変更、`scale` を `resize` に改名、`translate` を `src/moves.rs` に移動、`describe` を `describe_size` と `describe_kind` に分割、`sum_widths` と `sum_heights` を `sum_sides` に統合、`build_table` の本体を `table_rows` に移して `build_table` を 1 行の呼び出しにする、である。確かめたこと:

- `commit_order` が `git rev-list --reverse HEAD` と同じ順であること。
- `same_name` 以外の継承の関係のすべての行。`file_rename` 6 本(`build_table` は 7/9、他は 1.0)、`scale` → `resize` 0.75、`translate` 1.0、分割の 2 本が 6/11 で `ambiguous`、統合の 2 本が 0.625 で `ambiguous`、`build_table` → `table_rows` の `body_move` が 7/9 で `ambiguous` でないこと。残り殻のコミットに `same_name` が無いこと。
- 後継ごとの `unit_lineage` の鎖(たとえば `resize` ← `scale`(rename_fn)← `src/ops.rs` の `scale`(rename_file))と、残り殻の `from_sha`。
- `unit_churn` のコミット、`commits_by_name`、`ancestors`、`successors_at_split`。`resize` 2 / 1 / 2 / 1、`describe_size` と `describe_kind` 2 / 1 / 2 / 2、`sum_sides` 2 / 1 / 4 / 1、`table_rows` 3 / 1 / 2 / 1、残り殻の `build_table` 1 / 2 / 0 / 1。`unit_churn` が、すべての unit で `unit_history` と `unit_lineage_counts` に一致すること。
- `lineage_threshold` を 0.7 にすると、分割と統合のリンクが消え、他は残ること。
- `cache.sql` の写しの上で同じ行が出て、クエリの計画が源を読まないこと。

履歴の fixture では、その場で変更された unit の `same_name`、改名された結合テストの関数(`integration` → `integration_area`、0.5)の `content_match`、純粋な改名(`tests/integration.rs` → `tests/area.rs`)の `file_rename` と、その関数のコミットが 0 から 2 になることを確かめた。更新の fixture では、各段階で系譜のビューがライブと一致し、継承の関係が初回に作られ、新しいコミットで 1 コミット分、amend で amend されたコミットだけ導出され、中断の後に作り直されることを確かめた。`check.py fixture` の 7 つの段(fixture、history、lineage、scip、graph、refresh)はすべて通った。

`retro/` は動くことだけを確かめた。複製(HEAD)で `retro/changes.sql` を `since=e223bc20` で実行し(10,516 行、175 コミット)、同じ複製を基準の代わりにして `retro/signals.sql` を最後まで実行した。基準の木の索引は作っておらず、後ろ向きの記録の数の再計算はしていない(依頼文が求めていない)。

このリポジトリの複製での `check.py refresh` は、1 回目(系譜の層を初めて作った更新、6.26 s)、2 回目(0.34 s)とも、更新したすべてのビュー(系譜の 5 つを含む)とクエリがライブと一致した。

`check.py explain` の `READ_AST` の数は、前と後で同じだった(履歴を読むクエリは今のファイルの分と blob の分で 2、他は 0 か 1)。`READ_TEXT` は、`unit_churn`、`clone_change_consistency`、`unit_fan_in_churn` で 4 → 9、`unit_dependencies` で 18 → 23、`unit_cochange` と `cochange_without_reference` で 15 → 18 に増えた。系譜が blob の本文を読む `read_text` と、`unit_change_sides` を 2 度目に読む分の bytes と lines である。

## 10. 再現のコマンド

```sh
nice -n 19 duckdb-ext/build.sh
git clone -q . target/code-db/clone                       # 14b60400
(cd target/code-db/clone && rust-analyzer scip . --output target/code-db/index.scip)   # 254 s
duckdb-ext/sql/jscpd.sh target/code-db/clone target/code-db/clone/target/code-db/jscpd src crates preview-plugin/src
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 2
duckdb-ext/sql/run.sh query target/code-db/clone unit_churn LIMIT=20
duckdb-ext/sql/run.sh query target/code-db/clone unit_fan_in_churn LIMIT=10
duckdb-ext/sql/run.sh query target/code-db/clone unit_churn lineage_threshold=0.7   # 系譜を作り直す
```

`commits_by_name` と依頼前の `unit_churn` の比較、閾値 0.7 の数、台帳の名前の系譜は、`unit_successions`、`unit_lineage`、`unit_lineage_segments` を `run.sh sql` で読む一回限りの SQL で取った(コミットしていない)。

## 11. 限界

- 系譜は同じコミットの中だけで見つける。あるコミットで消え、後のコミットで別の場所に作り直された unit はつながらない。
- 本文の半分を超えて書き換えた改名や移動は 0.5 を下回る。前身の行の半分未満しか持たない部分への分割は、どの部分にもつながらない(部分の Jaccard 係数は、その部分の割合以下になる)。
- 本文が複数の unit やファイルに移った場合は、残り殻を切らない(6 節の `mod tests;`)。
- 短い unit はつながりやすい。シグネチャの行と閉じ括弧を共有する 3 行の関数どうしは、それだけで 0.5 になる。同じひな形から書いたテスト関数が、名前の違う組の多くを占める。
- 同じパスと名前で消えて後で現れた同一性は、`unit_changes` と同じく 1 つの同一性のままである。
- ライブでは、履歴を読むクエリが導出と `unit_changes` を別々に読み、依頼前の約 2 倍のメモリを使う(1 節)。
