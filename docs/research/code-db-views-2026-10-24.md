# コードデータベースをテーブル関数の上のビューとして組んだ記録(2026-10-24)

2026-10-24、オーナーは [設計文書](../code-database-design.md) の原則(事実はテーブル関数から取り、導出するものはすべてビューにし、実体化はキャッシュであってビルド手順ではない)に「はい。その方針でよいです。」と答えた。この増分の形(ビルド手順なし、Rust の追加なし、既存のテーブル関数の上の SQL ビューだけ)には「はい。お願いします。」と答えた。本記録は、その形で最初の増分を組んだときに分かったことと、この repo でその形にかかる費用の測定である。観察だけを記し、推奨は書かない。

成果物は `duckdb-ext/sql/` で、使い方・規約・パラメータはその `README.md` にある。作業は worker の作業ツリー(`origin/code-quality-survey` の ee399540)で行った。機械は AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB、Linux 7.2.9(Void)で、オーナーの対話用デスクトップを他の作業と共有している。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0(`~/.local/bin/duckdb2`、`v2.0.0 Cyanoptera d54cbbe3df`)である。sitting_duck は `INSTALL sitting_duck FROM community` で入った v1.16.0、git_history はこの作業ツリーで `duckdb-ext/build.sh` を使って作ったものである。jscpd は refactor-audit が固定している 5.3.1 である。

## 主要な観察

- **ビューだけで組めた。** Rust を書き足す必要はなかった。`read_ast`・`read_text`・`read_json`・`glob` と git_history の 3 関数は、`getvariable('…')` を引数として受け付ける(名前つき引数も同じ)。ビューの中に書いても、変数はビューを走査するたびに読まれる。`LOAD` だけは文字列リテラルしか受け付けない。
- **ビューは参照のたびに `read_ast` を最初から実行する。** `units` の計画には `read_ast` の走査が 11 個、`tags` には 16 個、`clone_site_tags` には 17 個ある。走査はそれぞれ 1,533,145 行のすべてを出し、`type` の条件は押し下げられない。オプティマイザは `read_ast` の行数を 1 と見積もっている。
- **ライブのままだと、構文から導くものが遅い。** `count(*)` にかかる時間は、`nodes` が 0.35〜0.55 s、`units` が 6.2 s、`tags` が 12.9〜13.7 s だった。`clone_site_tags` は 14.1〜14.6 s かかった。`nodes` を表に実体化すると(0.63〜0.65 s)、`units` は 0.29〜0.33 s、`tags` は 3.9〜4.0 s、`clone_site_tags` は 4.1〜4.2 s になった。`units` と `tags` も実体化すると(4.4〜4.7 s)、`clone_site_tags` は 0.07〜0.08 s になった。
- **履歴も参照のたびに計算し直す。** 共変更の 2 つのクエリは、ライブで 1.8〜2.5 s だった。`nodes` を実体化しても変わらなかった。`commits` と `file_changes` を実体化すると(0.89〜1.30 s)、0.01〜0.02 s になった。
- **ed63e944 の木では、units と tags が参照値と完全に一致した。** 参照値はスパイクと sitting_duck の評価記録のもので、units 8,878 件、tags 14,414 件である。kind 別・tag 別の件数と、test の付いたノード型の内訳まで一致した。HEAD との差は、その後に足された `duckdb-ext/` の 7 ファイルだけから来ていた。
- **ファイル集合を `glob()` で作る形には、穴が 2 つある。** `.gitignore` を読まないことと、除外の前に木全体を歩くことである。この作業ツリーでは追跡対象の 522 ファイルとちょうど一致し、0.03 s で済んだ。worktree を抱える主チェックアウトでは、`**/*.rs` の glob だけで 20.9 s かかり、14,958 ファイルを返した。

## 1. 組んだもの

| ファイル | 定義するもの |
| --- | --- |
| `00_load.sql` | 2 つの拡張の `INSTALL`・`LOAD`、パラメータの既定値、glob から作る `source_files`・`source_paths` |
| `views/files.sql` | `files(file_id, path, language)`、`sources(file_id, content)`、`source_hex`、`source_lines`、マクロ `byte_slice`・`normalize_ws` |
| `views/nodes.sql` | `nodes`:`file_id` と、`read_ast(…, source := 'full', peek := 'none', context := 'none')` の全列 |
| `lang/rust.sql` | `rust_units`・`rust_tags` と途中のビュー |
| `views/units.sql` | `units`・`tags`(言語ごとのビューの和。今は Rust だけ) |
| `views/history.sql` | `commits`・`file_changes`・`hunks` |
| `views/clones.sql` | `clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b, end_line_b, tokens, lines)` |
| `queries/*.sql` | `file_cochange`・`file_cochange_thresholds`・`clone_site_tags`・`clone_site_tags_summary`(それぞれ 1 文) |
| `jscpd.sh` | 2 つの jscpd レポート(`--absolute`、識別子を保つものと `--ignore-identifiers --ignore-literals`) |
| `check.py` | fixture での検査、repo での集計、CLI との比較、測定(uv script、`duckdb==2.0.0` だけ、`check.py.lock` で固定) |

依頼で示された配置との違いは 2 つある。1 つ目は `views/units.sql` を足したことである。言語ごとのビューの和は、すべての `lang/*.sql` の後で読む必要があるので、どれか 1 つの言語ファイルには置けない。2 つ目は `jscpd.sh` を足したことである。

ファイルは上の表の順に実行する。`views/clones.sql` を実行する時点で jscpd のレポートが存在している必要がある(4 節)。ほかのファイルは、実行してもソースを読まない。この repo で 7 ファイルを実行する時間は、3 回で 0.29〜0.36 s だった。うち `00_load.sql` が 0.18〜0.26 s で、すでに入っている sitting_duck の `INSTALL`・`LOAD` と glob を含む。ビューの作成は、`lang/rust.sql` の 0.07 s が最大だった。

行番号は、すべて 1 始まりで終端を含む `start_line`・`end_line` にした。sitting_duck、git の hunk ヘッダ、jscpd がいずれも 1 始まりだからである。スパイクの 0 始まりの `start_row` 列はなくなった。ノードの `end_line` はノードの終点の行である。そのため、改行まで含む行コメントは、次の行の 1 列目で終わる。units の対象になる item ではこの形にならない。

## 2. パラメータの渡し方

確かめたこと(`target/code-db/scratch/probe*.py`、コミットしていない):

| 書き方 | 結果 |
| --- | --- |
| `read_ast(getvariable('paths'), …)`(リスト)、`read_ast(getvariable('g'), …)`(glob 文字列) | 受け付ける |
| `glob(list_transform(getvariable('include_globs'), lambda g: getvariable('repo') \|\| '/' \|\| g))` | 受け付ける(定数として畳み込まれる) |
| `git_commits(getvariable('repo'))`、`git_file_changes(…, include_merges := getvariable('im'))`、`since := NULL` | 受け付ける。NULL は指定なしと同じに扱われた |
| `read_json(getvariable('rep'), filename := true)` | 受け付ける |
| `CREATE VIEW v AS SELECT … FROM read_ast(getvariable('g'), …)` の後で `g` を変える | 次の `SELECT * FROM v` に反映される |
| `LOAD getvariable('ext')` | `Parser Error: syntax error at or near "("` |
| `SET VARIABLE x = (SELECT list(file) FROM glob(…))` | 受け付ける |
| `read_ast` に空のリスト | `Binder Error: File pattern list cannot be empty` |
| `read_ast` に存在しないパス | `IO Error: … File or directory does not exist` |
| `CREATE VIEW … read_json(<存在しないファイル>)` | 作成の時点で `IO Error: No files found` になる(read_json は型を決めるために作成時にファイルを読む) |
| `CREATE VIEW … read_ast(…)`、`git_commits(…)` | 作成時にはソースを読まない(1.7 M ノード分のパスのリストでも作成は 0.002 s。git のない木でも作成できた) |
| `SET VARIABLE all = …` | 予約語のため構文エラー |
| `list_transform(l, g -> …)` | 2.0.0 では非推奨の矢印構文としてエラー。`lambda g: …` を使う |

そこで、`repo` だけを必須とし(絶対パスでないと `error()` で止める)、ほかの変数は `00_load.sql` で `coalesce(getvariable('x'), <既定値>)` として既定値を入れた。利用者は `00_load.sql` の前でも後でも変数を変えられる。ただし `source_files` と `source_paths` だけは `00_load.sql` が glob から 1 回計算するので、ファイルの追加や削除を拾うには `00_load.sql` を読み直す。ファイルの中身は常にライブに読まれる。クエリのパラメータ(`min_shared` など)は、クエリの中の `params` CTE で `coalesce(getvariable(…), 既定値)` として読む。クエリは状態を変えない 1 文のままである。

`LOAD` が変数を取れないので、git_history のパスは `00_load.sql` に Horizon のチェックアウトからの相対パスで書いた。クライアントは Horizon のチェックアウトの根で起動する。解析する repo は `repo` 変数で決まり、どこにあってもよい。

`repo` を絶対パスに限ったのは、jscpd の `--absolute` の名前と `read_ast`・`read_text` の `file_path` を、同じ接頭辞で repo 相対に直すためである。DuckDB には現在のディレクトリを返す関数がない(CLI の `getenv('PWD')` は CLI でしか使えない)。

## 3. ファイル集合(files)

追跡対象のファイルを返すテーブル関数はまだない。そこで `files` は、`glob()`(既定は `**/*.rs`)の結果から除外パターン(既定は `target/*`、`*/target/*`、`.*`、`*/.*` を GLOB 演算子で適用)を引き、拡張子が `languages`(既定は `MAP {'rs': 'rust'}`)にあるものとした。作業ツリーから読むので、パターンに合う未追跡のファイルも入る。

- この作業ツリーでは 522 ファイルで、`git ls-files '*.rs'` の 522 ファイルと集合として一致した。除外がなければ、`duckdb-ext/target/release/build/*/out/*.rs` の 4 ファイルが加わり、ノードは 231,374 増える(うち 231,237 が bindgen の出力 `bindgen_v2.rs`)。
- DuckDB の `glob()` は、隠しディレクトリにも下りる(fixture で `.hid/x.rs` と `.z.rs` を返した)。歩く範囲を除外で刈り込むこともできない。主チェックアウト `/home/satoshi/src/github.com/rail44/horizon/**/*.rs` の glob は 20.9 s で 14,958 ファイルを返した。これは `.claude/worktrees` の下の各 worktree を含んだ値である。この作業ツリーの `**/*.rs` は 0.03 s だった。
- `file_id` は、整列したリストでの位置(0 始まり)である。リストが変わらない間だけ安定する。

## 4. clone_pairs と read_json

`jscpd.sh` で作ったレポートは、exact が 270 組、normalized が 1,232 組で、どちらも 496 ファイルを解析していた(実行は 2 回で計 0.77 s)。走査したのは `src`、`crates`、`preview-plugin/src` で、スパイクと同じである。

`read_json` はレポートを 1 ファイル 1 行として読み、型を自動で決めた。`duplicates` は `STRUCT(firstFile STRUCT("end" BIGINT, endLoc STRUCT(…), "name" VARCHAR, "start" BIGINT, startLoc STRUCT(…)), format VARCHAR, fragment VARCHAR, isNew BOOLEAN, kind VARCHAR, lines BIGINT, secondFile STRUCT(…), tokens BIGINT)[]`、`statistics` は入れ子の STRUCT になった。手を加える必要があったのは次の 4 点だけである。

- `unnest(duplicates)` と `generate_subscripts(duplicates, 1)` で 1 組 1 行にし、レポート内の順番を保った。
- `end` は予約語なので、`d.firstFile."end"` と引用した。
- `source` のラベルは、`filename := true` で得たパスの、親ディレクトリの名前にした。
- 絶対パスの名前は、`repo || '/'` で始まるものだけを repo 相対にした。

ビューの作成時にレポートを読み(0.015〜0.027 s)、走査のたびにもう一度読む。`count(*) FROM clone_pairs` は 0.02 s だった。スパイクの `group_id`(重なる断片の union-find)は依頼の列にないので作っていない。

## 5. units と tags の件数

スパイクの `rust_units.sql`・`rust_tags.sql`(bf6cbbdf)からの移植で変えたのは次の点である。一時表 `sd_source_hex`・`sd_nonblank` をビュー `source_hex`・`source_lines` にした。`file_path` をすべて `file_id` にした。行を 1 始まりにした。`parent_node_id` を、親の unit の `unit_id`(`parent_unit_id`)に直した。ファイルタグは、ルートのノード(node_id 0)を `files.path` で選ぶようにした。`rust_nodes` は `nodes WHERE language = 'rust'` で、read_ast 自身の言語列を使う。

参照値の木(ed63e944 を `git archive` で `target/code-db/ref-ed63e944` に展開したもの)では次のとおりだった。

| | 参照値(評価記録) | ビュー @ ed63e944 | ビュー @ ee399540(この作業ツリー) |
| --- | --- | --- | --- |
| files | 515 | 515 | 522 |
| nodes | 1,514,979 | 1,514,979 | 1,533,145 |
| units | 8,878 | 8,878 | 8,982 |
| function | 4,795 | 4,795 | 4,841 |
| method | 1,821 | 1,821 | 1,846 |
| impl | 556 | 556 | 562 |
| type | 1,002 | 1,002 | 1,027 |
| module | 697 | 697 | 699 |
| macro | 7 | 7 | 7 |
| tags | 14,414 | 14,414 | 14,505 |
| generated_or_macro | 11,179 | 11,179 | 11,253 |
| test | 3,021 | 3,021 | 3,033 |
| trait_impl | 214 | 214 | 219 |

test の内訳(ノード型別)は、ed63e944 では評価記録の値(function_item 2,608、mod_item 272、source_file 64、use_declaration 43、const_item 18、struct_item 11、impl_item 3、type_item 1、enum_item 1)と同じだった。ee399540 では function_item 2,617、mod_item 273、source_file 66 で、ほかは同じだった。

ed63e944 と ee399540 の差を、ファイルと kind・tag ごとに比べた(`target/code-db/scratch/refdiff.py`)。差のあるファイルは、units が 6、tags が 5 で、すべて ed63e944 の後に足された `duckdb-ext/` の 7 ファイルに含まれていた(最大は `duckdb-ext/git-history/src/functions.rs` の units +41)。既存のファイルで件数の変わったものはなかった。ed63e944 から ee399540 までに変わった Rust ファイルも、その 7 つだけである。

## 6. 測定

コマンドは `uv run --locked --script duckdb-ext/sql/check.py measure <この作業ツリー> --runs 3` である。各段階を新しいプロセスで 3 回実行し、最小〜最大を記す。時間は、7 ファイルを実行した後の 1 文だけの wall time である。前準備(キャッシュの作成)は時間に入れていない。ピーク RSS は `resource.getrusage(RUSAGE_SELF).ru_maxrss` で、読み込みと前準備を含むプロセス全体の値である。ページキャッシュは温まっている。他の作業と共有している機械での値である。

### ライブ(何も実体化しない)

| 文 | 行数 | wall time | ピーク RSS |
| --- | --- | --- | --- |
| `SELECT count(*) FROM nodes` | 1,533,145 | 0.35〜0.55 s | 649〜651 MiB |
| `… FROM units` | 8,982 | 6.22〜6.23 s | 1,914〜2,000 MiB |
| `… FROM tags` | 14,505 | 12.91〜13.72 s | 1,763〜1,817 MiB |
| `… FROM hunks` | 34,775 | 0.94〜1.40 s | 267〜273 MiB |
| `… FROM clone_pairs` | 1,502 | 0.02 s | 154〜155 MiB |
| `file_cochange` | 400 行 | 2.33〜2.47 s | 357〜392 MiB |
| `file_cochange_thresholds` | 5 行 | 1.80〜2.34 s | 361〜395 MiB |
| `clone_site_tags` | 1,502 行 | 14.10〜14.61 s | 1,416〜1,435 MiB |
| `clone_site_tags_summary` | 15 行 | 14.69〜15.05 s | 1,333〜1,453 MiB |

### 参照のたびに read_ast が走るか

`check.py explain` で `EXPLAIN` の計画にある `READ_AST` を数えた。結果は、`nodes` 1、`units` 11、`tags` 16、`clone_site_tags` と `clone_site_tags_summary` 17、共変更の 2 つ 0 である。`READ_TEXT` は、`units` に 4、`tags` に 9 あった。`PRAGMA enable_profiling = 'json'` で実行時のプロファイルも取った(`target/code-db/scratch/profile.py`)。`units` では 11 の `READ_AST` 走査がそれぞれ 1,533,145 行を出し、見積もり行数はどれも 1、各走査の `timing` は 7.9〜9.5 だった(スレッドを合計した値とみられる)。`tags` では 16 走査で、どれも 1,533,145 行だった。したがって、`read_ast` は 1 クエリにつき 1 回ではなく、ビューの参照 1 つにつき 1 回、全ファイルを読み直して解析している。`rust_nodes` を参照する場所ごとに、別々の走査になる。

一方、同じ部分問い合わせを 2 回書いた `SELECT (SELECT count(*) FROM nodes), (SELECT count(*) FROM nodes)` では、計画の `READ_AST` は 1 つで、時間も 0.49〜0.57 s だった。全く同じ部分木はオプティマイザが 1 つにまとめる。だから、この形の時間を比べても、参照ごとに走るかどうかは確かめられない。

`git_file_changes` も同じで、`file_cochange` の計画には `GIT_FILE_CHANGES` の走査が 2 つ、`GIT_COMMITS` が 1 つある。

### nodes を実体化した場合

前準備は `CREATE TABLE nodes_cache AS SELECT * FROM nodes; CREATE OR REPLACE VIEW nodes AS SELECT * FROM nodes_cache;` である。上のビューはすべて `nodes` を名前で参照しているので、`nodes` を差し替えるだけで units と tags もキャッシュの上で動く。

| 文 | wall time | ピーク RSS |
| --- | --- | --- |
| `nodes_cache` の作成 | 0.63〜0.65 s | 1,152〜1,183 MiB |
| `count(*) FROM units` | 0.29〜0.33 s | 2,431〜2,507 MiB |
| `count(*) FROM tags` | 3.92〜4.03 s | 2,031〜2,136 MiB |
| `file_cochange` | 2.00〜2.22 s | 1,283〜1,368 MiB |
| `file_cochange_thresholds` | 2.04〜2.16 s | 1,341〜1,416 MiB |
| `clone_site_tags` | 4.09〜4.21 s | 2,340〜2,628 MiB |
| `clone_site_tags_summary` | 4.12〜4.15 s | 2,432〜2,579 MiB |

`tags` には、キャッシュの上でもまだ約 4 s かかる。評価記録では、同じ tags のビューを表の上で 1.24 s で実行していた。原因は切り分けていない。違いは 2 つある。`source_hex` が今回は `read_text` のライブなビューであることと、ノード表が `language` で絞ったビューを経由することである。

### units と tags も実体化した場合

前準備は上に加えて、`CREATE TABLE units_cache AS SELECT * FROM units`、`tags_cache` と、`units`・`tags` の差し替えである。

| 文 | wall time | ピーク RSS |
| --- | --- | --- |
| `units_cache`・`tags_cache` の作成(`nodes_cache` の上) | 4.40〜4.73 s | 2,629〜2,688 MiB |
| `file_cochange` | 2.40〜2.49 s | 2,541〜2,690 MiB |
| `file_cochange_thresholds` | 2.14〜2.36 s | 2,565〜2,669 MiB |
| `clone_site_tags` | 0.07〜0.08 s | 2,491〜2,842 MiB |
| `clone_site_tags_summary` | 0.07〜0.08 s | 2,700〜2,796 MiB |

### 履歴を実体化した場合

前準備は、`commits_cache`・`file_changes_cache` の作成と、`commits`・`file_changes` の差し替えである(`check.py measure … --only history`、`--only "build commits"`)。

| 文 | wall time | ピーク RSS |
| --- | --- | --- |
| `commits_cache`・`file_changes_cache` の作成 | 0.89〜1.30 s | 268〜270 MiB |
| `file_cochange` | 0.01〜0.02 s | 279〜283 MiB |
| `file_cochange_thresholds` | 0.01〜0.02 s | 271〜283 MiB |

### 層ごとの整理

測定から読めることを、層ごとに並べる。

- `nodes` そのもの、`hunks`、`clone_pairs` の 1 回の走査は 1.4 s 以下である。ライブのまま 1 回参照する分には、この値で済む。
- `units`・`tags` と、それらを使うクエリは、ライブでは 6〜15 s かかる。これは `read_ast` を参照の数だけ(11〜17 回)実行するためである。`nodes` を実体化すると、`units` は 0.3 s、`tags` を含むものは約 4 s になる。`units`・`tags` まで実体化すると、クローン位置のクエリは 0.1 s を切る。
- 共変更のクエリは、構文のキャッシュでは速くならない。`git_file_changes` を 2 回計算するのにかかる約 2 s が、そのまま残る。`commits`・`file_changes` を実体化すると、0.02 s 以下になる。
- ピーク RSS は、ライブの `units`・`tags` で 1.8〜2.0 GiB、すべてを実体化したプロセスで 2.5〜2.8 GiB だった(後者は作成時のピークを含む)。

## 7. 検査

- `check.py fixture`:一時ディレクトリに git リポジトリを作る。中身は、trait 1 つ、trait impl 2 つ、インラインのテストモジュール、`tests/it.rs`、未追跡のファイル 1 つ、除外されるべき `target/debug/build/out.rs` と `.hidden/x.rs`、4 コミット(うち 1 つは README だけを変える)である。jscpd 5.3.1 の JSON の形をしたレポート(2 組)も作る。そのうえで、`files`、`units`(kind・qualified_name・行・loc・親)、`tags`、履歴の 3 ビュー(`hunks` が `.rs` だけであること、`file_changes` が README を含むこと)、`clone_pairs`、4 つのクエリ(contain と overlap の両方、`clone_source` による絞り込み)を、期待値と完全に照合する。`since` を変えると `commits` が変わること、ファイルを編集すると読み直さずに `units` に反映されることも確かめる。結果は `fixture: ok` だった。
- `check.py repo .`:この作業ツリーで、全ビューの件数と 4 つのクエリを実行する。結果は file_cochange 400 行、thresholds 5 行(10 % で max 基準 1,503 組・average 基準 1,809 組、20 % で 791・1,125、30 % で 400・632)、clone_site_tags 1,502 行、summary 15 行だった。
- `check.py cli .`:同じ 7 ファイルを CLI 2.0.0 の `.read` で実行する(`duckdb2 -unsigned -no-agent -batch`)。件数(files・units・tags・hunks・clone_pairs と、4 クエリの行数)は Python と一致し、`cli: ok` だった。stdout が端末でなく、環境変数 `AI_AGENT` があるとき、CLI 2.0.0 は「agent mode」になり、結果を Markdown の表で出す。`-no-agent` でこれを止めた。
- Rust の `duckdb` クレートからの実行は試していない。

## 8. うまくいかなかったこと・まだないもの

- 追跡対象ファイルの関数がないので、`.gitignore` は効かない。主チェックアウトでは `include_globs` を絞らないと、glob だけで 20 s かかる(3 節)。
- `LOAD` は変数を取れない。git_history のパスは固定の相対パスである(2 節)。
- `read_json` はビューの作成時にレポートを読む。レポートがない repo では `clones.sql` を実行できない。
- `read_ast` に空のリストを渡せない。対象のファイルが 0 個の repo では、`nodes` を使うビューが失敗する。
- 依頼の範囲外のものは、作っていない。コミットごとの unit の変化(過去の blob が必要。設計文書の未決事項)、SCIP、Go・TypeScript、clone の group_id、移植した 4 つ以外のクエリである。
- 評価記録の端の場合の fixture(`target/code-db/fixture-repo`、負の impl や各種の cfg(test) など 12 ファイル)は、この作業ツリーにはない。そのため、今回のビューでは流していない。ed63e944 の全件一致と、`check.py fixture` の小さな fixture で確かめた。
