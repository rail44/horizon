# 履歴の blob を構文の層に入れ、unit_changes をビューにした記録(2026-10-25)

コードデータベースの SQL 層の 4 つ目の増分である。履歴の各コミットで変わったファイルの、変更前後の内容(blob)を、今のファイルと同じ `read_ast` で読み、同じ言語ビューの規則で unit を導き、hunk と突き合わせて「どのコミットがどの unit に触れたか」(`unit_changes`)をビューにした。その上に、スパイクの 2 つのクエリ `unit_churn` と `clone_change_consistency` を移した。観察だけを記し、推奨は書かない。

設計文書の未決点は、履歴の blob を `read_ast` に渡す手段だった。候補は、blob の内容を返す暫定のテーブル関数、C++ のファイルシステム拡張、2.0 版の `duck_tails` を待つこと、の 3 つである。オーナーはこれに「暫定の表関数にしましょう」と答えた。その表関数の形として、依頼側は blob を行として返す形と、blob をファイルに書き出す形(B)を示し、オーナーは 2026-10-25 に「オッケーです。Bにしましょう。」と答えた。B では `read_ast` と `lang/rust.sql` を履歴にもそのまま使える。関数 `git_changed_blobs(repo, dir, since := NULL, include_merges := false, extensions := NULL)` の形は依頼の時点で決まっており、別の worker が git_history に並行して実装した(a0921826)。この作業は、関数が入るまで代役(1 節)で組み、関数が入った後に代役を外して、すべての検査と測定をやり直した。本記録の数値は、断りのない限り関数での値である。

前提は [設計文書](../code-database-design.md) と、前の記録 [code-db-views-single-scan-2026-10-24.md](code-db-views-single-scan-2026-10-24.md)・[code-db-incremental-cache-2026-10-25.md](code-db-incremental-cache-2026-10-25.md)(以下「前回の記録」)である。意味の基準は [スパイクの記録](code-db-spike-2026-10-24.md) と [gix の記録](gix-parity-2026-10-24.md) §12、スパイクのコード(`git show 475599d5:scripts/code-db/history.py`)である。作業は worker の作業ツリーで `origin/code-quality-survey` の 865e4045 から始め、関数の入った a0921826 に進めた。機械は前回と同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している。代役での測定の間の load average は 5〜23、関数での測定の間はそれより低かった(記録していない)。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0、sitting_duck は v1.16.0、git_history はこの作業ツリーで `duckdb-ext/build.sh` で作ったもの、jscpd は 5.3.1 である。Rust は変えていない。

## 主要な観察

- **言語ビューの規則は、表マクロ 1 つにすると、今のファイルにも blob にも同じ文面で使えた。** `rust_units_over(nodes_t, bytes_t, lines_t)` は入力を `query_table` で名前から読む。`MATERIALIZED` の共通表式はマクロの中でも守られ、`read_ast` の走査は適用 1 つにつき 1 回のままだった。
- **今のファイルの blob から導いた unit は、そのファイルの `units` と一致した。** この repo では、`blob_files` に blob がある 500 ファイル・8,530 unit で、id 以外の全列が両方向に 0 行の差だった。残る 21 ファイルの今の内容は、マージのコミットで作られていて、`include_merges` が偽のときは新しい側に現れない。
- **`unit_changes` は、同じ hunk を与えたスパイクのコードと行ごとに一致した。** a0921826 で 40,731 行である。0df26a53 では 40,506 行で、gix の記録の「gix の hunk から」の値と同じだった。4ad82864 では 40,326 行で、スパイクの記録の 40,332 行との差は、1 行を除いて git と gix の `-U0` の配置の違いで説明がつく。
- **2 つのクエリの上位は、スパイクの記録の表と同じ値だった。** `TerminalSession` の impl 46 コミット、`definitions` 42、`judge/chat.rs`–`judge/mod.rs` の tests の対 0 / 25 / 1 などである。17 コミット新しい木で測っている。
- **履歴の blob は 6,586 個(152 MB)、ノードは 31,361,613 行あった。** 今のファイル(1,555,402 行)の約 20 倍である。ライブの `blob_units` は 18.3〜18.5 s・ピーク RSS 8.1 GB、2 つのクエリはライブで 14〜20 s かかった。
- **永続キャッシュの初回の作成は 20.1〜25.8 s になった(前回 3.5〜3.6 s)。** そのうち 13.9〜17.6 s が blob の解析と格納である。その後は、何も変わっていないとき 0.24〜0.25 s、1 コミットで 0.36〜0.43 s、amend で 3.56〜4.17 s だった。キャッシュファイルは 616.3 MiB になった(前回 約 50 MB)。
- **キャッシュの上では、`unit_churn` が 0.16〜0.19 s、`clone_change_consistency` が 0.19〜0.21 s だった。** `unit_changes` を表に実体化すると、クエリは 0.05〜0.13 s になったが、実体化に 0.14〜0.20 s かかった。`unit_changes` はビューのままにした。

## 1. 組んだもの

| ファイル | 変更 |
| --- | --- |
| `views/blobs.sql`(新規) | `blob_files`(`git_changed_blobs` の行)、blob のスナップショット `blob_snapshot`・`blob_paths`、`blob_list(blob_id, file, blob_ord)`、`blob_nodes(blob_id, …)`、導出の入力 `blob_scope_nodes`・`blob_sources`・`blob_source_bytes`・`blob_source_lines` |
| `lang/rust.sql` | `rust_units` の本体を表マクロ `rust_units_over` にし、`rust_units` と `rust_blob_units` をその 2 つの適用にした。`rust_tags` は `scope_nodes` を読む形にしただけで、blob 用は作っていない |
| `views/files.sql` | `source_bytes`・`source_lines` の本体を表マクロ `source_bytes_of`・`source_lines_of` にした |
| `views/nodes.sql` | `scope_nodes`(`scope_files` のファイルのノード)を足した。範囲の絞り込みは、これまで 2 つの言語ビューに同じ文面で書かれていた |
| `views/units.sql` | `derived_blob_units`・`blob_units` を足した |
| `views/unit_changes.sql`(新規) | `unit_changes` |
| `queries/unit_churn.sql`・`queries/clone_change_consistency.sql`(新規) | スパイクのクエリを 1 始まりの行とビューに合わせて移した。見出しの問い・パラメータ・根拠の行は残した。スパイクの `group_id` の列は今の `clone_pairs` に無いので落とした |
| `00_load.sql` | `blob_dir` の既定値(`<repo>/target/code-db/blobs`) |
| `cache.sql`・`refresh.sql` | blob の層(4 節) |
| `check.py` | 履歴の fixture、測定の項目 |

読む順は `00_load.sql`、`views/files.sql`、`views/nodes.sql`、`views/blobs.sql`、`lang/rust.sql`、`views/units.sql`、`views/history.sql`、`views/unit_changes.sql`、`views/clones.sql` になった。`CREATE VIEW` は作るときに本体を束縛するので、`rust_blob_units` が参照するビューを持つ `views/blobs.sql` を `lang/rust.sql` より先に読む。

### 導出の一般化

前回までの `rust_units` は、`nodes`・`scope_files`・`source_bytes`・`source_lines` を名前で読むビューだった。blob の集合に同じ規則を当てるには、同じ文面を別の入力で実行する必要がある。DuckDB の表マクロは引数に表の名前を受け取り、本体の `query_table(nodes_t)` がその名前の表やビューを読む。DuckDB CLI 2.0.0 での小さな試しで、マクロの中の `WITH … AS MATERIALIZED` が計画の `CTE` として 1 回だけ実行されることを確かめた。`check.py explain` では、`units` の `READ_AST` は 1 のまま、`unit_churn` と `clone_change_consistency` は 2(今のファイルと blob の 1 回ずつ)だった。

マクロの入力は、どちらも `file_id` という列を鍵に持つ。blob の側では、最初は blob id(40 文字)をそのまま `file_id` にした。代役の行で、ライブの `blob_units` は、同じプロセスで `blob_nodes` を数えた後に 22.9 s・ピーク RSS 13.2 GB だった。スナップショットの中の位置(`blob_ord`、整数)を鍵にし、最後に blob id に戻す形にすると、同じ手順で 15.9 s・8.0 GB になった。`rust_nodes` の 3,100 万行が行ごとに文字列の鍵を持たなくなったためと考えているが、内訳は測っていない。

テキストの切り出しの 2 つの規約(バイト列は `MATERIALIZED` の範囲の集合とだけ結合する、範囲は別の段階に置く)は、マクロの本文がそのまま持っている。

### スナップショット

`read_ast` と `read_text` は、束縛のときに定数のファイルの一覧を必要とし、そのファイルが存在していなければならない。そこで `views/blobs.sql` は、読まれたときに `blob_files` を 1 度走査して `blob_snapshot` と `blob_paths` に取る。`00_load.sql` が今のファイルについて行うことと同じである。この走査が `git_changed_blobs` を `since..HEAD` で実行し、無い blob を書き出す。この repo では、ビューのファイルを読む時間が、blob のディレクトリが空のとき 1.06〜1.96 s、書き出し済みのとき 0.93〜1.29 s だった(前回までは前提の確認とファイル集合だけで、測っていない)。範囲に blob が無いときは、存在しないファイル 1 つを渡す(`read_ast` は `ignore_errors`、`read_text` は存在しないファイルに対して 0 行を返した)。新しいコミットを見るには、このファイル(または `refresh.sql`)を読み直す。

### 関数が入るまでの代役

関数が入るまでは、`check.py` に代役を置いて組んだ。`git log HEAD --raw --no-abbrev -M -z --diff-merges=first-parent` から各変更の両側の blob(通常ファイル、その側のパスの拡張子が `languages` にあるもの)を取り、`git cat-file --batch` で `<blob_dir>/<blob_id>.<ext>` に書き、行を TSV にして、同じ名前と引数の表マクロ `git_changed_blobs` をその上に定義した。SQL のファイルは最初から関数を前提に書いた。関数が入った後、代役は `check.py` から消した。

代役と関数の行の違いは、内容の同じ変更(純粋な rename)の扱いだけだった。865e4045 の clone で、代役は 12,594 行、関数は 12,490 行で、差の 104 行は status `R` の 52 件の両側だった。関数は、両側の内容が同じ変更(rename だけ、mode だけ)に行を出さない。異なる blob の数(6,581)は同じだった。関数の行のある `(sha, path)` はすべて hunk を持ち、hunk のある `(sha, path)` はすべて関数の行を持っていた。`unit_changes` は hunk のある変更だけを使うので、行数は代役でも関数でも 40,675 行(865e4045)で同じだった。fixture は、純粋な rename の行が無いことを確かめる形に直した。

## 2. `unit_changes`

定義はスパイクの `history.unit_changes` を 1 始まりの行に直したものである。hunk のある `(sha, path)` ごとに、新しい側の blob の unit で、行の範囲 `[start_line, end_line]` が hunk の新しい範囲 `[new_start, new_start + new_lines - 1]` と交わるものは、古い側に同じ `(kind, qualified_name)` があれば `modified`、無ければ `added` である。古い側の blob の unit で、古い範囲 `[old_start, old_start + old_lines - 1]` と交わり、まだ分類されていないものは、新しい側にもあれば `modified`(`path` の下)、無ければ `removed`(`old_path`、無ければ `path` の下)である。`new_lines = 0`(`old_lines = 0`)の hunk は、その側の何とも交わらない。git の `-U0` では、そのときの開始行は挿入(削除)の直前の行だからである。`(sha, path, unit_kind, unit_qualified_name, change_kind)` の重複は除く。依頼文には「古い側の unit は `old_path`(または `path`)の下に記録」とあったが、スパイクのコードでは `old_path` を使うのは `removed` だけで、`modified` は `path` の下である。スパイクに合わせた。

SQL は、hunk のある `(sha, path)` に `blob_files` を結び、その blob の unit を `MATERIALIZED` の段階 `side_units` に取り、hunk と `(sha, path)` で結合して範囲の交わりで絞る、という形である。hunk 34,808 行と、各側の unit(blob ごとの unit は計 169,717 行)の結合で、キャッシュの上では 0.12〜0.13 s だった。

### スパイクとの一致

スパイクの `history.py`(475599d5 から `target/code-db/spike` に展開)に、git_history の `git_hunks` の行をそのまま与えて `unit_changes` を作り、この層の行と集合として比べた(`target/code-db/scratch/spike_compare.py`、tree-sitter 0.25.2、tree-sitter-rust 0.24.2)。スパイクのコードは、blob を `git cat-file` で読んで tree-sitter で解析し、unit を自分で導く。

| 木 | コミット | hunk(`*.rs`) | この層の行 | スパイクのコードの行 | 片側だけの行 |
| --- | --- | --- | --- | --- | --- |
| a0921826(throwaway の clone) | 1,602 | 34,808(gix) | 40,731 | 40,731(同じ gix の hunk) | 0 / 0 |
| 0df26a53 | 1,589 | 34,694(gix) | 40,506 | — | gix の記録 §12 の「gix の hunk から」40,506 と同じ数 |
| 4ad82864(スパイクの記録の木) | 1,585 | 34,682(gix) | 40,326 | 40,332(git CLI の hunk、スパイク自身の読み方) | スパイクのみ 7 / この層のみ 1 |

a0921826 では、blob の解析も unit の導出も別の実装でありながら、40,731 行がすべて一致した(代役のときの 865e4045 でも 40,675 行で一致した)。4ad82864 では、スパイクのコードに git CLI の hunk(スパイク自身の `history.hunks`)を与えると、スパイクの記録の 40,332 行と 34,690 hunk がそのまま再現された。この層との差のうち、スパイクにだけある 7 行は、gix の記録 §12 が挙げた git 側の誤った帰属と同じ 7 行(901edb40f6 の `impl ToolCallRequest` と `ToolCallRequest.identity`、43efd46a66・9bda1b0109・b3406dfd63・c325cd098d・fed32039eb の各テスト関数)だった。この層にだけある 1 行は、e91103e8af の `crates/horizon-sessiond/tests/latency_probe.rs` の `connect_and_handshake`(`modified`)である。この hunk は git CLI でも gix でも `@@ -142 +141,0 @@` で同じだった。スパイクの tree-sitter はこの関数を 0 始まりの行 131〜155 と読み、削除された 142 行目(1 始まり)はその中にある。それでもスパイクのコードの結果にこの行が無い理由は、突き止めていない。

blob の数も、4ad82864 で 6,558 個と、スパイクの記録の「6,558 blob を parse」と同じだった。

### 今のファイルとの一致

`blob_files` の blob を持つ今のファイルについて、その blob の unit と `units` を、`unit_ord`・`parent_unit_ord`(ファイルの中の位置)を含む全列で比べた。a0921826 の clone では 500 ファイル・8,530 行で、両方向とも 0 行の差だった。unit を持つ今のファイル 521 個のうち残る 21 個は、`crates/horizon-agent/src/config.rs` などで、今の内容がマージのコミットで作られている(この repo のマージは 343 個)。`include_merges` が偽のとき、マージは `blob_files` に行を持たない。fixture では、今のファイルすべての blob が範囲にあり、すべて一致した。

## 3. クエリ

`unit_churn`(throwaway の clone、a0921826)。スパイクの記録の表と同じ順位と値だった。

| unit | kind | loc | commits | スパイクの記録 |
| --- | --- | --- | --- | --- |
| `src/terminal/session.rs` `TerminalSession` | impl | 398 | 46 | 46 |
| `src/workspace/session_lifecycle.rs` `WorkspaceShell` | impl | 490 | 44 | 44 |
| `src/workspace/commands.rs` `WorkspaceShell` | impl | 928 | 42 | 42 |
| `crates/horizon-agent/src/tools/catalog.rs` `definitions` | function | 3 | 42 | 42 |
| `crates/horizon-agent/src/tools/state.rs` `ToolSessionState` | impl | 223 | 37 | 37 |
| `TerminalSession.spawn` | method | 99 | 32 | 32 |
| `WorkspaceShell.execute` | method | 204 | 30 | 30 |

著者は全 unit で最大 2 名で、これもスパイクの記録と同じだった。4ad82864 から a0921826 までに変わった `.rs` は `duckdb-ext/` のファイルだけで、これらの unit には触れていない。git と gix の配置の差の 7 行は、いずれもこの上位の unit ではなかった。

`clone_change_consistency`(片側だけの変更の多い順):

| 側 A | 側 B | A のみ / B のみ / 両側 | スパイクの記録 |
| --- | --- | --- | --- |
| `TerminalSession.spawn`(method) | `src/workspace/mod.rs` `WorkspaceShell`(type、正規化、6 行) | 28 / 17 / 4 | 28 / 17 / 4 |
| `judge/chat.rs` mod `tests` | `judge/mod.rs` mod `tests`(exact、19 行) | 0 / 25 / 1 | 0 / 25 / 1 |
| `Store.project_event` | `tools/explore.rs` `fold_until_terminal`(正規化、28 行) | 11 / 8 / 9 | 11 / 8 / 9 |
| `Provider for MockProvider.start_session` | `mock.rs` `respond_to_user_message`(正規化、17 行) | 15 / 3 / 3 | 15 / 3 / 3 |
| `frame/fold.rs` `apply_agent_event_to_frame` | `Store.project_event`(exact、12 行) | 4 / 12 / 8 | 4 / 12 / 8 |
| `src/keymap.rs` `command_for` | `src/terminal/input.rs` `term_key_code`(正規化、15 行) | 16 / 0 / 0 | 16 / 0 / 0 |

全体の数は次のとおりだった(クローンの対 1,502、側 3,004)。

| 数 | この層 | スパイクの記録 |
| --- | --- | --- |
| 囲む unit が無い側 | 771 | 2,980 側のうち 761 |
| 両側が unit に解決した対 | 1,109(うち両側が別の unit 672) | 437 |
| 両側が同じ unit に解決した対(`same_unit`) | 437 | — |
| 片側のみの変更が 1 回以上ある対 | 505(両側が別の unit のもの 493) | 498 |

スパイクの記録の「両側が unit に解決した対は 437」は、同じ記録の「2,980 側のうち 761 件は囲む unit が無い」と両立しない。1,490 対のうち囲む unit の無い側を持つ対は多くとも 761 なので、両側が解決した対は 729 以上になる。この層では `same_unit` の対がちょうど 437 だった。スパイクの 437 は `same_unit` の数を数えたものかもしれないが、スパイクの測定(worker のもの)の式は残っておらず、確かめられない。側の数(2,980 と 3,004)と対の数の差は、jscpd の報告がスパイクの木と今の木とで違うためである。

## 4. キャッシュ

### 永続キャッシュ(`refresh.sql`)

blob は内容が変わらないので、その解析(`cache.blob_nodes`)と unit(`cache.blob_units`)は blob id だけを鍵にし、行を足すだけにした。表は次の 5 つを足した(キャッシュのスキーマの版を 2 に上げた)。

| 表 | 行 |
| --- | --- |
| `blob_files` | `blob_files` の列。履歴と同じ `since` で追記し、履歴と一緒に作り直す |
| `blob_parsed` | 解析した blob ごとに `blob_id`、`bytes` |
| `blob_derived` | unit を導いた blob ごとに `blob_id`(unit を持たない blob もあるので、`blob_units` とは別に持つ) |
| `blob_nodes` | `blob_id` と `read_ast` の列 |
| `blob_units` | `blob_units` の列 |

更新の流れには、履歴の後に 3 段階を足した。

1. `blob_files`:履歴が作り直されるとき、または全 blob の unit を導き直すときに作り直し、そうでなければ `cached_head..HEAD` の行を追記する。導き直しは全 blob のファイルを読み直すので、作り直し(関数が無いファイルを書き出す)を先に行う。関数は範囲のすべての行を返すので、解析するかどうかは SQL で `blob_id NOT IN cache.blob_parsed` で決める。
2. 解析:`cache.blob_files` の blob のうち `cache.blob_parsed` に無いものに、スナップショットを絞った上で `views/blobs.sql` の `blob_nodes` を通して `read_ast` をかけ、`cache.blob_nodes` に入れる。
3. unit:範囲の blob のうち `cache.blob_derived` に無いものについて、`blob_nodes` をキャッシュに向け、スナップショットをそれらに絞って `derived_blob_units` を計算する。最後にスナップショットを範囲の全 blob に戻し、`blob_files`・`blob_nodes`・`blob_units` をキャッシュを読むビューに替える。範囲から外れた blob(amend の後など)はファイルに残り、ビューには出ない。

`meta` には、`blob_history`(`rebuilt` か `appended`)、`blob_rows_added`、`blobs_parsed`、`blob_bytes_parsed`、`blob_nodes_parsed`、`blobs_derived`、`blob_units_inserted`、`blobs_cached` と、層ごとの時間 `blob_files`・`blob_nodes`・`blob_units` を足した。`history_key` に `blob_dir` を足した。`meta` の列名を最初は `blob_files` にしたが、層ごとの時間の `blob_files` と `check.py` の中で同じ名前になり、測定の集計が壊れたので `blob_history` にした。

導出の定義のキーには、マクロの本文を入れた。unit の規則がマクロに移ったので、ビューの本文だけでは規則の変更を検出できない。前回の記録は、`duckdb_functions()` の読み出しに 18 ms かかるとしてマクロを入れなかった。今回は、内部でないマクロすべての本文を入れた。sitting_duck のマクロ(97 個)も内部でないものとして入るが、その本文が変わるのは版が変わるときで、そのときは構文ごと作り直しになる。前回 README に「検出されない」と書いた `byte_slice`・`normalize_ws` の変更も、これで検出される。`blob_units` は `refresh.sql` が差し替えるビューなので、キーから除いた。除かないと、同じセッションで 2 回目の更新が毎回導き直しになる。

導き直しは、キャッシュの blob のノードから行う。`lang/rust.sql` の変更の後の最初の更新で、6,581 個の blob の unit を 2.3 s で導き直した(代役のときの 1 回の観察)。ライブの解析(13〜19 s)は要らない。

### セッションキャッシュ(`cache.sql`)

`blob_files` と `blob_units` をコピーするようにした。`blob_nodes` は 3,100 万行あるのでコピーせず、ライブのまま残した。`blob_units` のコピーを作るときに、blob の解析が 1 回走る。作成は 17.0〜20.4 s(うち `blob_units` 13.5〜16.4 s)、ピーク RSS 8.8〜9.0 GB になった(前回 2.9〜3.0 s・3.0〜3.1 GB)。

### `unit_changes` を実体化するか

キャッシュの上で、ビューのままの場合と、更新の後に `unit_changes` を一時表に実体化した場合を比べた(新しいプロセスで 3 回、`refresh.sql` の後)。

| 文 | ビュー | 実体化した表 | 実体化そのもの |
| --- | --- | --- | --- |
| `count(*) FROM unit_changes` | 0.12〜0.13 s | 0.00 s | 0.14〜0.18 s |
| `unit_churn` | 0.16〜0.19 s | 0.05 s | 0.14〜0.16 s |
| `clone_change_consistency` | 0.19〜0.21 s | 0.11〜0.13 s | 0.14〜0.20 s |

1 つのクエリでは、実体化の費用とクエリの短縮がほぼ同じである。永続キャッシュに入れるなら、`blob_files` と `hunks` が変わるたびに作り直す(または追記の規則を持つ)ことになる。`unit_changes` はビューのままにした。

## 5. 検査(`check.py fixture`)

前回までの 2 つの fixture に、履歴の fixture を 1 つ足した。最初の fixture の 4 コミットに、次の 6 コミットを足す。

| 段階 | 変更 | 確かめた `unit_changes` |
| --- | --- | --- |
| one_side | `src/shapes.rs` の `Shape for Square.area` の 1 行(クローンの片側) | impl と method が `modified` |
| both_sides | 同じ行を `shapes.rs` と `rect.rs` の両側で(別の著者 `Second`) | 両ファイルの impl と method が `modified` |
| added | `src/lib.rs` の末尾に 1 行の関数 `volume` | `volume` が `added` |
| renamed_edit | `tests/it.rs` を `tests/integration.rs` に移し、関数 `integration` を `integration_area` に | 新しいパスの下に `integration_area` が `added`、古いパスの下に `integration` が `removed` |
| removed | `volume` を消す | `volume` が `removed` |
| pure_rename | `tests/integration.rs` を `tests/area.rs` に移すだけ | 行なし(`blob_files` にも行なし) |

最初の 4 コミットの行(最初のコミットで全 unit が `added`、`Square.new` などの `modified`、README だけのコミットで行なし)も含めて、全行を段階ごとに比べる。3 つ目のコミットで `lib.rs` の末尾に足す `perimeter` は、直前の行と同じ `}` で終わるので、`-U0` の挿入を 1 行上にも置ける。gix は `area` の `}` の後に置き、`area` は触れられない。依頼では、この種の曖昧な挿入を fixture に入れる必要はないとされていた。新しく足したコミットは、どれも曖昧にならないように作った(1 行の関数、行の中身の置き換え)。

ほかに、`blob_files` の側と行数(足したファイルに古い側が無い、rename の `old_path`、純粋な rename に行が無い、ファイル名が `<blob_dir>/<blob_id>.rs`)、異なる blob ごとに解析が 1 回であること、今のファイルすべてで blob の unit が `units` と一致すること、`unit_changes` の行に重複が無いこと、`unit_churn` の値(`Shape for Square.area` が 3 コミット・2 名、rename 後の `tests/area.rs` の関数が 0 コミット)、`since_time` と `kind` の絞り込み、`clone_change_consistency`(対 0 は A のみ 1・両側 2、対 1 は B 側が unit に解決しない)、`clone_source` の絞り込み、`since` で `blob_files` と `unit_changes` が一緒に狭まることを確かめる。

更新の fixture には、blob の統計を足した。空のキャッシュから blob の行 12・解析 7(`shapes.rs` は最初の内容に戻されるので 8 ではなく 7)、新しいコミットで行 2・解析 1(rename は純粋なので行なし)、amend で `blob_files` の作り直し(行 14)と解析 0、中断の後で解析 8、をそれぞれ確かめる。更新の後の各ビュー(blob の層と `unit_changes` を含む 11 ビュー)と 6 つのクエリが、すべての段階でライブと一致する。キャッシュの上では、`clone_site_tags`・`unit_churn`・`clone_change_consistency` の計画に `READ_AST`・`READ_TEXT` が無い。

何も変わっていないときの、同じセッションでの 3 回の読み込みの最短は、前回の 100 ms 未満の条件を満たさなくなった。blob の 3 段階が加わり(合わせて 0.05〜0.07 s)、代役で組んでいた間は機械の負荷も高かった(load average 5〜22)。観測した値は 0.13〜0.30 s で、条件を 500 ms 未満に緩めた。関数での最後の実行では 134〜150 ms だった。

## 6. 測定

この作業ツリーを書き換えないように、throwaway の clone(`target/code-db/clone`、a0921826、`.rs` 526 ファイル)で測った。jscpd の報告はその clone について作った。各回は新しいプロセスで、最小〜最大を記す。

`uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 3`:

| 場合 | wall time | ビューのファイル | 主な層 | ピーク RSS | 行数 |
| --- | --- | --- | --- | --- | --- |
| 空のキャッシュ(blob のディレクトリも消す) | 20.05〜25.79 s | 1.06〜1.96 s | blob_nodes 13.88〜17.57、blob_units 1.76〜2.42、file_changes 1.00〜1.95、hunks 1.07〜1.39、nodes 0.75〜0.86、blob_files 0.51〜0.60、checkpoint 0.34〜0.41 s | 6,745〜6,859 MiB | blob 6,586(152,399,700 バイト)、blob のノード 31,361,613、blob の unit 169,717、blob_files 12,498 |
| 何も変わっていない | 0.24〜0.25 s | 1.05〜1.08 s | files 0.06、blob_units 0.05、units 0.03 s | 344〜354 MiB | すべて 0 |
| 1 ファイルの編集 | 0.31〜0.33 s | 0.93〜1.12 s | | 368〜375 MiB | nodes 4,245、units 39 |
| 10 ファイルの編集 | 0.46〜0.66 s | 1.03〜1.16 s | checkpoint 0.03〜0.25 s | 460〜594 MiB | nodes 29,891、units 174 |
| 3 ファイルを変えた 1 コミット | 0.36〜0.43 s | 0.95〜1.21 s | blob_nodes 0.01〜0.04 s | 393〜411 MiB | commits 1、blob_files 6 |
| amend(履歴の作り直し) | 3.56〜4.17 s | 1.03〜1.29 s | file_changes 1.31〜1.44、hunks 1.31〜1.48、blob_files 0.64〜0.90 s | 356〜422 MiB | commits 1,602、blob_files 12,498(作り直し)、解析 0 |

空のキャッシュの場合、blob のファイルは `refresh.sql` の前に、ビューのファイル(`views/blobs.sql` のスナップショット)を読む時点で書き出される。表の wall time はその後の `refresh.sql` だけで、書き出しは「ビューのファイル」の列に入る。1 コミットの場合、最初の回で 3 つの新しい blob が解析され(`blobs_cached` が 6,586 から 6,589 になった)、2 回目と 3 回目は同じ編集で同じ blob id になるので解析 0 だった。表の「行数」は最後の回の値である。

前回の記録との違いは、空のキャッシュが 3.54〜3.60 s から 20.05〜25.79 s、何も変わっていないときが 0.13〜0.20 s から 0.24〜0.25 s、amend が 2.17〜2.87 s から 3.56〜4.17 s(うち `blob_files` の作り直し 0.64〜0.90 s)である。何も変わっていないときの増分は、blob の 3 段階(0.08 s)と、導出のキーでの `duckdb_functions()` などである。キャッシュファイルは測定の終わりで 616.3 MiB(WAL を含む)だった。

キャッシュからのクエリ(ビューのファイルと `refresh.sql` の後、同じコマンドの続き):

| クエリ | wall time | ピーク RSS(更新を含む) |
| --- | --- | --- |
| `file_cochange` | 0.02 s | 371〜383 MiB |
| `file_cochange_thresholds` | 0.02 s | 364〜392 MiB |
| `clone_site_tags` | 0.08 s | 402〜407 MiB |
| `clone_site_tags_summary` | 0.07〜0.08 s | 395〜403 MiB |
| `unit_churn` | 0.16〜0.19 s | 747〜758 MiB |
| `clone_change_consistency` | 0.19〜0.21 s | 760〜782 MiB |
| `count(*) FROM unit_changes` | 0.12〜0.13 s | 681〜705 MiB |

ライブと `cache.sql` の上(`check.py measure target/code-db/clone --runs 3 --only …`):

| 文 | ライブ | ピーク RSS | `cache.sql` の上 | ピーク RSS(作成を含む) |
| --- | --- | --- | --- | --- |
| `count(*) FROM units` | 0.59〜0.63 s | 1,187〜1,240 MiB | 0.00 s | 8,845〜8,952 MiB |
| `count(*) FROM blob_units` | 18.26〜18.47 s | 8,107〜8,131 MiB | 0.00 s | 8,790〜8,928 MiB |
| `count(*) FROM unit_changes` | 16.80〜20.07 s | 8,365〜8,418 MiB | 0.12〜0.13 s | 8,759〜8,947 MiB |
| `unit_churn` | 14.25〜19.79 s | 8,646〜8,683 MiB | 0.12〜0.19 s | 8,833〜8,932 MiB |
| `clone_change_consistency` | 20.15〜20.49 s | 8,548〜8,725 MiB | 0.14〜0.22 s | 8,874〜8,966 MiB |
| `cache.sql` の作成 | 17.04〜20.44 s | 8,798〜8,973 MiB | | |

ライブの `units` は前回の記録の 0.55〜0.64 s と同じ範囲で、表マクロに移したことによる違いは見えなかった。代役で組んでいた間の同じ測定は 1.27〜1.31 s で、機械の負荷の差と考えている。

スパイクの Python は、履歴(解析を含む)を 15.2 s で作っていた。この層の空のキャッシュからの blob の解析と格納(13.9〜17.6 s)はそれと同じ程度である。`read_ast` だけの時間は、代役のときのライブの `count(*) FROM blob_nodes` で 13.1〜19.0 s(2 回の観察)だった。

`check.py refresh target/code-db/clone` では、1 回目(空のキャッシュ、28.2 s)と 2 回目(何も変わっていない、0.48 s)の両方で、11 ビューと 6 つのクエリがライブと一致した。`check.py cli target/code-db/clone` では、CLI の `.read` と Python で、件数(526・9,073・14,646・34,808・1,502)と 6 つのクエリの行数(403・5・1,502・15・9,073・1,502)がライブと更新後の両方で一致した。

## 7. うまくいかなかったこと・残っていること

- **`check.py refresh` で `blob_nodes` を行ごとに比べると、メモリが足りなくなった。** ライブの 3,100 万行を一時表に写して `EXCEPT ALL` で比べる形で、プロセスのピーク RSS が 31 GB を超え、機械の空きメモリが 5 GB まで下がったので止めた。`blob_nodes` だけは、blob ごとの行数と行のハッシュの和で比べる形にした。fixture でも同じ比べ方である。他のビューは行ごとに比べている。
- **最初の履歴の fixture は、未追跡の `src/scratch.rs` をコミットに含めてしまった。** `git add -A .` を使ったためで、対象のパスを指定する形にした。
- **代役のマクロを CLI に渡すとき、タブの区切り文字が空白に潰れた。** 文を 1 行にするための空白の詰め直しで、`'\t'` の中の実際のタブが消えた。代役は今は無い。
- **何も変わっていないときの 100 ms の条件**は、上のとおり 500 ms に緩めた。
- 今のファイルの blob の一致は、`include_merges` が偽のとき、マージで作られた 21 ファイルについては確かめられていない。`include_merges` を真にした測定はしていない。
- blob のタグは作っていない。`rust_tags` は表マクロにしていない。
- unit の同一性は、パスの中の `(kind, qualified_name)` である。rename で履歴が切れることは fixture の `tests/area.rs`(0 コミット)のとおりで、変えていない。同じ名前の unit が 1 つのファイルに 2 つあると(`impl Foo` が 2 つ)、1 つの unit として数える。
- 1 つの blob が異なる拡張子のパスで記録されると、blob id の鍵が衝突する。言語が 1 つのうちは起きない。
- `blob_dir` のファイルは、unit を導き直すときにだけ読み直す。そのときは先に `blob_files` を作り直し、関数が無いファイルを書き出す。
- 4ad82864 でこの層にだけある 1 行(e91103e8af の `connect_and_handshake`)の理由は分かっていない(2 節)。
- スパイクの記録の「両側が unit に解決した対は 437」は、同じ記録の他の数と両立しない(3 節)。

## コマンド

```sh
duckdb-ext/build.sh
git clone -q . target/code-db/clone
duckdb-ext/sql/jscpd.sh target/code-db/clone target/code-db/clone/target/code-db/jscpd src crates preview-plugin/src
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py cli target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 3
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only unit
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only clone_change
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only "build cache"
```

スパイクとの比較(`target/code-db/scratch/` の使い捨てのスクリプト、コミットしていない):`git archive 475599d5 scripts/code-db` を `target/code-db/spike` に展開し、`count_uc.py` で各 clone の `unit_changes` と `hunks` を TSV に書き、`uv run --with tree-sitter==0.25.2 --with tree-sitter-rust==0.24.2 --with tree-sitter-go==0.25.0 --with tree-sitter-typescript==0.23.2 python -I spike_compare.py SPIKE_DIR CLONE HUNKS_TSV|cli UNIT_CHANGES_TSV` で比べた。0df26a53 と 4ad82864 の clone は、それぞれ `git clone` の後に `git reset --hard` で作った。
