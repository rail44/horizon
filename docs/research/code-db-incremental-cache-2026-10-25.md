# ファイル集合を追跡対象ファイルに替え、永続の差分キャッシュを足した記録(2026-10-25)

2026-10-25、オーナーは、コードデータベースの SQL 層の 3 つ目の増分として示された形に「はい。お願いします。」と答えた。示された形は次のとおりである。ファイル集合を `glob()` から `git_tracked_files` に替える。repo ごとのキャッシュファイルを持ち、構文とファイルごとの導出を `(path, blob_id)` で引き、blob id の変わったファイルだけをパス単位で入れ替える。履歴は `since` で追記し、キャッシュした head が祖先でなくなっていれば作り直す。1 ファイル・10 ファイルの編集と 1 コミットの費用を、全体の作成と比べて測る。本記録は、その形で組んだときに分かったことと、この repo での測定である。観察だけを記し、推奨は書かない。

前提は [設計文書](../code-database-design.md) の原則と、前の 2 つの記録 [code-db-views-2026-10-24.md](code-db-views-2026-10-24.md)・[code-db-views-single-scan-2026-10-24.md](code-db-views-single-scan-2026-10-24.md)(以下「前回の記録」)である。作業は worker の作業ツリーで、`origin/code-quality-survey` の 7abe28d1 から行った。機械は前回と同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している(測定中の load average は 2〜4.6)。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0、sitting_duck は v1.16.0、git_history はこの作業ツリーで `duckdb-ext/build.sh` を使って作ったもの、jscpd は 5.3.1 である。Rust は変えていない。

## 主要な観察

- **ファイル集合は `git_tracked_files` の 1 回の呼び出しになった。** この repo で 0.007〜0.010 s、524 ファイルだった。同じ作業ツリーで `glob('**/*.rs')` と既定の除外は 0.069〜0.070 s で、ファイルの集合は完全に一致した。
- **永続キャッシュは `refresh.sql` の 1 ファイルで組めた。** 空のキャッシュからの作成は 3.54〜3.60 s、ピーク RSS 2.0〜2.2 GB だった。同じ時間帯の `cache.sql`(セッション内のコピー)の作成は 2.94〜3.03 s、ピーク RSS 3.0〜3.1 GB だった。
- **差分の更新は 0.1〜0.7 s だった。** 何も変わっていないとき 0.13〜0.20 s、1 ファイルの編集で 0.22〜0.27 s、10 ファイルの編集で 0.32〜0.71 s、3 ファイルを変えた 1 コミットで 0.31〜0.52 s だった。amend で履歴を作り直す場合は 2.17〜2.87 s で、その大半は `git_file_changes` と `git_hunks` の再計算だった。
- **更新の後の各ビューは、ライブと行ごとに一致した。** この repo では、`files`・`nodes`(1,542,853 行)・`units`・`tags`・履歴の 3 ビューの全行と、4 つのクエリの全行で確かめた。
- **何も変わっていないときにも `read_ast` を呼ばずには済まなかった。** `read_ast` は空のリストを束縛の時点で拒み、`CASE` で包んでも避けられない。存在しないパス 1 つと `ignore_errors` を渡す番兵にした。この呼び出しはファイルを 1 つも読まない。
- **言語ビューに部分集合の絞り込みを入れると、ライブの `units`・`tags` のピーク RSS が約 200〜270 MB 増えた。** 絞り込みの前に変数が NULL かを調べる形にすると、元に戻った。

## 1. ファイル集合(`git_tracked_files`)

`00_load.sql` は `include_globs` をやめ、`git_tracked_files(repo, include_untracked := …)` からファイル集合を取るようにした。対象は、mode が `100644` か `100755` の通常ファイルで、拡張子が `languages` にあるものである。シンボリックリンク(blob はリンク先のパス)とサブモジュール(id はコミット)は、`read_ast` が読む内容と blob id が対応しないので外した。`include_untracked`(既定 `false`)を真にすると、無視されていない未追跡ファイルも入る。`exclude_patterns` は任意の追加の除外として残し、既定を空にした。`.gitignore` は git_tracked_files が読むので、前回までの既定の除外(`target/*`、隠しディレクトリ)は要らなくなった。

`tracked_source_files` というビューを置き、`source_files` はそこから取ったスナップショット(`{path, mode, blob_id}` のリスト)にした。`files` は `file_id, path, language, mode, blob_id` になった。`file_id` が並びの位置であることは変わらない。

時間は、新しいプロセスで 3 回、ファイル集合を作る文だけを測った。

| 対象 | `glob('**/*.rs')` と既定の除外(変更前の形) | `git_tracked_files` | ファイル数 |
| --- | --- | --- | --- |
| この作業ツリー(`target/` 626 MB、その下に測定用の clone を含む) | 0.069〜0.070 s | 0.008 s | 524(両方で一致) |
| 測定用の clone | 0.017〜0.028 s | 0.008 s | 524(両方で一致) |
| `check.py measure-refresh` の `file set` 段階(`languages` と mode の絞り込みを含む) | | 0.007〜0.010 s | 524 |

前回までの README にあった、入れ子の作業ツリーを持つ主チェックアウトでの約 21 s(前回の記録では 20.9 s)は、測り直していない。主チェックアウトには触れないという作業の決まりによる。`git_tracked_files` は `.git` を持つディレクトリ(別の clone や作業ツリー)を降りないと文書化されている(`duckdb-ext/README.md`)。この作業ツリーの `target/code-db/clone` に置いた clone は、どちらの集合にも入らなかった。glob の側は `target/*` の除外で落ちたもので、git_tracked_files の側は無視と入れ子の両方の理由で出てこない。glob の 0.069 s は、`target/` の下をすべて歩いてから除外した時間である。

## 2. 永続キャッシュ(`refresh.sql`)

### 流れ

`refresh.sql` は、ビューのファイルの後に、どのセッションで何度読んでもよい。1 回の読み込みは次の順に進む。

1. 前提の確認(ビューのファイルが読まれていること、ビューが `cache.sql` のコピーを読んでいないこと)。
2. ファイル集合のスナップショットを取り直す(`00_load.sql` と同じ 2 文)。
3. `cache_path`(既定 `<repo>/target/code-db/cache.duckdb`)を `cache` として `ATTACH IF NOT EXISTS` し、表が無ければ作る。
4. 前回の `meta` の行を変数に読み、作り直しが要るかを決める。`meta.state` を `refreshing` にする。
5. 構文:現在のファイルのうち `(path, blob_id, language)` が `cache.nodes_files` に無いものを「変わった」、キャッシュにあって現在のファイルに無いパスを「消えた」とする。その両方の行を `cache.nodes` と `cache.nodes_files` から消し、変わったパスだけを 1 回の `read_ast` で読んで入れる。
6. `nodes` をキャッシュを読むビューに差し替える。
7. units と tags:変わったパスと消えたパスの行を消し、`scope_paths` を変わったパスにして `derived_units`・`derived_tags` を計算し、入れる。`scope_paths` を NULL に戻す。`units`・`tags` を差し替える。
8. 履歴:キャッシュした head が `git_commits(repo, since := since)` の中にあれば、`since := cached_head` で `cached_head..HEAD` を 3 つの表に追記する。無ければ 3 つの表を作り直す。3 つのビューを差し替える。
9. `CHECKPOINT cache` の後、`meta` の行を書き直す(`state = 'ok'`)。

文は 1 つずつ自動コミットで実行し、トランザクションで包んでいない。`now()` はトランザクションの開始時刻なので、文ごとに変数へ記録した時刻の差が層ごとの時間になる(`meta.seconds`)。途中で止まると `state` が `refreshing` のまま残り、次の読み込みは全体を作り直す。fixture でこの経路を確かめた。

### キーと作り直しの条件

| 層 | キー | 全体を作り直す条件 |
| --- | --- | --- |
| nodes | ファイルごとの `(path, blob_id, language)` | `meta` が無い、`state` が `ok` でない、`repo` が違う(`nodes.file_path` が絶対パスのため)、sitting_duck の版が違う |
| units、tags | nodes と同じ | 上の条件、または導出に使うビューの定義(`scope_files`・`sources`・`source_bytes`・`source_lines`・`*_units`・`*_tags`)の md5 が違う(再解析はしない) |
| commits、file_changes、hunks | `since`・`include_merges`・`languages` と head | 上のどれかが違う、キャッシュした head が `since..HEAD` に無い、head が無い |

導出の定義のキーに、マクロ `byte_slice`・`normalize_ws` は含めなかった。マクロの本文は `duckdb_functions()` からしか読めず、この呼び出しは 18 ms かかった(sitting_duck が数千の関数を登録しているため)。何も変わっていないときの更新の約 5 分の 1 にあたる。マクロを変えたときはキャッシュファイルを消す、と README に書いた。キャッシュのスキーマの版(今は 1)が違うファイルは、エラーにして消すよう促す。表の形を条件つきで作り直すことは SQL ではできないためである。

`lang/rust.sql` を変えた後の最初の更新で、導出のキーの違いが検出された。再解析なしに units 9,019 行と tags 14,553 行を全ファイルについて計算し直し、0.63 s かかった(1 回の観察)。

### 空の変更リストの扱い

`read_ast` は空のリストを拒む(`Binder Error: File pattern list cannot be empty`)。この検査は束縛の時点で起きるので、`read_ast(CASE WHEN … THEN […] ELSE [] END)` も同じエラーになった。glob が何にも当たらないパターンは、実行時に `read_ast needs at least one file to read` で止まった。存在しないパス 1 つに `ignore_errors := true` を付けると、エラーにならず 0 行を返した。

そこで、変わったファイルが無いときだけ、`<cache_path>.no-such-file.rs` を 1 つ渡し、`ignore_errors` を真にする。どちらも変数で切り替える。挿入の `WHERE` は変わったパスの行だけを残すので、仮にそのパスにファイルがあっても何も入らない。ファイルがあるときは `ignore_errors` を偽にする。読めないファイルを黙って 0 行にしないためである。2 つのファイルに分けてクライアントに条件つきで実行させる形は、CLI の `.read` だけでは書けないので採らなかった。

この形では、何も変わっていないときにも `read_ast` の呼び出し自体は残る。ファイルは読まず、`meta.nodes_parsed` は 0 である。呼び出しの有無そのものを確かめる手段(プロファイルの演算子の数え上げなど)は、fixture には入れていない。

### ビューの側

`file_id` と `unit_id` は今のファイル並びの位置なので、キャッシュには保存せず、クエリのときに付ける。`nodes` は `cache.nodes` を `cache.nodes_files` とパスで結合し、さらに `files` と、パスと blob id の両方で結合する。キャッシュした blob id が今のスナップショットの blob id と同じファイルの行だけが見える。`units` はファイルの中での順番(`unit_ord`、`parent_unit_ord`)を保存し、`unit_id` を `(file_id, unit_ord)` の行番号として、`parent_unit_id` を `unit_id - unit_ord + parent_unit_ord` として作る。ライブの `unit_id` は `(file_id, start_byte, -end_byte)` の順の行番号なので、ファイルの中の単位は連続していて、この変換は同じ値になる。

### 部分集合の計算

`views/files.sql` に `scope_files`(`scope_paths` が NULL なら全ファイル、そうでなければそのパスだけ)を置いた。`lang/rust.sql` の 2 つのビューは、`nodes` を `scope_files` のファイルに絞り、ファイル単位のタグ(パスの glob)も `scope_files` から取る。`sources` も `scope_files` と結合するので、`source_bytes`・`source_lines` も同じ範囲になる。`rust_nodes AS MATERIALIZED` の 1 回の走査と、文字の切り出しの 2 つの規約は変えていない。`explain` での `read_ast` の走査数は、`units`・`tags`・クローン位置のクエリでいずれも 1 のままだった。

`views/units.sql` には、言語の和をとる `derived_units`・`derived_tags` を足し、`units`・`tags` はそれを読むビューにした。`refresh.sql` は `units`・`tags` をキャッシュの上に差し替えるが、新しい行は差し替えられない `derived_*` から計算する。

最初は `nodes WHERE language = 'rust' AND file_id IN (SELECT file_id FROM scope_files)` とした。新しいプロセスで 3 回ずつ、絞り込みの無い写し(`target/ab/`)と比べた。

| ライブの文 | 絞り込みなし | `file_id IN (…)` | `scope_paths IS NULL OR file_id IN (…)` |
| --- | --- | --- | --- |
| `count(*) FROM units` | 0.55〜0.64 s / 1,051〜1,119 MiB | 0.57〜0.62 s / 1,317〜1,392 MiB | 0.56〜0.63 s / 1,075〜1,095 MiB |
| `count(*) FROM tags` | 0.78〜0.84 s / 1,226〜1,273 MiB | 0.79〜0.84 s / 1,377〜1,412 MiB | 0.78〜0.84 s / 1,252〜1,284 MiB |

時間は変わらず、ピーク RSS だけが増えた。計画の違いは調べていない。`getvariable` はクエリの束縛のときに定数になるので、NULL の判定を先に置くと、ライブでは条件全体が真に畳まれると考えて、その形にした。畳まれたかどうかは計画で確かめていない。表の 3 列目がその結果である。

### `cache.sql` との関係

`cache.sql`(セッション内の一時表)はそのまま残した。2 つは混ぜられないようにした。`cache.sql` は、`nodes` のビューが永続キャッシュを読んでいるときにエラーで止まる。`refresh.sql` は、`nodes` のビューが `nodes_cache` を読んでいるときに止まる。どちらも、ビューのファイルを読み直すとライブに戻る。`duckdb_views()` が返すビューの本文は正規化されていて、データベース名が引用符つき(`"cache".nodes`)になる。最初の `LIKE '%cache.nodes%'` は当たらなかったので、正規表現 `"?cache"?\.nodes\b` にした。

CLI は、失敗した文の後も続けて実行する。この 2 つの検査でファイルを止めるには `-bail`(または `.bail on`)が要る。`check.py cli` は `-bail` で、ビューのファイルの後に `refresh.sql` を読み、ライブと更新後の件数が Python と一致することを確かめる(524・9,019・14,553・34,791・1,502 と、クエリの行数 400・5・1,502・15)。

## 3. 検査(`check.py fixture`)

fixture の repo に `.gitignore`(`target/`、`.hidden/`)を足した。前回までの、glob の既定の除外で落としていた 2 つのファイルは、無視されるファイルとして落ちる。最初の fixture は `include_untracked = true` で読み、未追跡の `src/scratch.rs` を含めた。`files` の `mode` と、`blob_id` が `git rev-parse HEAD:src/shapes.rs` と一致することも確かめる。`file_changes` には `.gitignore` の 1 行が増えた。

2 つ目の fixture では、次の各段階の後で `refresh.sql` を読む。その前にビューのファイルを読み直し、ライブの 7 ビューを一時表に写し、4 つのクエリの結果を取っておく。そのうえで、更新後の 7 ビューが `EXCEPT ALL` の両方向で 0 行の差であること、4 つのクエリの結果が一致することを、すべての段階で確かめる。

| 段階 | 確かめたこと(`meta` の値) | 結果 |
| --- | --- | --- |
| 空のキャッシュから | syntax・history とも rebuilt、files_added 4、commits_added 4、head が `HEAD`、クローン位置のクエリの計画に `READ_AST`・`READ_TEXT` が無い | 通過 |
| 何も変わっていない | 変更の数がすべて 0、syntax incremental、history appended。ビューを読み直さずに 3 回読み、最短が 100 ms 未満 | 通過(89・82・91 ms) |
| 1 ファイルの編集(`src/rect.rs`) | files_changed 1、nodes_parsed・units_inserted・tags_inserted がそのファイルの行数と一致。他のパスの `cache.nodes`・`units`・`tags` の行が変わらず、そのファイルの行は変わった | 通過 |
| ファイルの追加(`git add`) | files_added 1、units_inserted 1、他のパスの行が変わらない | 通過 |
| ファイルの削除(`git rm`) | files_removed 1、そのパスの行が 0 | 通過 |
| 名前の変更(`git mv`) | files_added 1・files_removed 1、units 1・tags 3 | 通過 |
| 新しいコミット | history appended、commits_added 1、head が新しい `HEAD`、commits 5 行 | 通過 |
| amend | history rebuilt、commits_added 5、amend 前の件名が無い | 通過 |
| 未追跡ファイル | `include_untracked` が偽で変化なし、真で files_added 1、偽に戻すと files_removed 1 | 通過 |
| 中断された更新(`state = 'refreshing'`) | syntax・history とも rebuilt | 通過 |
| `cache.sql` と `refresh.sql` | 互いのビューの上では、それぞれのエラー文で止まる | 通過 |
| 新しいセッション | ビューのファイルを読み、更新すると何も作り直さない(変更の数が 0、syntax incremental)。ビューのファイルを読み直すと `nodes` が `read_ast` に戻る | 通過 |

何も変わっていないときの 100 ms は、同じセッションで続けて読んだ場合の値である。ビューのファイルを読み直し、ライブの写しを作った直後の読み込みは 0.10〜0.14 s だった(この形で試した 5 回)。範囲が空のときの `derived_units`・`derived_tags` は、fixture での単独の実行でそれぞれ 16〜17 ms・13〜14 ms かかり、同じセッションの読み込み全体(84〜91 ms)の約 3 分の 1 にあたった。範囲が空のときにこの 2 文を飛ばす手段は、SQL のファイルの中には無い。

`check.py refresh <clone>`(この repo の clone)でも、1 回目(空のキャッシュから)と 2 回目(変化なし)の両方で、7 ビューと 4 つのクエリがライブと一致した。

## 4. 測定

この作業ツリーを書き換えないように、`git clone -q . target/code-db/clone` で作った clone(7abe28d1、`.rs` が 524 ファイル)の上で測った。jscpd の報告は `duckdb-ext/sql/jscpd.sh` でその clone について作った。コマンドは `uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 3` である。この段階は、各回を新しいプロセスで実行し、ビューのファイルを読んだ後の `refresh.sql` 全体の wall time と、`meta.seconds` の層ごとの時間、プロセスのピーク RSS を記録する。編集の回の前には、`git reset --hard` で元に戻して同期の更新を 1 回(測らずに)行う。編集は、選んだファイルの末尾に `fn code_db_measure_edit() {}` を 1 行足すことである。対象の 10 ファイルは、`git ls-files '*.rs'` の並びから等間隔に選んだ。1 ファイルの場合はその最初(`crates/horizon-acp/src/ext.rs`)、1 コミットの場合は最初の 3 つである。最小〜最大を記す。

| 場合 | wall time | 主な層 | ピーク RSS | 行数 |
| --- | --- | --- | --- | --- |
| 空のキャッシュ | 3.54〜3.60 s | nodes 0.66〜0.71、units 0.14〜0.15、tags 0.34〜0.36、commits 0.02〜0.03、file_changes 0.98〜1.01、hunks 1.01〜1.03、checkpoint 0.26〜0.29、files 0.04〜0.06 s | 2,040〜2,225 MiB | nodes 1,542,853、units 9,019、tags 14,553、commits 1,599 |
| 何も変わっていない | 0.13〜0.20 s | files 0.03〜0.05、units 0.03〜0.04、tags 0.02〜0.03、commits 0.02〜0.04 s | 184〜185 MiB | すべて 0 |
| 1 ファイルの編集 | 0.22〜0.27 s | nodes 0.04〜0.05、units 0.05〜0.07、tags 0.04〜0.05 s | 205〜212 MiB | nodes 4,245、units 39、tags 17 |
| 10 ファイルの編集 | 0.32〜0.71 s | nodes 0.06〜0.08、units 0.05〜0.08、tags 0.07 s、checkpoint 0.02〜0.43 s | 273〜404 MiB | nodes 21,203、units 149、tags 147 |
| 3 ファイルを変えた 1 コミット | 0.31〜0.52 s | nodes 0.07、units 0.07、tags 0.06 s、checkpoint 0.01〜0.22 s | 244〜371 MiB | nodes 7,299、units 55、tags 46、commits 1 |
| amend(履歴の作り直し) | 2.17〜2.87 s | file_changes 1.00〜1.34、hunks 1.02〜1.36 s | 318〜325 MiB | commits 1,599 |

比べる相手の `cache.sql` の作成は、同じ clone で `check.py measure target/code-db/clone --runs 3 --only "build cache"` により 2.94〜3.03 s(nodes 0.51〜0.54、units 0.14、tags 0.26、commits 0.01〜0.02、file_changes 0.99〜1.00、hunks 1.01〜1.10 s)、ピーク RSS 2,955〜3,062 MiB だった。空のキャッシュからの `refresh.sql` は、これより 0.5〜0.6 s 長い。差の主なものは checkpoint(0.26〜0.29 s)と、nodes・tags を表に書く時間だった。ピーク RSS は約 0.9 GB 小さかった。この差の内訳は調べていない。

| キャッシュからのクエリ(ビューのファイルと `refresh.sql` の後) | wall time | ピーク RSS(更新を含む) |
| --- | --- | --- |
| `file_cochange` | 0.02〜0.03 s | 215〜222 MiB |
| `file_cochange_thresholds` | 0.03 s | 216〜220 MiB |
| `clone_site_tags` | 0.12 s | 231〜238 MiB |
| `clone_site_tags_summary` | 0.08〜0.11 s | 231〜235 MiB |

前回の記録の `cache.sql` の上では、クローン位置のクエリが 0.11〜0.24 s、共変更のクエリが 0.02〜0.09 s だった。

キャッシュファイルの大きさは、1 回目の測定の終わりで 47.8 MiB、2 回目の測定の終わりで 50.3 MiB だった(どちらも WAL を含む)。行を消しても領域は詰められず、再解析を重ねると少しずつ大きくなる。

checkpoint は、多くの回で 0.01〜0.02 s だったが、10 ファイルと 1 コミットの場合に、それぞれ 3 回のうち 1 回が 0.43 s・0.22 s だった。同じ 2 つの場合のピーク RSS の幅(273〜404 MiB、244〜371 MiB)も、他の場合より広かった。回ごとの対応と原因は調べていない。

`rust.sql` の範囲の判定を変える前に、同じコマンドで 1 回測っている(`file_id IN (…)` の形)。値は上の表と同じ範囲だった(空のキャッシュ 3.54〜3.59 s、何も変わっていない 0.16〜0.17 s、1 ファイル 0.20〜0.30 s、10 ファイル 0.27〜0.36 s、1 コミット 0.27〜0.32 s、amend 2.19〜2.33 s、キャッシュファイル 47.8 MiB)。

何も変わっていないときの 0.13〜0.20 s の内訳は、ファイル集合と前提の確認・キーの計算(files 0.03〜0.05 s)、範囲が空の `derived_units`・`derived_tags`(合わせて 0.05〜0.07 s)、履歴の祖先の判定と空の追記(commits 0.02〜0.04 s、`git_commits` を 1,599 件たどる)である。文は約 75 個あり、1 文ごとの解析と束縛にも時間がかかる。

## 5. うまくいかなかったこと・残っていること

- **名前つき引数に式を渡すと、位置引数として解釈された。** `ignore_errors := len(getvariable('c')) = 0` は `(ignore_errors := len(…)) = 0` になり、`read_ast(VARCHAR[], BOOLEAN)` という一致しない呼び出しのエラーになった。変数を渡す形にした。
- **`ATTACH` はディレクトリを作らない。** `cache_path` のディレクトリは先に作っておく必要がある。`check.py` は作ってから読む。SQL だけでディレクトリを作る手段は使っていない。
- **`CREATE TABLE IF NOT EXISTS … AS SELECT … FROM derived_units LIMIT 0` は、表があっても毎回 6〜7 ms かかった。** `units` と `tags` の表は列を明示して作る形にした。`nodes` の表だけは、番兵のパスへの `read_ast` から列を取っている(1 ms 前後)。
- 何も変わっていないときの 100 ms 未満は、fixture の同じセッションでの読み込みに限った値である。この repo では新しいプロセスで 0.13〜0.20 s だった。
- 履歴は、最後の更新の `since`・`include_merges` についてのものである。変えると次の更新で作り直しになる。
- スナップショットを取ってから `read_ast` が読むまでの間にファイルが変わると、新しい構文が古い blob id の下に入る。次の更新で blob id の違いとして読み直される。更新の途中で HEAD が動くと、3 つの履歴の表が 1 コミットずれうる。
- 更新の後に `00_load.sql` だけを読み直すと、ビューはキャッシュのまま、ファイルのスナップショットだけが新しくなる。blob id が変わったファイルは、次の更新まで行が見えない。
- 主チェックアウトでの glob の 20.9 s と `git_tracked_files` の比較は測っていない(1 節)。
- `git_commits` の最初の行を新しい head としている。`git log` の順に 1 つのスレッドから出すという `duckdb-ext/README.md` の記述に頼っている。
- 名前の変更は、古いパスの削除と新しいパスの追加として扱い、内容が同じでも読み直す。`nodes.file_path` が絶対パスを持つためである。
