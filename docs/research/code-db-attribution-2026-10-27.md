# コードデータベースの帰属の規則(2026-10-27)

コードデータベースの SQL 層の 9 つ目の増分である。[数え方の 5 つの規則の記録](code-db-counting-rules-2026-10-27.md)(以下「数え方の記録」)の 5 節が残した M1(変更した行を最も内側の unit に付ける規則)の未決の点について、2 つの扱いを組み、fixture の確かめを足し、今の上位の候補と後ろ向きの precision への効き方を測った。SQL と `check.py` だけを変え、Rust は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、オーナーは 2026-10-27 に、次の 2 つの扱いについて「どちらも推奨でよいです」と承認した。

1. **item の前にある doc コメントと属性は、その item の unit に属する。** Rust では、item の直前の `///` などの doc コメント(tree-sitter の `line_comment` か `block_comment` で、item との間にコメントと属性しか無いもの)と `attribute_item` は、帰属のための unit の範囲に入る。これは言語の規則で、`lang/rust.sql` に置く。
2. **空白だけの行は、どの unit にも付かない。** 帰属を求める側で空行(前後の空白を除くと空)である hunk の行は、その側の帰属に使わない。囲む unit は、内側の unit の外にある、自分の空行でない行(頭、where 節、フィールド、後ろのコード)だけで付く。

M1 と M5 は既定のままとする、という決定も依頼文にある。前提は、数え方の記録、`duckdb-ext/sql/` の SQL と README、[読み判定 2](code-db-dogfood-reading-2-2026-10-26.md) の 45 件の候補とその判定である。記録の決定は問い直していない。

## 要約

- **2 つの扱いを、innermost(既定)の帰属に組んだ。** 1 は `lang/rust.sql` の unit の規則に新しい列 `lead_start_line`(前置きの doc コメントと属性の始まりの行)として足し、`start_line` と `loc` は変えていない。2 は blob の本文から空行を求める層 `blob_blank_lines` を足し、`unit_change_sides` がそれを読む。`attribution := 'all'` の結果は変わらない(HEAD の複製で `unit_changes` 40,924 行が前の SQL と一致した)。継承の関係(`unit_successions` 20,970 行)も一致した。
- **innermost の `unit_changes` は 39,434 → 39,024 行になった。** `modified` は impl 853 → 68、module 874 → 420 に減り、function 7,185 → 7,540、method 2,957 → 3,201、type 1,535 → 1,750 に増えた。前置きの行が item 自身に付くようになったためで、method の `added`(3,872 → 3,880)と `removed`(1,950 → 1,957)も少し増えた。innermost は、`all` が何も付けない変更に unit を付けることがあるので、もう `all` の部分集合ではない。
- **残った impl と module の `modified` の内訳**は、impl 68 のうちコードの行を含むものが 51(前と同じ)、自分の前置きの doc コメントか属性だけが 8、それ以外の doc コメントか属性が 1、コメントだけが 8、空行だけが 0 だった。module は 420 のうち 329(前と同じ)、23、2、66、0 だった。
- **読み判定 2 の 45 件のうち、数え方の記録の 19 件に加えて 7 件が上位から外れた**(上位 10 件。循環は変わらない)。`unit_churn` 6 番の `Workspace` impl、`unit_fan_in_churn` 2・5・9 番の `ToolSessionState`・`AgentdState`・`WriterHandle` の impl、`clone_change_consistency` 3 番の `AgentdHandle` と `TerminaldHandle`、`cochange_without_reference` 4・5 番の `NamedProviderConfig` の 2 組である。外れたものは、数え方の記録が測った使い捨ての扱い(コードの行を含まない impl と module の `modified` を数えない)が外したものと、co-change の 2 組を除いて同じだった。
- **外れた 7 件のうち `問題あり` は 2 件で、`AgentdState` impl と、`AgentdHandle` と `TerminaldHandle` である。** `AgentdState` impl のコミットは 24 → 5 になった。同じ原因を指す `AgentdState` 型は fan-in の 2 位に残った(25 → 26 コミット)。`AgentdHandle` と `TerminaldHandle` の片側は 9 / 0 → 1 / 0 になった。断片が複数の method にまたがるので側が impl に解決し、impl の自分の行がほとんど変わらないためである(3 節)。
- **後ろ向きの `production` の範囲では、正解も少し変わった**(1 コミット以上の refactored 760 → 778、3 コミット以上 89 → 92)。1 コミット以上で、`fan_in_x_commits` の P@10 が 0.8 → 0.9、P@100 が 0.43 → 0.48、co-change の P@100 が 0.49 → 0.43、`fan_in` の P@10 が 0.2 → 0.4 になった。`unit_churn` とクローンの動きは 0.02 以内だった。
- **fixture、更新、計画。** `check.py fixture` が通った(counting fixture に 6 つのコミットを足した)。HEAD の複製で `check.py refresh` の 2 回の更新がすべてのビューとクエリでライブと一致した。`explain` の `READ_AST` の数は変わらず、履歴を読むクエリのライブの `READ_TEXT` が 2 つずつ増えた。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 66115382 まで fast-forward した。測定は使い捨ての複製 3 つで行い、作業ツリーのファイルと履歴は測定に使っていない。複製は、数え方の記録の worker が作ったもの(`target/code-db/head` = e30d981a、`retro/base` = e3c619a0、`retro/end` = 46c37290)を、索引(`index.scip`)と jscpd の報告ごと reflink で写した。jscpd の報告は絶対パスで書かれているので、写した後に複製の場所へ書き換えた(書き換える前は、クローンの側がどのファイルにも解決せず、片側のある行が 0 になっていた)。DuckDB の拡張は同じ worker が作ったものを写した(`duckdb-ext` の Rust は e30d981a と 66115382 の間で変わっていない)。キャッシュは複製ごとに作り直した。

前の SQL は 66115382 の `duckdb-ext/sql`(`git archive` で `target/code-db/oldsql` に取り出したもの)、後の SQL はこのブランチのものである。2 つは `cache_path` で別のキャッシュのファイルを使った(スキーマの版が違う)。前の SQL での既定の上位の候補は、数え方の記録の `fixed` の列と、上位 30 件のすべての行が一致した。後ろ向きの前の SQL の値も、数え方の記録の `production` の fixed / innermost の値と一致した。

機械はオーナーの対話用デスクトップで、他の作業と共有している(測定中の 1 分の load average は 1.6〜11)。DuckDB CLI 2.0.0、sitting_duck v1.16.0 を使い、すべて `nice -n 19` で動かした。測定用の SQL とシェルは `target/code-db/measure/` に置き、コミットしていない。

```sh
duckdb-ext/sql/run.sh sql target/code-db/head target/code-db/measure/top.sql m_out=… m_label=after [attribution=all]
target/code-db/oldsql/duckdb-ext/sql/run.sh sql target/code-db/head target/code-db/measure/top.sql m_label=before …
# 後ろ向き(production だけ。前の SQL と後の SQL のそれぞれで)
run.sh sql END duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 retro_changes=…
run.sh sql END duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 …
run.sh sql BASE duckdb-ext/sql/retro/signals.sql retro_scope=production retro_min_commits={1,3} …
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/head
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/head
```

## 1. 組んだもの

### 前置きの doc コメントと属性(`lang/rust.sql`)

`rust_units_over` の出力に `lead_start_line` を足した。unit の節の兄弟のうち、その節の直前に続く属性とコメントの並び(`rust_tags` が `#[test]` などを探すのと同じ並び)を求め、その並びの中の最初の `attribute_item` か outer doc コメント(子に `outer_doc_comment_marker` を持つ `line_comment` か `block_comment`、つまり `///` と `/** */`)の行を始まりとする。無ければ `start_line` である。並びは、属性でもコメントでもない兄弟(名前の無いトークンを含む)で区切る。

- 並びの中で、最初の doc コメントか属性より前にある普通のコメントは前置きに入らず、後ろにあるものは範囲に入る。
- inner doc コメント(`//!`)は、後ろの item ではなく、それを囲む item のものなので、前置きに入らない。
- doc コメントと item の間の空行は並びを切らない(構文木では空行は節にならない)。その空行は 2 の扱いでどの unit にも付かない。

`start_line` を前置きまで広げる案は採らなかった。`start_line` は `loc`、系譜の本文(`[start_line, end_line]` の行の集合)、グラフの範囲の判定、SCIP の囲む範囲との照合、各記録の数が読んでおり、広げるとそのすべてが変わる。新しい列は innermost の帰属だけが読む。HEAD の複製では、今の unit 9,264 個のうち 5,776 個、blob の unit 169,911 個のうち 107,975 個に前置きがあった。

列が増えたので、永続キャッシュの `cache.units` と `cache.blob_units` に列を足し、スキーマの版を 4 → 5 にした(前の版のファイルは、前と同じく、消すよう求めるエラーになる)。`rust_units_over` の本文が変わるので、最初の更新はすべての unit を導出し直す。

### 空行(`views/blobs.sql`、`views/unit_changes.sql`)

`blob_blank_lines(blob_id, line)` を足した。blob の本文(`blob_sources`)を行に分け、ASCII の空白(スペース、タブ、CR、VT、FF)だけの行を返す。`source_lines_of` と `loc` が空行とみなす規則と同じで、言語によらない。`refresh.sql` は、blob の unit を導出するとき(`cache.blob_derived` に無い blob)に同じ blob の空行を `cache.blob_blank_lines` に足し、`cache.sql` は一時表に写す。HEAD の複製で 282,008 行(6,594 blob)だった。

`unit_change_sides` の innermost は、各 unit の自分の行を `[lead_start_line, end_line]` から入れ子の unit の同じ範囲を除いた区間として作り、hunk がその区間で変えた行 `[lo, hi]` のすべてがその側で空行なら、その hunk ではその unit を変わったとしない。`all` の計算は変えていない。

依頼文は、空行を `lineage_sources` などの本文から求めてよいとしていた。`lineage_sources` は使わなかった。理由は 3 つある。1 つは、`unit_change_sides` が本文を直接読むと、キャッシュの上の `unit_churn` などの計画に `READ_TEXT` が入り、キャッシュの上のクエリが本文を読まないことを確かめる既存の検査(`check.py` の `assert_no_source_scan`)が落ちること、もう 1 つは、更新のたびに継承の関係を導出するとき、`derived_unit_successions` が `unit_change_sides` の全体を作るので、そのたびにすべての blob の本文を読むことになること、最後は、`lineage_sources` が `views/lineage.sql` で定義され、`views/unit_changes.sql` より後に読まれることである。blob の本文から一度だけ求めて unit と同じようにキャッシュする形にした。

### 系譜と `all`

継承の関係は、前と同じく `change_kind_all` を読むので、2 つの扱いの影響を受けない。HEAD の複製で、`unit_successions` は前の SQL と後の SQL で 20,970 行、行の hash の和も一致した。`attribution := 'all'` の `unit_changes`(40,924 行)と `unit_history`(23,013 行)も、前の SQL と行の hash の和まで一致した。`refresh.sql` の `lineage_key` は `unit_change_sides` の本文を含むので、最初の更新で継承の関係を導出し直す(結果は同じ)。

## 2. 測定 A:今の上位の候補

### `unit_changes`

HEAD の複製(e30d981a の Horizon のソース)の全履歴。前は前の SQL の既定(innermost)、後は後の SQL の既定である。`all` の列は前と後で同じだった。

| 種類 | 変更 | 前 | 後 | `all` |
| --- | --- | ---: | ---: | ---: |
| function | modified | 7,185 | 7,540 | 7,187 |
| method | added | 3,872 | 3,880 | 3,872 |
| method | modified | 2,957 | 3,201 | 2,957 |
| method | removed | 1,950 | 1,957 | 1,950 |
| impl | modified | 853 | 68 | 1,917 |
| module | modified | 874 | 420 | 1,297 |
| type | modified | 1,535 | 1,750 | 1,536 |
| 合計 | | 39,434 | 39,024 | 40,924 |

ほかの行(すべての `added` と `removed`、macro)は変わらなかった。function、method、type の `modified` が増えたのは、item 自身の doc コメントや属性だけを変えたコミットが、前はどの unit にも付かないか(トップレベルの item)、囲む unit に付いていた(impl の method)のが、その item に付くようになったためである。method の `added` と `removed` が増えた 15 行は、impl の改名(`AgentdHandle` → `SessiondHandle` など 4 コミット)で method の同一性が変わり、その method の中で hunk に触れたのが doc コメントの行だけだった場合だった。`all` では、この method はどちらの側でも付かない(doc コメントは method の範囲の外にあり、impl の頭の行も method の外にある)。

innermost で `modified` の impl と module を、自分の行の変更を本文で分けると次のとおりだった(新旧の両側。分け方は数え方の記録と同じで、行の先頭の文字で `///`・`//!`・`/**` を doc、ほかの `//`・`/*`・`*` をコメント、`#[`・`#![` を属性とした)。後の「自分の行」は `[lead_start_line, end_line]` から入れ子の unit の同じ範囲を除いたもので、空行を含めて分けた。

| | コードの行を含む | doc コメントか属性(自分の前置きだけ) | doc コメントか属性(ほかを含む) | コメントだけ | 空行だけ | 計 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| impl、前 | 51 | — | 692 | 2 | 108 | 853 |
| impl、後 | 51 | 8 | 1 | 8 | 0 | 68 |
| module、前 | 329 | — | 525 | 7 | 13 | 874 |
| module、後 | 329 | 23 | 2 | 66 | 0 | 420 |

前の行は数え方の記録の表(impl 51 / 692 / 110、module 329 / 525 / 20)と同じである(その表の「空行かコメントだけ」は、ここでは「コメントだけ」と「空行だけ」に分けた)。後の「自分の前置きだけ」は、impl や module 自身の doc コメントか属性(`#[cfg(test)]` など)の変更である。「ほかを含む」は、自分の前置きでも入れ子の unit の前置きでもない doc コメントか属性で、unit でない item(定数、`use` など)のものである。後の「コメントだけ」が module で 7 → 66 に増えたのは、前は doc コメントや属性の行と一緒に変わっていた普通のコメントの行が、doc コメントや属性が内側の unit に移った後に残ったためである。

### 読み判定 2 の 45 件

数え方の記録の既定(前)で上位 10 件に残っていた 26 件(循環を除くと 23 件と循環 3 個)のうち、後の既定で上位 10 件から外れたのは次の 7 件だった。循環は帰属を読まないので、24 個の循環のすべてが前と同じだった。上位から外れていた 19 件が戻ったものは無かった(クローン 5 番が 15 → 11 位、クローン 10 番が 11 → 14 位)。

| 候補(読み判定 2 の番号) | 判定 | 前 | 後 |
| --- | --- | --- | --- |
| churn 6 `horizon-workspace` の `Workspace` impl | 問題なし | 7 位、36 コミット | 30 位の外、5 コミット |
| fan-in 2 `ToolSessionState` impl | 問題なし | 2 位、36 コミット | 30 位の外、1 コミット |
| fan-in 5 `AgentdState` impl | 問題あり | 10 位、24 コミット | 30 位の外、5 コミット |
| fan-in 9 `WriterHandle` impl | 問題なし | 7 位、20 コミット | 30 位の外、4 コミット |
| クローン 3 `AgentdHandle` と `TerminaldHandle` | 問題あり | 7 位、片側 9 / 0、両側 2 | 30 位の外、片側 1 / 0、両側 1 |
| co-change 4 `NamedProviderConfig` と `RawConfig.resolved_providers` | 問題なし | 1 位、5 / 5 / 5、100% | 30 位の外、5 / 6 / 5、83% |
| co-change 5 `NamedProviderConfig` と `ResolvedProviderConfig` | 問題なし | 2 位、5 / 5 / 5、100% | 30 位の外、5 / 6 / 5、83% |

数え方の記録が測った使い捨ての扱い(コードの行を含まない impl と module の `modified` を数えない。その記録の 5 節)の上位と比べると、外れた impl の 5 件は同じだった。違うのは co-change の 2 組で、使い捨ての扱いでは 1・2 位のままだった。`NamedProviderConfig` 型には、66d2113d(「Apply model switches from one resolved configuration snapshot」)で型の doc コメントだけを変えた変更があり、2 つの扱いでこれが型自身の変更になった。型のコミットが 5 → 6 になり、結合が 100% から 83% に下がって、結合 100% の組に抜かれた。

### 2 つの `問題あり`

依頼文は、この 2 件が上位 10 件に残るかを報告するよう求めていた。どちらも残らなかった。

- **`AgentdState` impl(fan-in 5 番)。** innermost で impl 自身に付いたコミットは、前の 11 から 1(8d8cdd0e、改名の一斉の変更)になった。前の 11 のうち 10 は、method の doc コメントや属性、method の間の空行の変更だった。lineage を通した `unit_churn` のコミットは 24 → 5 で、fan-in 21 との積が 504 → 105 になった。読み判定 2 の判定の理由は、reload が `agent_config` と `providers` を別々の lock で入れ替え、起動が別々に読むことで、その箇所は `AgentdState.reload_provider_config` などの method と、2 つの `Mutex` を持つ型の定義にある。同じ原因を指す `AgentdState` 型は fan-in の 2 位に残った(30 × 26 = 780。前は 3 位)。数え方の記録の `session_lifecycle.rs` の impl と同じく、同じ原因を指す候補が impl から型に移った形である。
- **`AgentdHandle` と `TerminaldHandle`(クローン 3 番)。** 5 つの断片のどれも複数の method にまたがるので、`clone_change_consistency` は側を断片を含む最も小さい unit、つまり 2 つの impl に解決する。impl 自身の行(頭、閉じ括弧、method の間の行)は、2 つの扱いの後はほとんど変わらず、片側は 9 / 0 → 1 / 0 になった。判定の理由は `begin_reload`・`stop_and_wait`・橋渡しのスレッドが字句まで同じに 2 つあることで、写しは method の中にある。数え方の記録が書いたとおり、これは M1 と、クローンの側を囲む unit で数える数え方(機構 M4、今回も直していない)の組み合わせで起きる。

依頼文は、規則が `問題あり` を外し、規則と判定のどちらが誤っているかを分けられない場合は止めて報告するよう求めていた。この 2 件では、外れた機構(impl の数から method の前置きの行が抜けたこと、クローンの側が impl に解決すること)は上のとおり分けられたので、止めずに測定を続けた。判定そのものは読み直していない。

### 上位 10 件の前と後

各信号の上位 10 件。後に入った候補は判定していない。

- **`unit_churn`**:前は `run_session`(50)、`build_definitions`(49)、`WorkspaceShell.execute`(45)、`WorkspaceShell.new`(41)、`WorkspaceShell` 型(40)、`WorkspaceShell.reconcile`(38)、`Workspace` impl(36)、`ToolSessionState` impl(36)、`run_provider_stream`(35)、`Render for WorkspaceShell.render`(34)。後は `run_session`(51)、`build_definitions`、`execute`、`new`、`WorkspaceShell` 型、`reconcile`、`run_provider_stream`、`render`(ここまで前と同じ数)、`spawn_rig_session`(34、入った)、`TerminalSession.spawn`(34、入った)。
- **`unit_fan_in_churn`**:前は `Event`、`ToolSessionState` impl、`AgentdState` 型、`Command`、`ToolCallResult`、`RigAgentConfig`、`WriterHandle` impl、`ToolCallRequest`、`MessageRole`、`AgentdState` impl。後は `Event`(89 × 24)、`AgentdState` 型(30 × 26)、`ToolCallResult`(59 × 12)、`Command`(41 × 17)、`ToolCallRequest`(64 × 9)、`RigAgentConfig`、`MessageRole`、`WorkspaceShell` 型(12 × 40、入った)、`SessionId` impl(78 × 5、入った)、`CommandId`(13 × 30、入った)。
- **`clone_change_consistency`**:前の 1〜6 位と `open_palette` と `open_session_manager`(8 → 7 位)はそのまま。`tools/tests.rs` と `src/board/model.rs` の `tests` は 0 / 7 → 0 / 6(9 → 8 位)。`AgentdHandle` と `TerminaldHandle`(7 位)と、`persistence.rs` の `tests` と `tests.rs`(10 位)が外れ、`duckdb/append.rs` の `Store.append_event` と `Store.append_record_uncommitted`(3 / 2、両側 11)、`open_palette` と `open_view_chooser`(2 / 3、両側 5)が入った。
- **`cochange_without_reference`**:前の 3・5〜10 位(`render_composer_hints` と `stop_button`、`ViewKindState` と `ViewKind` の 4 組、`placeholder` の 2 組)は残った。`NamedProviderConfig` の 2 組(1・2 位)と `ProviderSummary` と `RawNamedProviderConfig`(4 位)が外れ、`ProviderSummary` と `named_provider_configs`(4 / 4 / 4)、`WorkspaceShell.control_plane_terminate` と `handle_terminal_exited`(4 / 4 / 4)、`client.rs` の `ModelClient` と `module client`(3 / 3 / 3)が入った。どれも結合 100% である。

ほかの数:`unit_cochange` の組 4,163 → 3,761、`cochange_without_reference` 1,239 → 1,203、`clone_change_consistency` の片側のある行 526 → 520。`unit_churn`(9,264 行)と `unit_fan_in_churn`(2,915 行)の行数、循環(24 個)は変わらなかった。

## 3. 測定 B:後ろ向き(`production` の範囲)

依頼文に従い、`production` の範囲だけを、前の SQL と後の SQL のそれぞれで正解と信号を作り直した(既定の変数。正解は innermost)。前の SQL の値は、数え方の記録の `production` の表の「fixed を innermost の正解で」の値とすべて一致した。

`production` は関数と method だけを数えるので、数え方の記録では innermost にしても正解が変わらなかった。今回は、関数と method の doc コメントや属性だけの変更がその unit の変更になったので、正解が変わった。

| | 前 | 後 |
| --- | ---: | ---: |
| refactored、1 コミット以上 | 760 / 2,583(29.4%) | 778 / 2,583(30.1%) |
| refactored、3 コミット以上 | 89(3.4%) | 92(3.6%) |
| preserve | 30 | 30 |

各欄は「前 / 後(差)」で、角括弧の中は分解のための 2 つの組み合わせ [後の信号を前の正解で、前の信号を後の正解で] である。

### 1 コミット以上

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.90 / 0.90(±0)[0.90、0.90] | 0.76 / 0.76(±0)[0.76、0.76] | 0.71 / 0.72(+0.01)[0.72、0.71] | 0.09 / 0.09 |
| `loc` | 2,583 | 0.80 / 0.80(±0) | 0.78 / 0.78(±0) | 0.75 / 0.75(±0) | 0.10 / 0.10 |
| `clone_one_sided` | 154 | 0.80 / 0.80(±0)[0.80、0.80] | 0.74 / 0.72(-0.02)[0.72、0.74] | 0.65 / 0.66(+0.01)[0.66、0.65] | 0.09 / 0.09 |
| `fan_in_x_commits` | 1,307 | 0.80 / 0.90(+0.10)[0.90、0.80] | 0.52 / 0.50(-0.02)[0.46、0.54] | 0.43 / 0.48(+0.05)[0.46、0.45] | 0.06 / 0.06 |
| `cochange_without_reference` | 256 → 268 | 0.60 / 0.60(±0)[0.50、0.60] | 0.46 / 0.44(-0.02)[0.40、0.48] | 0.49 / 0.43(-0.06)[0.41、0.50] | 0.06 / 0.06 |
| `fan_in` | 1,307 | 0.20 / 0.40(+0.20)[0.20、0.40] | 0.32 / 0.36(+0.04)[0.32、0.36] | 0.29 / 0.32(+0.03)[0.29、0.32] | 0.04 / 0.04 |
| `module_cycles`(集合) | 580 | | | 232 / 240 個 | |

### 3 コミット以上

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.50 / 0.50(±0)[0.50、0.50] | 0.34 / 0.32(-0.02)[0.32、0.34] | 0.30 / 0.31(+0.01)[0.30、0.31] | 0.34 / 0.34 |
| `loc` | 2,583 | 0.50 / 0.50(±0) | 0.36 / 0.36(±0) | 0.29 / 0.29(±0) | 0.33 / 0.32 |
| `clone_one_sided` | 154 | 0.30 / 0.30(±0)[0.30、0.30] | 0.18 / 0.20(+0.02)[0.20、0.18] | 0.19 / 0.19(±0)[0.19、0.19] | 0.21 / 0.21 |
| `fan_in_x_commits` | 1,307 | 0.60 / 0.60(±0)[0.60、0.60] | 0.24 / 0.24(±0)[0.24、0.24] | 0.16 / 0.15(-0.01)[0.15、0.16] | 0.18 / 0.16 |
| `cochange_without_reference` | 256 → 268 | 0.00 / 0.00(±0)[0.00、0.00] | 0.12 / 0.06(-0.06)[0.06、0.12] | 0.14 / 0.11(-0.03)[0.11、0.14] | 0.16 / 0.12 |
| `fan_in` | 1,307 | 0.20 / 0.20(±0)[0.20、0.20] | 0.08 / 0.08(±0)[0.08、0.08] | 0.05 / 0.06(+0.01)[0.05、0.06] | 0.06 / 0.07 |
| `module_cycles`(集合) | 580 | | | 40 / 42 個 | |

### 変わったこと

- **依頼文が挙げた 3 つの信号のうち、`unit_churn` とクローンの動きは 0.02 以内だった。** co-change は、1 コミット以上の P@100 が 0.49 → 0.43、P@50 が 0.46 → 0.44、3 コミット以上の P@50 が 0.12 → 0.06、P@100 が 0.14 → 0.11 に下がり、P@10 は変わらなかった。分解の値では、下がり方は信号の変化で決まる(後の信号を前の正解で数えると 1 コミット以上の P@100 は 0.41、前の信号を後の正解で数えると 0.50)。順位の付いた組は 256 → 268 に増えた。数え方の記録で co-change の 1 コミット以上の P@100 は 0.41 → 0.49 に上がっており、今回の後の値 0.43 はその間にある。
- **`fan_in_x_commits` の 1 コミット以上の P@10 は 0.8 → 0.9、P@100 は 0.43 → 0.48 になった**(信号の変化による。分解の値では後の信号を前の正解で 0.9、0.46)。`fan_in` の P@10 0.2 → 0.4 は、信号が帰属を読まないので、正解の変化による。
- **`module_cycles` の集合は同じで、正解に入る数が 232 → 240(3 コミット以上 40 → 42)になった。** 正解の変化による。

## 4. 測定 C:fixture、更新、計画

### fixture

`check.py fixture` は、既存の fixture のすべてと、広げた counting fixture で通った(全体で 1 分 24 秒)。counting fixture には、別のファイル `src/doc.rs`(1 つの型、その impl に 2 つの method、doc コメントのある inline module)と 6 つのコミットを足し、2 つの帰属で次を確かめる。

| 場合 | innermost(既定) | `all` |
| --- | --- | --- |
| method の doc コメントを変える | method `Doc.get` | impl `Doc` |
| method の属性を変える(`#[inline]` → `#[inline(always)]`) | method `Doc.twice` | impl `Doc` |
| 区切りの空行と一緒に method を足す | method `Doc.thrice`(`added`)だけ | impl `Doc` と method `Doc.thrice` |
| inline module の外側の doc コメント(`///`)を変える | module `inner` | 無し |
| inline module の内側の doc コメント(`//!`)を変える | module `inner` | module `inner` |
| method の中と method の間に空行を足す | 無し | impl `Doc` と method `Doc.get` |

impl の頭の変更が impl に付くこと(既定でも `all` でも)は、既存の M1 の場合(`Sq` の impl に `where` を足すコミット)が確かめている。各 unit の `lead_start_line`(doc コメントか属性のある 3 つの unit で 1 行前、ほかは `start_line`)も確かめる。最後に、既存の比べ方に `attribution := 'all'` の `unit_changes` を足して、`cache.sql` の上で同じ行になることを確かめる。

history fixture の `unit_changes` の期待値は、2 つの帰属のどちらでも変わらなかった(前置きの行と空行だけの変更が無いため)。history fixture の「blob の unit と今の `units` が行ごとに等しい」の比べ方に `lead_start_line` を足した。`REFRESHED_VIEWS` に `blob_blank_lines` を足し、refresh fixture と `check.py refresh` がライブと比べるようにした。

### 更新と計画

HEAD の複製で、キャッシュを消してからの初回の更新は、`run.sh query … unit_churn LIMIT=3` の全体で、前の SQL が 25.0 s(`meta.seconds` の合計 22.6 s。`blob_nodes` 11.5 s、`blob_units` 1.6 s、`lineage` 5.7 s)、後の SQL が 28.3 s(25.9 s。13.3 s、2.4 s、6.1 s)だった。後の SQL の測定の終わりには load average が 11 まで上がっていた。`blob_units` の段の 0.8 s の差は、空行を求める分を含む。何も変わっていない更新は 0.24 s → 0.27 s、キャッシュのファイルは 649.9 MB → 651.7 MB だった。上位の候補を出す測定用の SQL(すべての信号のクエリを含む)は、キャッシュがある状態で 15.9 s → 17.6 s だった(下の結合を直す前の値。直した後の 1 回は 21.9 s で、そのときの load average は高かった)。

`check.py refresh` を HEAD の複製で実行し、2 回の更新のどちらでも、すべてのビュー(`blob_blank_lines` と系譜の 5 つを含む)とクエリがライブと一致した。更新は 26.2 s(最初の更新。`blob_units` の段 2.3 s)と 0.35 s で、コマンド全体はおよそ 9 分、子のプロセスの RSS の合計の最大はおよそ 25.5 GB だった。ライブの `SELECT count(*) FROM unit_changes` は、前の SQL が 12.9 s・最大 RSS 7.6 GB、後の SQL が 12.8 s・8.7 GB だった。

最初に組んだ `unit_change_sides` は、ライブでこのクエリが 30 GB を超えた(`check.py refresh` は 47 GB で OOM により終了した)。hunk と自分の行の区間の重なりを求める結合で、計画が `own_spans` と hunk を行の範囲だけで結んでいた。区間と hunk の側をそれぞれ先に materialize し、結合を `(sha, path, side)` の等しさと範囲の 1 つにしてから上の値になった。直した前と後で、上位の候補、`unit_changes` の行、後ろ向きの値はすべて同じだった。キャッシュの上では、直す前も問題は出ていなかった。

`check.py explain` の `READ_AST` の数は、前の SQL と同じだった(構文のビューは 1、履歴を読むクエリは 2)。履歴を読むクエリのライブの `READ_TEXT` は 2 つずつ増えた(`unit_churn`・`clone_change_consistency`・`unit_fan_in_churn` が 9 → 11、`unit_dependencies` が 23 → 25、`unit_cochange` と `cochange_without_reference` が 21 → 23)。ライブの `blob_blank_lines` が blob の本文を読む分である。キャッシュの上では本文を読まない(`assert_no_source_scan` は変えずに通った)。

## 5. 依頼文が決めていなかったこと

- **前置きをどこに持つか。** `start_line` を広げず、`lead_start_line` の列を足した(1 節)。`loc` は前と同じ意味である。
- **どのコメントが前置きか。** 直前の属性とコメントの並びのうち、最初の属性か outer doc コメントから(1 節)。普通のコメントだけの並び(区切りのコメントなど)は前置きにならない。doc コメントと item の間の空行は並びを切らない。inner doc コメント(`//!`)は囲む item に残る。
- **空行の本文。** `lineage_sources` ではなく、blob の本文から求めてキャッシュする `blob_blank_lines` にした(1 節)。そのため、キャッシュの表が 1 つ増えた。
- **空行の判定の単位。** hunk と自分の行の区間の重なりごとに、そのすべての行が空行なら付けない。行がコードから空行に変わった場合は、古い側ではコードの行なので、古い側で付く。
- **キャッシュのスキーマの版。** 列と表が増えたので 4 → 5 にした。
- **innermost が `all` の部分集合でなくなること。** 前置きの行は、innermost ではその item に付き、`all` ではその item に付かない(2 節)。依頼文は `all` の振る舞いを保つことを求めていたので、`all` は変えていない。
- **fixture の impl の頭の場合。** 既存の M1 の場合で確かめ、新しい場合は足していない。

## 6. 記録と食い違った点、気づいた点

- **数え方の記録の 5 節は、doc コメントか属性による impl の残りを 692 件としていた。** 2 つの扱いの後に、自分の前置きでも入れ子の unit の前置きでもない doc コメントか属性の変更が残ったのは impl 1 件、module 2 件で、残りはすべて内側の unit に移った。
- **数え方の記録の使い捨ての扱いとの違いは、co-change の 2 組だけだった。** 使い捨ての扱いは impl と module の数だけを変えたので、型の doc コメントの変更が型自身に付くことはなかった。
- **jscpd の報告は絶対パスを持つ。** 複製を別の場所へ写すと、クローンの側がファイルに解決しなくなり、`clone_change_consistency` の片側のある行が黙って 0 になった。報告を作り直すか、パスを書き換える必要がある。

## 7. 限界

- **判定は読み判定 2 のものを引いた。** 外れた `問題あり` についての 2 節の記述は、読み判定 2 の判定の理由と数の変化から書いたもので、コードを読み直してはいない。後に入った候補は判定していない。
- **後ろ向きは `production` の範囲だけである。** `all` の範囲の正解は帰属に依存し、数え方の記録で下がることが示されているので、依頼文に従い測っていない。
- **前置きの規則は Rust の構文木だけで決めている。** マクロの中の item、`#[path]` などの属性の意味は見ていない。
- **内訳の分類は行の先頭の文字だけで行った**(数え方の記録と同じ)。ブロックコメントの途中の行やマクロの中は区別していない。
- **1 つのリポジトリの、上位 10 件ずつ**である。
