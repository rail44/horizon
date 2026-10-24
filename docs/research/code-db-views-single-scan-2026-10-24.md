# 構文ビューを 1 回の走査にし、セッションキャッシュを足した記録(2026-10-24)

2026-10-24、オーナーは、コードデータベースの SQL 層の次の作業を 4 つ並べた提案に「その順で作業してみましょう」と答えた。承認された順は、D(構文ビューの 1 クエリを `read_ast` 1 回の走査にし、表の上でも `tags` が遅い理由を調べる)→ 追跡対象ファイル → キャッシュ A → C である。本記録は、そのうち D と A を扱う。追跡対象ファイルのテーブル関数(`git_tracked_files`)は別の worker が並行して作っており、`views/files.sql` のファイル集合の仕組みには手を入れていない。C は扱っていない。観察だけを記し、推奨は書かない。

前提は [設計文書](../code-database-design.md) の原則(事実はテーブル関数から取り、導出はビューにし、実体化はキャッシュに限る)と、最初の増分の記録 [code-db-views-2026-10-24.md](code-db-views-2026-10-24.md)(以下「前回の記録」)である。作業は worker の作業ツリー(`origin/code-quality-survey` の 2a0de30a から)で行った。機械は前回と同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している(測定中の load average は 7〜10)。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0、sitting_duck は v1.16.0、git_history はこの作業ツリーで `duckdb-ext/build.sh` を使って作ったもの、jscpd は 5.3.1 である。Rust は変えていない。

## 主要な観察

- **ビューの中の `WITH … AS MATERIALIZED` は、DuckDB 2.0.0 で守られた。** ビューを別のビューから参照しても、外側のクエリから参照しても、計画の `READ_AST` は 1 つだった。`units`・`tags`・`clone_site_tags`・`clone_site_tags_summary` の `READ_AST` は、11・16・17・17 から、すべて 1 になった。
- **形は、1 つのビューを 1 つの文にすることだった。** `rust_units` と `rust_tags` は、それぞれ `nodes` を 1 度だけ `MATERIALIZED` の共通表式 `rust_nodes` に読み、途中の段階(以前の中間ビュー)をすべて同じ文の共通表式として組む。中間ビューは独立したビューとしては無くなった。
- **`tags` に `start_line`・`end_line` を足し、クローン位置の 2 つのクエリはそれを使うようにした。** `tags` と `nodes` を結合すると、`nodes` の 2 回目の走査になるためである。
- **ライブの時間は、`units` が 5.8 s から 0.7 s、`tags` が 12.5〜13.3 s から 1.0 s、`clone_site_tags` が 13.3〜13.8 s から 1.0〜1.2 s になった。** 行は、units 8,982 件・tags 14,505 件と 4 つのクエリの全行が、変更前と完全に一致した。
- **表の上の `tags` が約 4 s かかった原因は、`rust_tag_idents` の文字の切り出しだった。** この段階は `tags` の中で 5 回展開され、1 回ごとに約 1.4〜1.6 s かかっていた(ピーク RSS 9.3 GB)。`source_hex` を表にすると、同じ段階が 0.03 s、`tags` が 0.19〜0.20 s になった。計画では、`hex(content)` が結合の上の射影に置かれ、識別子 1 行ごとにファイル全体の 16 進文字列が作られていた。
- **組み直しの途中で、計画の結合順によって `units` のピーク RSS が 7.6 GB(ライブ)、44 GB(ノード表の上)になった。** ファイルのバイト列を、そのファイルの全ノードと先に結合する計画だった。バイト列との結合を、`MATERIALIZED` にした小さな段階に分けて解消した。
- **キャッシュは `cache.sql` で、一時表を作り、同じ名前のビューを差し替える形にした。** 作成は 2.9〜5.3 s(機械の負荷で揺れた)、ピーク RSS 3.0〜3.2 GB だった。キャッシュの上では、クローン位置のクエリが 0.11〜0.24 s、共変更のクエリが 0.02〜0.09 s だった。

## 1. 1 回の走査にした形(D)

### MATERIALIZED がビューの中で守られるか

`target/code-db/scratch/t1.sql` で、`WITH n AS MATERIALIZED (SELECT * FROM nodes)` を本体に持ち、`n` を 2 回参照するビューを 2 つ作った(2 つのスカラー部分問い合わせと、`n` どうしの結合)。どちらも計画の `READ_AST` は 1 つで、0.45〜0.46 s だった。ビュー `w AS SELECT * FROM v` のように入れ子にしても、走査は 1 つのままだった。`EXPLAIN ANALYZE` では、共通表式の演算子は `CTE` と表示され、`Execution Mode: STREAMING_FANOUT` だった。

試した候補のうち、表マクロに節点の集合を引数として渡す形と、`units` と `tags` を 1 つの文で作る形は、上の形で足りたので試していない。

### 変えたこと

- `lang/rust.sql`:`rust_units` と `rust_tags` を、それぞれ 1 つの `CREATE VIEW … AS WITH … SELECT` にした。どちらも `rust_nodes AS MATERIALIZED (SELECT 必要な列 FROM nodes WHERE language = 'rust')` から始まる。以前の中間ビュー(`rust_unit_candidates`、`rust_unit_name_nodes`、`rust_units_unnested`、`rust_tag_idents`、`rust_token_tree_named`、`rust_cfg_test_trees`、`rust_attribute_kinds`、`rust_attribute_runs`)は、同じ名前の共通表式になった。2 回以上参照するものと、文字を切り出すものは `MATERIALIZED` にした。`source_lines` も `lines AS MATERIALIZED` として 1 度だけ読む。
- `rust_tags` の出力に、タグの付いたノードの `start_line`・`end_line` を足した。`queries/clone_site_tags.sql` と `queries/clone_site_tags_summary.sql` の `tagged` は、`tags JOIN nodes` をやめて `tags` の列を読む。依頼では、クエリの変更は列が足りない場合に限っていた。`nodes` との結合を残すと、クエリの走査は 2 になる。
- `views/files.sql`:`source_hex` を `source_bytes(file_id, bytes)`(`encode(content)` の BLOB)に替えた。`byte_slice(bytes, s, e)` は `decode(bytes[s + 1:e])` になった(BLOB の添字はバイト単位で、1 始まり・両端を含む)。理由は 2 節にある。ファイル集合の部分(`files`、`sources`)は変えていない。

### 計画の証拠

`check.py explain .` の計画中の演算子の数:

| 文 | READ_AST 前 | READ_AST 後 | READ_TEXT 前 | READ_TEXT 後 |
| --- | --- | --- | --- | --- |
| `count(*) FROM nodes` | 1 | 1 | 0 | 0 |
| `count(*) FROM units` | 11 | 1 | 4 | 2 |
| `count(*) FROM tags` | 16 | 1 | 9 | 1 |
| `clone_site_tags` | 17 | 1 | 9 | 1 |
| `clone_site_tags_summary` | 17 | 1 | 9 | 1 |
| 共変更の 2 つ | 0 | 0 | 0 | 0 |

`units` の `READ_TEXT` が 2 なのは、`source_bytes` と `source_lines` がそれぞれ `sources` を読むからである。`read_text` の 1 回の走査は 0.01 s 前後だった。実行時のプロファイル(`PRAGMA enable_profiling = 'json'`、`target/code-db/scratch/rss.py`)でも、`units`・`tags` の `READ_AST` の `TABLE_SCAN` は 1 つで、1,533,145 行を出していた。

### 結果が変わらないこと

`target/code-db/scratch/compare.py` で、変更前のファイル(2a0de30a の `duckdb-ext/sql` を `target/code-db/old` に展開)と変更後のファイルの結果を、この作業ツリーで比べた。`units` の全列 8,982 行、`tags`(変更前は `nodes` と結合して行を足したもの)14,505 行、4 つのクエリの全行、`match_mode = 'overlap'` の `clone_site_tags` 1,502 行が、すべて一致した。

### うまくいかなかったこと:結合順

最初に組んだ `rust_units` は、名前のノードと `source_bytes` と単位を同じ段階で結合していた。ライブの `count(*) FROM units` は 1.4〜1.8 s だったが、ピーク RSS が 7.6 GB あった。プロファイルでは、`file_id = file_id` だけを条件とする `HASH_JOIN` が 55,024,931 行(中間 4.8 GB)を出していた。`nodes` を表にした上では、同じビューが 10.9〜17.5 s、ピーク RSS 43.6〜44.4 GB になった。計画(`target/code-db/scratch/plan.py`)では、`rust_nodes` 全体(1,533,145 行)と `source_bytes` を `file_id` だけで先に結合していた。見積もり行数は、共通表式を重ねるたびに大きくずれていた(実際は数千行の段階が数億〜数兆行と見積もられた)。

名前のノードの範囲を `rust_unit_name_spans AS MATERIALIZED` に取り出し、それと `source_bytes` だけを結合する `rust_unit_names AS MATERIALIZED` を別の段階にした。`MATERIALIZED` の共通表式の境界は、結合順の並べ替えを越えさせなかった。これで、ライブ 0.58〜0.77 s・ピーク RSS 1.1 GB、表の上 0.13 s・1.9 GB になった。`rust_tag_idents` も同じ形(`rust_tag_ident_spans` を分ける)にした。この規則は README の規約に書いた。

## 2. 表の上でも tags が遅かった理由

前回の記録では、`nodes` を表にしても `tags` に約 4 s かかり、評価記録の 1.27 s との差は切り分けていなかった。違いの候補は、`source_hex` がライブのビューであること、ノードが `language` で絞ったビューを経由すること、評価記録では `rust_tag_idents` と `sd_source_hex` が一時表だったこと(`git show bf6cbbdf:scripts/code-db/views/rust_tags.sql`、`rust_units.sql`)だった。変更前のファイルで、`nodes_cache` を作った後に 1 つずつ実体化して測った(新しいプロセスで 3 回、`target/code-db/scratch/tags_old.sh`)。

| 変更前のファイル、`nodes` は表 | wall time | ピーク RSS |
| --- | --- | --- |
| `count(*) FROM tags` | 3.95〜4.18 s | 2,063〜2,178 MiB |
| `rust_tag_idents` の `count(*), sum(length(text))` | 1.38〜1.64 s | 9,247〜9,298 MiB |
| `source_hex` も表にしたときの `tags` | 0.19〜0.20 s | 1,742〜1,809 MiB |
| 同じく `rust_tag_idents` | 0.03 s | 1,211〜1,235 MiB |
| `rust_tag_idents` を表にしたときの `tags` | 0.14 s | 9,278〜9,460 MiB(表の作成を含む) |

`text` を読まない `count(*) FROM rust_tag_idents` は 0.02 s だった。中間ビューの `count(*)` は、`rust_cfg_test_trees` 1.53 s(`READ_TEXT` 3)、`rust_attribute_kinds` 2.50 s(同 5)、`rust_tags` 4.21 s(同 9)で、`rust_tag_idents` を参照する数とともに増えた。`rust_tag_idents` は 98,950 行で、`tags` の中で 5 か所から参照される。したがって約 4 s の大部分は、識別子の文字を切り出す段階を 5 回計算する時間だった。`language` による絞り込みは効いていなかった。

切り出しを、`target/code-db/scratch/slice*.py` で単独に測った(識別子 98,950 個の位置を表にしたうえで)。

| 切り出し方 | wall time |
| --- | --- |
| 表に保存した `hex` 列の `substring` | 0.015〜0.018 s |
| ライブの `source_hex` ビューの `substring`(変更前の形) | 3.36〜3.42 s |
| 表に保存した `content` から、行ごとに `hex(content)` して `substring` | 1.47〜1.72 s |
| `MATERIALIZED` の共通表式に `hex` を作ってから `substring` | 1.53 s |
| `content` の `substring`(コードポイント単位、参考) | 1.06〜1.10 s |
| `encode(content)` の BLOB 添字 `[s + 1:e]`(表から / ライブの `sources` から) | 0.22 / 0.23 s |
| ライブの `read_blob` の BLOB 添字 | 0.29 s |

ライブの `source_hex` の計画では、`hex(content)` が `file_id` の結合の上の射影(`length(decode(unhex("substring"(hex(content), …))))`)に置かれていた。つまり、識別子 1 行ごとに、そのファイル全体を 16 進にしていた。共通表式で 16 進を先に作っても、`substring` は遅いままだった。合成データ(1 MB の ASCII 文字列 1 行と 10 万の位置)では、表に保存した列の `substring` は 0.009 s、計算した値の `substring` は開始位置を 0〜50 に限ると 0.022 s、開始位置を文字列全体に散らすと 16.6 s だった。計算した文字列の `substring` は、開始位置までの長さに比例して遅くなる。保存した列だけが速い理由は、DuckDB のコードを読んで確かめてはいない(列の統計で ASCII だけと分かる場合に別の実装を使う、という仮説は検証していない)。

BLOB の添字は、ファイルの大きさに比例した(1 MB の BLOB から 10 万回切り出すと 10.9 s、100 B の BLOB では 0.022 s)。この repo のファイルの大きさでは、識別子 98,950 個で 0.22 s である。大きなファイルが多い repo では、この費用が大きくなる。

直したことは 2 つである。1 つ目は、`rust_tag_idents` を `MATERIALIZED` にして 1 回だけ計算するようにしたこと。2 つ目は、切り出しを BLOB の添字に替えたこと。後者は `source_hex` を表にする前提に頼らずにライブで速く、16 進の倍の長さも作らない。変更後の `tags` は、ライブで 1.00〜1.06 s、`nodes` が表の上で 0.27〜0.29 s(ピーク RSS 2.5 GB)だった。sitting_duck の内部の問題ではなかった。

## 3. セッションキャッシュ(A)

`duckdb-ext/sql/cache.sql` は、ビューのファイルの後に読む。層ごとに `DROP TABLE IF EXISTS <view>_cache; CREATE TEMP TABLE <view>_cache AS SELECT * FROM <view>; CREATE OR REPLACE VIEW <view> AS SELECT * FROM <view>_cache;` を、`nodes`・`units`・`tags`・`commits`・`file_changes`・`hunks` の順に実行する。上のビューは下の層を名前で参照するので、`units` と `tags` は `nodes_cache` から作られる。クエリのファイルは変えずに、そのままキャッシュを読む。

仕組みを選ぶ前に、DuckDB 2.0.0 の名前解決を確かめた。

| 試したこと | 結果 |
| --- | --- |
| ビュー `x`(main)と、同じ名前の一時表 `x` があるとき `SELECT * FROM x` | 一時表が返る。`x` を参照する別のビューからも一時表が返る |
| その後で `CREATE OR REPLACE VIEW x …` | 一時表が返るまま(隠れたビューが置き換わるだけ) |
| データベースファイルで、ビュー `x` を一時表 `x_cache` に差し替えて閉じ、開き直して `SELECT * FROM x` | `Catalog Error: Table with name x_cache does not exist!` |

同じ名前の一時表で影にする形では、ビューのファイルを読み直してもライブに戻らない。そこで、別名の一時表とビューの差し替えにした。ビューのファイルを読み直すと、ビューがライブの定義に戻る。キャッシュを作り直すには、ビューのファイルを読み、続けて `cache.sql` を読む。ビューのファイルを挟まずに `cache.sql` を 2 回読むと、最初の `DROP TABLE` がまだ `nodes` の読んでいる表を消すので、`Catalog Error` で止まる。コピーのコピーを黙って作るよりよいと考えて、この振る舞いをそのままにした(fixture で確かめている)。データベースファイルを使う場合は、差し替えたビューが残り、次のセッションでは一時表が無い。どのクライアントも、セッションごとにビューのファイルを読む前提である。

キャッシュを読んだ時点で固定されるもの:ソースファイルの構文と文字(編集は見えない)、`languages`、その時点の `since`・`include_merges` での履歴。クエリのパラメータ(`min_shared`、`match_mode` など)は、クエリのたびに読まれる。`00_load.sql` を読み直すと `file_id` が振り直されるので、その後はキャッシュを作り直す必要がある。

`hunks` もキャッシュに含めた。今のクエリは `hunks` を読まないが、含めないと、`since` を変えた後に `commits`・`file_changes` は固定のまま `hunks` だけがライブになり、3 つの履歴ビューが別の範囲を表す。費用は作成時間の約 0.9〜2.0 s で、作成全体の約 3〜4 割にあたる。

`check.py` には次を足した。`measure` は各文をライブとキャッシュの上の両方で測り、`build cache` で `cache.sql` を層ごとに測る。前回の記録にあった `nodes_cache` だけ、`units_cache` まで、などの段階的な前準備の測定は除いた。`fixture` は、ライブの検査の後で `cache.sql` を読み、ノード・units・tags・履歴の 3 ビュー・4 つのクエリの全行がライブと一致すること、`clone_site_tags` の計画に `READ_AST`・`READ_TEXT` が無いこと、ファイルの編集と `since` の変更がキャッシュ中は見えないこと、`cache.sql` の 2 回目の読み込みが失敗すること、ビューのファイルを読み直すとライブの行に戻ること、作り直したキャッシュが新しい内容を持つことを確かめる。

## 4. 測定

コマンドは `uv run --locked --script duckdb-ext/sql/check.py measure <この作業ツリー> --runs 3` である(変更前は `target/code-db/old` に展開した 2a0de30a の `check.py` で同じことをした)。前回と同じく、各段階を新しいプロセスで 3 回実行し、最小〜最大を記す。時間は、ビューのファイル(キャッシュの列では `cache.sql` も)を実行した後の 1 文だけの wall time である。ピーク RSS はプロセス全体の `ru_maxrss` で、キャッシュの列ではキャッシュの作成を含む。

### 構文から導くもの

| 文 | 変更前 ライブ | 変更後 ライブ | 変更後 キャッシュの上 |
| --- | --- | --- | --- |
| `count(*) FROM nodes` | 0.43〜0.48 s / 614〜688 MiB | 0.52〜0.62 s / 617〜641 MiB | 0.00 s / 3,054〜3,172 MiB |
| `count(*) FROM units` | 5.79〜5.86 s / 1,784〜2,008 MiB | 0.70〜0.77 s / 1,109〜1,132 MiB | 0.00 s / 3,042〜3,158 MiB |
| `count(*) FROM tags` | 12.54〜13.26 s / 1,749〜1,770 MiB | 1.00〜1.06 s / 1,249〜1,279 MiB | 0.00 s / 3,047〜3,162 MiB |
| `clone_site_tags` | 13.31〜13.80 s / 1,431〜1,516 MiB | 0.96〜1.20 s / 1,228〜1,313 MiB | 0.11〜0.24 s / 3,027〜3,204 MiB |
| `clone_site_tags_summary` | 13.53〜14.01 s / 1,416〜1,454 MiB | 0.94〜1.16 s / 1,232〜1,301 MiB | 0.11〜0.24 s / 3,091〜3,207 MiB |
| `count(*) FROM clone_pairs` | 0.02〜0.03 s / 153〜156 MiB | 0.04〜0.05 s / 148〜150 MiB | 0.05〜0.07 s(ライブのまま) |

変更後のクローン位置のクエリの幅は、2 回の測定(全段階を通したもの、負荷の低い時間に段階を絞ったもの)を合わせた値である。変更前の、すべてキャッシュした前準備(`nodes_cache`・`units_cache`・`tags_cache`)の上では、クローン位置の 2 つは 0.13〜0.14 s、`nodes_cache` だけの上では `units` 0.30〜0.32 s、`tags` 4.00〜4.22 s、クローン位置 4.14〜4.23 s だった。

### 履歴

| 文 | 変更前 ライブ | 変更後 ライブ | 変更後 キャッシュの上 |
| --- | --- | --- | --- |
| `count(*) FROM hunks` | 1.39〜2.28 s | 0.95〜2.34 s | 0.00 s |
| `file_cochange` | 2.03〜4.60 s | 4.04〜4.50 s | 0.02〜0.09 s |
| `file_cochange_thresholds` | 2.58〜4.34 s | 1.77〜4.70 s | 0.02〜0.08 s |

履歴のビューとクエリは変えていない。それでも、測定の回によって約 2 倍の幅が出た(同じ変更前のファイルで 2.03〜2.96 s の回と 4.00〜4.60 s の回があった)。機械を共有している他の作業の負荷とみられるが、切り分けてはいない。ピーク RSS は、ライブで 270〜396 MiB、キャッシュの上では作成を含んで 2,988〜3,262 MiB だった。`file_cochange` の計画には、前回と同じく `GIT_FILE_CHANGES` の走査が 2 つある(D の対象外として変えていない)。

### キャッシュの作成

| | 1 回目の測定 | 2 回目の測定 |
| --- | --- | --- |
| `cache.sql` 全体 | 5.23〜5.29 s | 2.86〜2.94 s |
| nodes | 0.68〜0.73 s | 0.62〜0.63 s |
| units(`nodes_cache` の上) | 0.16 s | 0.13〜0.14 s |
| tags(`nodes_cache` の上) | 0.28〜0.32 s | 0.27〜0.28 s |
| commits | 0.03 s | 0.01〜0.02 s |
| file_changes | 2.00〜2.03 s | 0.88〜0.95 s |
| hunks | 2.02〜2.07 s | 0.93〜0.94 s |
| ピーク RSS | 3,034〜3,054 MiB | 3,056〜3,184 MiB |

units と tags の作成は、変更前の 4.36〜5.47 s(前回の記録では 4.40〜4.73 s)から 0.40〜0.48 s になった。作成全体の時間の大半は、`git_file_changes` と `git_hunks` の 2 回の履歴の計算で、その時間は機械の負荷で揺れた。ピーク RSS の約 3 GB は、`nodes_cache`(read_ast の全列、1,533,145 行)を含むプロセス全体の値である。

## 5. 検査

- `check.py fixture`:`fixture: ok`(3 節のキャッシュの検査を含む)。
- `check.py repo .`・`check.py cli .`:files 522、units 8,982、tags 14,505、hunks 34,775、clone_pairs 1,502、クエリの行数 400・5・1,502・15 が Python と CLI で一致し、`cli: ok`。CLI での `cache.sql` は試していない。
- `check.py explain .`:1 節の表のとおり。
- 変更前との全行比較:1 節のとおり一致。

## 6. うまくいかなかったこと・残っていること

- 1 節の結合順の問題は、`MATERIALIZED` の境界で避けた。境界を置かない結合は、共通表式の見積もり行数がずれるため、入力が表かビューかで計画が変わりうる。
- BLOB の添字による切り出しは、ファイルの大きさに比例する(2 節)。
- `cache.sql` は全層を一度にキャッシュする。層ごとの選択や、ファイルが変わったときの部分的な作り直しはない。
- 共変更のクエリの `GIT_FILE_CHANGES` 2 回の走査は残っている。
- 中間の段階が独立したビューでなくなったので、途中の結果を直接 `SELECT` で見ることはできない。見るには、文の一部を取り出して実行する。
