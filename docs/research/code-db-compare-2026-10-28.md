# コードデータベースの前後比較(2026-10-28)

コードデータベースの SQL 層の 14 個目の増分である。ある過去のコミットの unit とモジュールのグラフを、チェックアウトせずにキャッシュから取り出し、系譜を通して今(HEAD)の unit と対応させ、2 つの問い(`compare_units` と、2 つに分けた `compare_graph`・`compare_cycles`)にした。SQL、`check.py`、文書だけを変え、Rust は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、依頼側が示した推奨 A・B・C(A:過去のコミットのスナップショットを `git_tree` でキャッシュから取り、スナップショットごとに SCIP の索引を持つ。B:両側の対応は系譜から自動で求め、あいまいなものは印を付ける。C:`compare_units(before, after)` と `compare_graph(before, after)` を作り、9 月のスイープで確かめる)に、オーナーは 2026-10-27 に「はい。では作業をお願いします。」と答えた。

前提は、[設計の文書](../code-database-design.md)(原則と、7・12・13 個目の増分。13 個目は、マージを差分に含めないと blob の層に無い blob があるという測った前提を書いている)、[2 回目の後ろ向きの記録](code-db-dogfood-retrospective-2-2026-10-26.md)、[系譜の継承の記録](code-db-inherited-lineage-2026-10-27.md)、`duckdb-ext/sql/` の SQL と README である。記録の決定は問い直していない。

## 要約

- **blob の層は、`include_merges` にかかわらずマージを最初の親との差分に含めるようにした**(変数 `blob_include_merges`、既定 true)。履歴のビュー(`commits`、`file_changes`、`hunks`)は `include_merges` に従ったままで、blob の側を hunk につなぐ `unit_changes` と系譜はマージの行を見ない。HEAD の複製で、`unit_changes`(39,056 行)、`unit_successions`(21,116 行)、`unit_history`(21,725 行)の行のハッシュは前後で同じだった。既存のキャッシュの最初の更新は blob のファイルの表だけを作り直し、30 個の blob を解析した(更新全体 1.19 s)(1.1 節)。
- **前の側を `views/snapshot.sql` に組んだ。** `compare_before` を `commits` の 1 つのコミットに解決し(`compare_commit`)、その木を `git_tree` で読み、今のファイルと同じ規則で範囲を決め(`snapshot_files`)、`blob_units` につなぐ(`snapshot_units`)。blob の層に blob が無いファイルは `snapshot_missing` に出す。今の unit の、そのコミットでの同一性は `unit_at_before` が求める(1.2、1.3 節)。
- **グラフの導出を 1 つの定義にした。** `views/graph.sql` の `unit_occurrences` から `module_cycles` までのビューを、入力の関係の名前を引数にとるテーブルマクロ `graph_*_over` にし、今のビューも前の側もそれを適用する。今のグラフのビュー 24 個(グラフ以外を含む)の行のハッシュは前後で同じだった(1.4 節)。
- **スイープ(46c37290 の複製、全履歴のキャッシュ、`compare_before = e223bc20`)で、`snapshot_missing` は 0 だった。** 前の側は、e223bc20 をチェックアウトした複製の今のビュー(その木の索引)と、unit 6,936 個、モジュールの辺 2,343 本、循環 18 個、fan-in 2,115 行、モジュール 6,936 行のすべてで一致した。`unit_at_before` は、`retro/lineage.sql` の歩き方を今のすべての同一性から始めたものと、6,703 組で一致し、片方だけの組は無かった(3 節)。
- **`compare_units` の数は、`kept` 6,282、`renamed` 31(うち `ambiguous` 7)、`moved` 390(55)、`added` 1,276、`removed` 242、`missing` 0 だった。** 後ろ向きの正解と比べると、範囲で追加され HEAD にある 1,609 個の同一性のうち 1,256 個が `added`、353 個が系譜でつながった(`moved`・`renamed`)。範囲で削除され基準にあって HEAD に無い 590 個のうち 241 個が `removed`、349 個がつながった。preserve の 36 個は `kept` 28、`moved` 2、`added` 6 だった。食い違いはすべて、正解が名前で数えていることと、範囲の中で作られた unit への preserve で説明できた(3.3 節)。
- **`compare_graph` は、モジュールの辺の `added` 1,283、`removed` 214、`kept` 2,129、`compare_cycles` は循環の `appeared` 5、`kept` 18、`disappeared` 0 だった。** 足された辺 2 本と現れた循環 1 つを `git diff`・`git grep` で確かめた(3.4 節)。
- **このブランチと main(merge-base 1e60bd32)では、`added` 415 がすべて `duckdb-ext/` の下で、ほかはすべて `kept`(コミット 0)、`removed` は無かった。** モジュールの辺と循環は全部 `kept` だった。`duckdb-ext/` は根のワークスペースの外にあり、根で作った索引に出現が無いので、辺は増えない(4 節)。
- **時間。** キャッシュからの問いは、読み込み(約 3 s)を含めて `compare_units` 4.1〜4.5 s、`compare_graph` 3.1〜3.2 s、`compare_cycles` 4.9 s で、最大の RSS は 1.4〜5.1 GB だった。更新、計画、検査は 5 節に書いた。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の fb16cd55 に合わせた(変更の無い状態で `git reset --hard`)。測定はこのリポジトリの使い捨ての複製 4 つ(作業ツリーの `target/code-db/cmp/` の下、`git clone --no-checkout` の後に `checkout`)で行い、作業ツリーのファイルと履歴は測定に使っていない。

- `end`:46c37290(スイープの終わり)。全履歴のキャッシュ。
- `before`:e223bc20(スイープの基準。`scip_before` と、前の側の照合に使う今のビュー)。
- `head`:fb16cd55(このブランチ)。
- `base`:1e60bd32(このブランチと main の merge-base。`scip_before`)。

4 つとも `run.sh index` で索引と jscpd の報告を作った(rust-analyzer scip は 125〜147 s、最大の RSS 6.6〜7.2 GB)。rust-analyzer は複製の `target/debug` にビルドスクリプトの出力を約 2.7 GB ずつ書いたので、索引ができた後に消した。前の SQL は、`git archive` で取り出した fb16cd55 の `duckdb-ext/sql`(`orig`)と、1 つ目のコミット 7c9eb79d の `duckdb-ext/sql`(`c1`)で動かした。

機械はオーナーの対話用デスクトップで、他の作業と共有している。ディスクの空きは 17〜23 GB で、`00_load.sql` の spill の上限はその半分だった。DuckDB CLI 2.0.0、rust-analyzer、jscpd 5.3.1 を使い、すべて `nice -n 19` で動かした。測定用の SQL とシェルは作業ツリーの `target/code-db/cmp/scripts/` に置き、コミットしていない(`retro/compare.sql` だけはコミットした)。時間は Python の `resource.getrusage(RUSAGE_CHILDREN)` で包んだ 1 回ずつの値である。

```sh
nice -n 19 duckdb-ext/build.sh
uv run --locked --script scripts/refactor-audit/run.py setup
duckdb-ext/sql/run.sh index target/code-db/cmp/{before,end,head,base}
# 1 つ目のコミット:orig と c1 で同じキャッシュを更新し、行のハッシュと meta を比べる
orig/duckdb-ext/sql/run.sh sql HEAD_CLONE hashes.sql ; c1/duckdb-ext/sql/run.sh sql HEAD_CLONE hashes.sql
# 2 つ目のコミット:c1 と後の SQL で、グラフのビューの行のハッシュを比べる
c1/duckdb-ext/sql/run.sh sql HEAD_CLONE graph_hashes.sql ; duckdb-ext/sql/run.sh sql HEAD_CLONE graph_hashes.sql
# スイープ
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 retro_changes=…/changes.parquet
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 retro_reviews=END/scripts/refactor-audit/reviews.json \
    retro_record_preserves=…/september-2026-record-preserves.csv retro_lineage_changes=… retro_lineage_preserves=…/lineage-preserves.parquet
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/compare.sql compare_before=e223bc20 scip_before=BEFORE/target/code-db/index.scip \
    retro_changes=…/changes.parquet retro_lineage_preserves=…/lineage-preserves.parquet
duckdb-ext/sql/run.sh sql BEFORE export_side.sql side=current          # e223bc20 の今のビュー
duckdb-ext/sql/run.sh sql END export_side.sql side=snapshot compare_before=e223bc20 scip_before=…   # 前の側
duckdb-ext/sql/run.sh query END compare_graph compare_before=e223bc20 scip_before=BEFORE/target/code-db/index.scip
# このブランチと main
duckdb-ext/sql/run.sh query HEAD_CLONE compare_units compare_before=1e60bd32 scip_before=BASE/target/code-db/index.scip
uv run --locked --script duckdb-ext/sql/check.py explain|measure-refresh|refresh HEAD_CLONE
uv run --locked --script duckdb-ext/sql/check.py fixture
```

## 1. 組んだもの

### 1.1 blob の層とマージ(1 つ目のコミット 7c9eb79d)

`00_load.sql` に変数 `blob_include_merges`(既定 true)を足し、`views/blobs.sql` の `blob_files` と `refresh.sql` の取り出しの `git_changed_blobs` に、`include_merges` の代わりに渡した。`commits`、`file_changes`、`hunks` と `commit_order` は `include_merges` に従ったままである。`unit_change_sides` は `blob_files` を `hunks` の `(sha, path)` につないでから unit を見るので、`include_merges` が false なら、マージのコミットの blob の行は `unit_changes` にも系譜にも届かない。

キャッシュの鍵。blob のファイルの表は履歴の鍵(`meta.history_key`)と一緒に作り直される作りなので、この変数を履歴の鍵に足すと、履歴と系譜まで作り直すことになる。そこで `meta.history_key` には、履歴の鍵の後ろに ` blob_include_merges=…` を足した文字列を入れ、履歴の比べ方はその部分を除いた前半(`split_part`)で、blob のファイルの表の比べ方は全体で行うようにした。`meta` の列もスキーマの版も変えていないので、前の SQL のキャッシュは消さずに使える。前のキャッシュには後半が無いので、最初の更新で blob のファイルの表だけが作り直される。解析済みの blob(`cache.blob_parsed`)と導出済みの blob(`cache.blob_derived`)は残るので、新しく入る blob だけが解析される。

HEAD の複製(fb16cd55、マージ 343 を含む 1,629 コミット)で測った値:

| 段階 | blob のファイルの行 | 異なる blob | `blob_units` の行 | 取り出し(`blob_files`) | 解析(`blob_nodes`) | 更新全体 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| orig、空のキャッシュから | 12,517 | 6,600 | 170,047 | 0.44 s | 10.41 s | 29.27 s(壁時計 33.8 s、8.20 GB) |
| c1、orig のキャッシュの更新 | 18,634 | 6,630 | 171,301 | 0.53 s | 0.22 s(30 個) | 1.19 s(4.9 s) |
| c1、変更なし | 同じ | 同じ | 同じ | 0.00 s | 0.01 s | 0.23 s |
| c1、新しいキャッシュと blob のディレクトリから | 18,634 | 6,630 | 171,301 | 0.55 s | 15.53 s | 28.01 s(32.6 s、8.64 GB) |
| orig、新しいキャッシュと blob のディレクトリから | 12,517 | 6,600 | 170,047 | 0.47 s | 13.23 s | 25.43 s(29.4 s、8.43 GB) |

c1 の更新では `history` と `lineage` は `appended`(系譜の導出 0 コミット)で、`blob_history` だけが `rebuilt` だった。`unit_changes`、`unit_successions`、`unit_history` の行の数と行のハッシュの和は、5 つの段階のすべてで同じだった(39,056 / 21,116 / 21,725 行)。blob のファイルの行は 6,117 行増えた。マージは最初の親との差分のすべてのファイルを持つので、行は blob の数(30)よりずっと多く増える。13 個目の増分の数(6,600 → 6,630)と同じである。

### 1.2 前の側(`views/snapshot.sql`)

`views/lineage.sql` の後に読むファイルを足した(`run.sh` と `check.py` の順に入れた)。

- **`compare_commit(sha, ord)`**:`compare_before` を `commits` のコミットに解決し、`commit_order` の位置を付ける。`git_commits` は親のコミットの列を返さないので、`compare_before^..HEAD` に出て `compare_before..HEAD` に出ないコミット(両方とも `include_merges := true`)を求める。`compare_before` が HEAD の祖先でマージでなければ、これはちょうど `compare_before` の 1 つになる。1 つにならないとき(祖先でない、マージで第 2 の親から別のコミットが入る)と、それが `commit_order` に無いとき(`since` より前、`include_merges` が false のマージ)は、問いが止まり、その旨を書いた 1 行が出る。`compare_before` が無いときも止まる。根のコミットには `^` が無いので、ビューを束縛する時点で `git_commits` が失敗する。`compare_range(sha)` は `compare_before..HEAD`(マージを含む)である。
- **`snapshot_files(file_id, path, language, mode, blob_id)`**:`git_tree(repo, compare_before)` を、今のファイルと同じ規則で絞る。規則を 1 つにするため、`00_load.sql` の `tracked_source_files` の条件をスカラーのマクロ `is_source_file(path, mode)` に移し、両方がそれを使う。`compare_before` が無いときは HEAD の木を示す(`git_tree` は NULL の版を受け付けない)。
- **`snapshot_units`**:`units` の列(入れ子を含む)と `blob_id`。`snapshot_files` を `blob_units` につなぎ、`units` と同じ順(ファイルの順、ファイルの中の位置)で番号を付ける。`parent_unit_id` は `unit_ord` と `parent_unit_ord` から求める。
- **`snapshot_missing(path, mode, blob_id)`**:`snapshot_files` のうち、blob が blob の層(`blob_list`)に無いもの。依頼文は「blob に `blob_units` の行が無いもの」と書いていたが、unit を持たない blob(`use` だけのファイルなど)は層にあっても行が無いので、層にあるかどうかで決めた。スイープの基準の木(e223bc20)では、層にあって unit を持たない blob が 4 個あり、依頼文の定義ではそれらが `missing` になっていた。
- **`snapshot_nodes`、`snapshot_impl_parts`、`snapshot_tags`**:前の側のグラフの入力。`snapshot_nodes` は `blob_nodes` に `snapshot_files` の `file_id` を付けたもの(持続キャッシュでは `cache.blob_nodes` の写し)。`snapshot_impl_parts` は言語の規則を適用する。そのために `lang/rust.sql` の `rust_impl_parts` をテーブルマクロ `rust_impl_parts_over(nodes_t)` にし(`rust_units_over` と同じ形)、`views/units.sql` に言語の和をとる点としてマクロ `impl_parts_over(nodes_t)` を置いた。blob にはタグが無いので、`snapshot_tags` は空である。

### 1.3 対応(`unit_at_before`)

今の unit ごとに、その系譜(`unit_lineage_segments`)の同一性のうち、コミットの区間 `[from_ord, until_ord]` が `compare_before` の位置を含み、前の木の unit にあるものを、そのコミットでの同一性とする。深さ 0 は unit 自身の同一性で、それが基準まで届くときに出る。1 つも無ければ、その unit は基準の後に作られた(体が動いた後で名前を保った unit も、そこで系譜が始まり直すのでここに入る)。複数あれば、その unit は複数の前の unit を継いでいる(併合)。1 つの前の同一性に複数の今の unit が届けば、分割である。同じ同一性に複数の道で届くときは、最も浅いものを 1 行にする。

最も浅いものだけを残すことも考えたが、`retro/lineage.sql` は、範囲のリンクだけで歩き、基準まで届いた(範囲の中で始まり直さない)同一性のうち基準の木にあるものを、深さによらずすべて残す。両者を一致させるため、区間が位置を含み前の木にあるものをすべて残した。

### 1.4 グラフの導出を 1 つにする(`views/graph.sql`)

`unit_occurrences`、`unit_tests`、`unit_references`、`unit_edges`、`unit_ancestors`、`impl_targets`、`impl_for`、`file_crate`、`modules`、`unit_module`、`module_edges`、`module_cycle_edges`、`module_cycles` の 13 のビューを、同じ本文のテーブルマクロ `graph_<ビュー>_over(...)` にした。引数は入力の関係の名前(`files_t`、`units_t`、`tags_t`、`impl_parts_t`、`occurrences_t` のうち、そのビューが読むもの)で、`query_table` で読み、前の段は同じ名前でその段のマクロを適用して読む。今のビューは、たとえば `CREATE VIEW module_edges AS FROM graph_module_edges_over('files', 'units', 'tags', 'impl_parts', 'scip_occurrences')` になった。本文はビューの本文の入力の名前だけを置き換えたもので、ほかのビュー(`unit_contains`、`implements`、`crate_edges`、`crate_cycles`、共変更の 4 つ)は変えていない。

選んだ理由。依頼文が挙げた 3 つの形のうち、変数で入力を切り替える形は、1 つの文の中で両側を求められない(変数は文の中で変えられない)。物化した入力の上のマクロは、`rust_units_over` が同じ本文を今のファイルと blob に適用している前例に従う。両側に `side` の列を持たせる形(両側の和の上で 1 回求める)も考えたが、グラフのビューは `MATERIALIZED` の CTE を多く持ち、そこで条件が押し下げられないので、今のビューを読むたびに前の側も求めることになる。

マクロとキャッシュの鍵。`refresh.sql` の `derive_key` は内部でないマクロの本文をすべて含む。`views/graph.sql` と `views/scip.sql` は索引があるときだけ読まれるので、そこにマクロを置くと、読む読まないで鍵が変わり、そのたびにすべての unit が導出し直される(`views/scip.sql` の注釈が、局所記号の修飾をマクロにしなかった理由として書いていたこと)。そこで `derive_key` から名前が `graph_` と `scip_` で始まるマクロを除いた。キャッシュのどの層もそれらを通して導出されない。前の SQL のキャッシュでは、そういう名前のマクロが無かったので、鍵は除く前と同じ値になる。

前の側の出現は `scip_before` の索引から読む。`views/scip.sql` の `scip_document_occurrences` の整形(1 始まりの行と列、局所記号の修飾)をマクロ `scip_document_occurrences_of(index_path)` にし、`scip_document_occurrences` と前の側の `snapshot_scip_occurrences` の両方がそれを使う。`scip_key` には、ビューの本文に加えてこのマクロの本文を含めた。`read_scip` は束縛の時点でファイルを開くので、`scip_before` が無いときは今の索引(`scip_index`)で束縛し、行は条件(`scip_before IS NOT NULL`)で空にする。

前の側のビューは `snapshot_unit_module`、`snapshot_unit_fan_in`、`snapshot_module_edges`、`snapshot_module_cycles` で、入力は `snapshot_files`、`snapshot_units`、`snapshot_tags`、`snapshot_impl_parts`、`snapshot_scip_occurrences` である。タグが無いので、前の側の unit の辺はテストの辺にもマクロの中の辺にもならないが、unit の辺の数、モジュールの辺(`nested_modules` と `same_type_unit_edges` を含む)、循環はタグを読まない。

ほかに、`unit_fan_in(unit_id, fan_in_files, reference_occurrences)` を、`queries/unit_fan_in_churn.sql` の fan-in の定義を写したマクロ `graph_unit_fan_in_over` のビューとして足した(問いのファイルは変えていないので、定義は 2 か所にある)。`module_renames(module_before, module)` は、前の側のモジュールを HEAD での名前にする。モジュールの名前のパスの部分を、`compare_before..HEAD` のファイルの改名(`file_changes.old_path → path`)でコミットの順にたどる。あるパスは、そこに着いたコミットより後の、そのパスの最初の改名で運ばれる(パスが改名で空いた後に同じ名前の新しいファイルができ、それも改名される場合に、後の改名で運ばないため)。`::` から後(インラインのモジュールの名前)はそのままで、インラインのモジュールの改名はたどらない。

### 1.5 問い

- **`queries/compare_units.sql`**:行は、HEAD の同一性と、それが継ぐ前の同一性の組ごとに 1 行、何も継がない HEAD の同一性ごとに 1 行、どの今の unit も継がない前の同一性ごとに 1 行、無いファイルごとに 1 行。同一性を共有する unit(1 つのファイルの 2 つの `impl Foo`)は 1 つの同一性にまとめ、`loc` は和、fan-in は最大、モジュールはどれか 1 つをとる。`status` は `kept`(両側で同じ同一性)、`renamed`(系譜でつながり、パスが同じで種類か名前が違う)、`moved`(系譜でつながり、パスが違う。名前が変わっていても `moved`)、`added`、`removed`、`missing` で、`ambiguous` は別の真偽の列である。`ambiguous` は、組がどちらかの同一性にとって複数の組の 1 つであるとき(分割、併合)、または HEAD の unit の系譜の範囲の中のリンクのどれかが `ambiguous` のとき(前の同一性への道に限らない)に真になる。`commits` と `move_commits` は、HEAD の unit の `unit_history` のうち `compare_before..HEAD` のコミット(祖先の関係で決め、マージは `include_merges` のときだけ)の数で、`unit_churn` の数え方である。`fan_in_before` は `scip_before` が無いと NULL になる。並びは状態(上の順)、`commits`、`|loc_after − loc_before|`(どちらも降順)、パス。すべての行に `snapshot_missing` の数が付く。モジュールと fan-in を出すので `views/graph.sql`(今の索引)を要る。
- **`queries/compare_graph.sql`**:モジュールの辺の `added`/`removed`/`kept` と `unit_edges_before`/`unit_edges_after`。前のモジュールは `module_renames` で HEAD の名前にし、同じ組に着いた前の辺は足す。`crate` で片方の端の crate に絞れる。
- **`queries/compare_cycles.sql`**:循環は行の形が辺と違うので、別の問いに分けた。前と HEAD の循環は、メンバー(前の側は `module_renames` の後の名前)を 1 つでも共有すれば対応する。`kept` は対応する組ごとに 1 行(両側の大きさと共有するメンバーの数)、`appeared`、`disappeared`、どちらかの側が他の側の複数と対応するときの `ambiguous`。

`compare_graph` と `compare_cycles` は `scip_before` が無いと止まる。どの問いも持続キャッシュで使う前提である(ライブでは前の側がすべての blob を解析する)。

### 1.6 `check.py` と README

`check.py` に、2 つの比較の fixture(`check.py compare-fixture`、`fixture` の中でも走る)を足した。どちらも rust-analyzer が無いと飛ばす。前のコミットの索引は、`git archive` でその木を取り出した crate から作る(`make_scip_index_at`)。

- 系譜の fixture で、3 つの前のコミットからの `compare_units`。`volume` を足したコミットからは、統合テストが `moved`、`volume` が `removed`、後で作られたファイルの unit がすべて `added`。`src/ops.rs` を足したコミットからは、`kept` 以外のすべての行(ファイルの改名、その場の改名、移動、分割と併合が `moved` で `ambiguous`、体が動いた先が `moved` で名前を保った方が `added`、それぞれのコミット数と移動のコミット数)。`src/meters.rs` を足したコミットからは、1 つのファイルの中の改名が `renamed`(継承のリンクを含む)、足された method が `added`。各段で `snapshot_files` がその木と一致し `snapshot_missing` が 0 であること、2 つの unit の fan-in が両側で 1 であること、`scip_before` が無いと前の fan-in が NULL であること、同じ行が `refresh.sql` を通しても出ること、2 つの誤り(`compare_before` が無い、HEAD から届かないコミット)。`since` を範囲の中に置くと、範囲のコミットが触れなかったファイルが `missing` に並ぶこと。
- グラフの fixture のコミットで、前の側(unit、モジュールの辺、循環、fan-in、モジュール)が、前のコミットの今のビュー(その木の索引)と一致すること。循環を足すコミットとの比較(辺 4 本が `added`、循環が `appeared`)、次のコミット(`src/rect.rs` を改名し、循環を断つ)との比較(改名したモジュールの辺が `kept`、辺 1 本が `removed`、循環が `disappeared`)、`scip_before` が無いときの誤り、`refresh.sql` を通した同じ行。

`check.py refresh` が比べるビューに `snapshot_files`、`snapshot_units`、`snapshot_missing` を足した(`compare_before` が無いので HEAD の木になる)。

README に節「Before/after comparison」を足し、変数の表(`compare_before`、`scip_before`、`blob_include_merges`)、配置の表、`Check` の節、blob の層の節(マージ)、キャッシュの作り直しの条件、「Deliberately not here yet」を合わせた。

## 2. 依頼文が決めていなかった判断

1. 変更を 2 つのコミットに分けた(blob の層、比較)。1 つ目の効き方を単独で測るため。
2. `blob_include_merges` を履歴の鍵の後半として持ち、別に比べる(`meta` の列もスキーマの版も変えない)。
3. `compare_before` の解決に `compare_before^..HEAD` と `compare_before..HEAD` の差を使う。根のコミットは扱えない。
4. `snapshot_missing` を「blob が層に無い」で決めた(1.2 節)。
5. ファイルの範囲の規則をマクロ `is_source_file` にして今のファイルと共有した。
6. `compare_units` は同一性の単位でまとめた(`loc` は和、fan-in は最大、モジュールはどれか 1 つ)。
7. パスが変われば名前が変わっていても `moved`、パスが同じなら `renamed`。
8. `ambiguous` は、組の多対多に加え、HEAD の unit の系譜の範囲の中の `ambiguous` なリンクで立てる(前の同一性への道に限らない)。
9. `unit_at_before` は、区間が位置を含み前の木にある同一性をすべて残す(`retro/lineage.sql` に合わせる)。
10. 対応は `commit_order` の位置で、コミットの数は祖先の関係(`compare_before..HEAD`)で決める。
11. グラフの 1 つの定義はテーブルマクロにした(1.4 節)。`derive_key` から `graph_` と `scip_` のマクロを除き、整形は `scip_document_occurrences_of` にし、その本文を `scip_key` に入れた。`rust_impl_parts` もマクロにしたので、既存のキャッシュの最初の更新ではすべての unit が導出し直される(5 節)。
12. 前の側にはタグが無い(`snapshot_tags` は空)。辺の数と循環は影響を受けない。
13. `scip_before` が無いとき、前の出現のビューは今の索引で束縛して空にする。
14. `compare_units` は `views/graph.sql` を要る(モジュールと fan-in)。
15. `compare_graph` を `compare_graph` と `compare_cycles` に分けた。循環はメンバーを共有すれば対応させ、前の循環の名前は改名後の最小のメンバーにした。
16. `module_renames` はファイルの改名だけをたどり、インラインのモジュールの改名はたどらない。
17. 並び:`compare_units` は依頼文の状態の順、`compare_graph` は `added`・`removed`・`kept`、`compare_cycles` は `appeared`・`disappeared`・`kept`。
18. fan-in は `unit_fan_in_churn` の定義を写したビュー `unit_fan_in` で求め、問いのファイルは変えていない(定義が 2 か所にある)。
19. `compare_graph` に `crate` の絞り込みを付けた。
20. 後ろ向きの照合を `retro/compare.sql` としてコミットした。

## 3. 測定 1:9 月のスイープ

`end`(46c37290)の全履歴のキャッシュで、`compare_before = e223bc20`、`scip_before` は `before` の索引。範囲は 104 コミット(うちマージ 1)。

### 3.1 前の側の完全さ

`snapshot_missing` は 0 だった。前の側を、`before`(e223bc20 のチェックアウト)の今のビューと比べた。

| 比べたもの | 前の側(`end` のキャッシュ) | `before` の今のビュー | 片方だけ |
| --- | ---: | ---: | ---: |
| unit(パス、種類、限定名、行、`loc`、`lead_start_line`、node、親の名前) | 6,936 | 6,936 | 0 / 0 |
| モジュールの辺(crate、unit の辺、出現、`nested_modules`、`same_type_unit_edges`) | 2,343 | 2,343 | 0 / 0 |
| 循環(メンバー、辺、unit の辺、出現) | 18 | 18 | 0 / 0 |
| fan-in(`fan_in_files`、出現) | 2,115 | 2,115 | 0 / 0 |
| unit のモジュール | 6,936 | 6,936 | 0 / 0 |

### 3.2 対応と `retro/lineage.sql`

`retro/compare.sql` は、`retro/lineage.sql` の歩き方(範囲のコミットの `same_name` 以外の継承の関係だけをたどり、範囲の `body_move` の始まり直しで切り、範囲の中で始まり直さずに届いた同一性を残す)を、HEAD のすべての同一性から始め、前の木にある同一性だけを残す。`unit_at_before` の組(今の同一性、前の同一性)と比べると、両方にある組が 6,703、片方だけの組は 0 / 0 だった。`unit_at_before` の深さは、0 が 6,298、1 が 414(`content_match` 372、`file_rename` 22、`body_move` 20)、2 が 9 だった。

### 3.3 状態の数と正解との照合

| 状態 | `ambiguous` | 行 | コミット 1 以上 |
| --- | --- | ---: | ---: |
| `kept` | false | 6,282 | 996 |
| `renamed` | false | 24 | 24 |
| `renamed` | true | 7 | 7 |
| `moved` | false | 335 | 327 |
| `moved` | true | 55 | 55 |
| `added` | false | 1,276 | 1,276 |
| `removed` | false | 242 | 0 |
| `missing` | — | 0 | — |

正解は 2 回目の後ろ向きの記録と同じ入力から作った。`retro/changes.sql`(`retro_since=e223bc20`、4,433 行、84 コミット、3,309 個の同一性)の `added` の同一性のうち HEAD にあるもの、`removed` の同一性のうち基準の木にあって HEAD に無いもの、`retro/lineage.sql` が書いた preserve の源が名指す HEAD の同一性(レビューの台帳と記録の行、`retro/signals.sql` の名前の規則)である。各同一性の `compare_units` の行の状態をまとめた。

| 正解 | 同一性 | 期待どおり | 系譜でつながった | そのほか |
| --- | ---: | ---: | --- | --- |
| 範囲で追加、HEAD にある | 1,609 | `added` 1,256 | `moved` 325(うち `ambiguous` 21)、`renamed` 26(2)、`moved` と `renamed` の両方 2(2) | — |
| 範囲で削除、基準にあり HEAD に無い | 590 | `removed` 241 | `moved` 334(24)、`renamed` 15 | — |
| preserve の名指し | 36 | `kept` 28(うちコミット 1 以上 13)、`moved` 2(2) | — | `added` 6 |

食い違いを系譜の行で読んだ。

- **追加と削除の食い違い(353 と 349)は、正解が同一性(パス、種類、名前)の名前で数えていることによる。** たとえば `crates/horizon-agent/src/providers/rig/retry.rs` の `retry_backoff` は、0ef42e15(「Consolidate provider configuration …」)の `content_match` で `providers/rig/completion.rs` の `retry_backoff` につながる(深さ 1)。`knowledge/document.rs` の `parse_status` は 5451c800 の `content_match` で `knowledge.rs` から、`contract/tool_result.rs` の `ToolCallResult.new` は 9eb4adea で `contract.rs` から、`persistence/projection/duckdb/append.rs` の `Store.append_records_atomic` は d5d28ca6 で `import.rs` の `Store.apply_chunk_in_one_transaction` から、`policy.rs` の `classify_input` は cf9a2daa の `body_move` で `classify_call` から来ている。名前で数えると新しいパスの同一性は追加、古いパスの同一性は削除になり、系譜で数えると同じ unit の移動か改名になる。
- **正解にない `removed` が 1 つあった。** `src/agent/view/rows.rs` の `tests.finished_tool_call_mark_reads_superseded_not_success` で、fe4f704d でファイルが `src/agent/view/transcript/rows.rs` に改名され(`file_rename` のリンク)、b9a2f2ae で新しいパスの下で削除された。名前の正解は削除を新しいパスに記録するので、基準の同一性の削除にならない。
- **preserve の `added` 6 個は、範囲の中で作られた unit への preserve だった。** `policy.rs` の `plan_prepared_call`(最初の `added` は cf9a2daa)、`providers/rig/conversation/mod.rs` の `ConversationHistory.apply`(46c37290)、`tools/completion.rs` の `ToolCompletion.live_request`(b00cb83b)、`tools/execution.rs` の `execute_synchronous`(901edb40)、`agentd/src/session/completion.rs` の `publish_tool_update`(b00cb83b)、`src/workspace/modals.rs` の `WorkspaceShell.fetch_picker`(b9a2f2ae)で、どれも最初の追加が範囲の中(`commit_order` 1,434〜1,443、基準は 1,339)にある。`plan_prepared_call` は cf9a2daa の `body_move` で `plan_tool_call` を継ぐが、`plan_tool_call` も基準の木に無い。台帳のレビューは範囲の中のコミットで行われていて、その時点で作られていた unit を名指している。
- **preserve の `kept` のうち 15 個はコミット 0 だった。** 依頼文は preserve を「`kept`/`renamed`/`moved` でコミット 1 以上」と見込んでいたが、記録の行が名指した unit の多く(`parse_line`、`DaemonProcess`、`legacy_bytes`、`scheme_from` など)は範囲で一度も変わっていない。

### 3.4 グラフ

| 状態 | 辺 | unit の辺(前) | unit の辺(後) |
| --- | ---: | ---: | ---: |
| `added` | 1,283 | — | 5,975 |
| `kept` | 2,129 | 17,485 | 17,003 |
| `removed` | 214 | 1,067 | — |

前の側の辺は 2,343 本、HEAD は 3,412 本、前の側の unit は 6,936 個、HEAD は 7,961 個だった。範囲の `.rs` のファイルの改名は 3 つで、`module_renames` は 3 つのモジュール(`src/agent/view/scroll.rs`、`rows.rs`、`rows.rs::tests` を `transcript/` の下へ)を運んだ。unit の辺の多い `added` と `removed` には、インラインのテストのモジュールをファイルに移したものが目立つ(`completion.rs::tests` の辺が `removed`、`completion/tests.rs` の辺が `added`)。モジュールの名前が変わるので、改名の対応には入らない。

循環は `appeared` 5、`kept` 18、`disappeared` 0 で、`ambiguous` は無かった。`kept` のうち、`judge/handle.rs` の循環は 18 → 25 モジュール(共有 14)、`providers/rig/completion.rs` の循環は 2 → 5 モジュール(HEAD では `clearing.rs` が最小のメンバー)、`contract.rs` の循環は共有 1 で HEAD では `contract/provider_event.rs` との組になった。

確かめ(`end` の複製で `git diff --stat e223bc20 46c37290` と `git grep`):

- **現れた循環 `src/terminal/paint.rs` ↔ `src/terminal/shaping.rs`。** e223bc20 では `shape_row_items` と `char_columns` がどちらも `src/terminal/mod.rs` にあり(1,352 行目と 1,524 行目)、同じモジュールの中なので辺が無い。46c37290 では `mod.rs` が 1,390 行減り、`paint.rs`(462 行)と `shaping.rs`(200 行)ができ、`paint.rs` の `paint_terminal` と `paint_scrollback_window` が `shaping::shape_row_items` を呼び、`shaping.rs` が `paint::char_columns` を `use` している。問いの辺の記号もこの 2 つだった。
- **足された辺 `crates/horizon-agent/src/config.rs` → `auxiliary.rs`。** `auxiliary.rs` は e223bc20 の木に無く、46c37290 の `config.rs` の 289 行目に `pub auxiliary: Option<crate::auxiliary::AuxiliaryConfig>` がある。
- **足された辺 `crates/horizon-agent/src/judge/handle.rs` → `tools/completion.rs`。** `tools/completion.rs` は e223bc20 の木に無く、46c37290 の `handle.rs` の 101 行目と 171 行目が `crate::tools::ToolCompletion` を使っている。

## 4. 測定 2:このブランチと main

`head`(fb16cd55)で、`compare_before` は `git merge-base origin/code-quality-survey main` の 1e60bd32(マージでない)、`scip_before` は `base` の索引。範囲は 47 コミットで、マージは無い。

| 状態 | 行 |
| --- | ---: |
| `kept` | 8,856(コミット 1 以上は 0) |
| `added` | 415 |
| ほか | 0 |

`added` はすべて `duckdb-ext/` の下だった(`git-history-core` 147、`scip-index-core` 132、`git-history` 73、`scip-index` 48、`host-check` 10、`scip-probe` 4、`gix-probe` 1)。`src/` と `crates/` の下に `removed` は無く、`kept` 以外の行も無かった。`snapshot_missing` は 0 だった。モジュールの辺は 3,517 本すべて `kept`(unit の辺 25,165 で前後同じ)、循環は 24 個すべて `kept` だった。`duckdb-ext/` は根のワークスペースから除かれていて、`rust-analyzer scip .` の索引に出現が無いので、足された unit から辺はできない。

## 5. 測定 3:時間、メモリ、更新、検査

### 問いの時間(キャッシュから、`run.sh query` の全体、2 回)

`run.sh` の読み込み(ビューのファイルと変更の無い `refresh.sql`)だけでも 2.6〜3.2 s かかるので、それを並べた。

| 複製 | 読み込みだけ | `compare_units` | `compare_units`(`scip_before` なし) | `compare_graph` | `compare_cycles` |
| --- | --- | --- | --- | --- | --- |
| `end`(スイープ) | 2.8〜2.9 s、0.3 GB | 4.1〜4.3 s、4.1〜4.2 GB | 4.4 s、4.2 GB | 3.2〜3.5 s、1.4 GB | 4.6〜4.9 s、1.8〜1.9 GB |
| `head`(ブランチ) | 2.6〜3.2 s、0.3 GB | 4.1〜4.5 s、5.1 GB | — | 3.1〜3.3 s、1.6〜1.7 GB | 4.9〜5.7 s、2.2 GB |

`compare_units` の最大の RSS は、`unit_history`(`unit_changes` を通る)と `unit_lineage_segments` を別々に展開することによる。`unit_churn` のように 1 回に読み直すことはしていない。

### 更新

2 つ目のコミットの SQL で、1 つ目のコミットの SQL が作った `head` のキャッシュを更新すると、`derived`、`lineage`、`scip` と blob のファイルの表が `rebuilt` になり、6,630 個の blob の unit が導出し直された(解析は 0、更新 8.92 s)。`lang/rust.sql` と `views/units.sql` のマクロ(`rust_impl_parts_over`、`impl_parts_over`)と `00_load.sql` の `is_source_file` が `derive_key` に入り、`scip_document_occurrences_of` が `scip_key` に入るからである。その後の変更の無い更新は 0.27 s だった。今のグラフのビューと関連のビュー 24 個(`unit_occurrences`、`unit_references`、`unit_edges`、`unit_tests`、`unit_ancestors`、`impl_targets`、`impl_for`、`implements`、`file_crate`、`modules`、`unit_module`、`module_edges`、`crate_edges`、`module_cycle_edges`、`module_cycles`、`crate_cycles`、`unit_cochange`、`cochange_without_reference`、`scip_occurrences`、`impl_parts`、`units`、`tags`、`unit_changes`、`unit_history`)の行の数と行のハッシュの和は、前の SQL と同じだった。

`check.py measure-refresh head --runs 2`(段階ごとに新しいプロセス)を、前の SQL(`orig`)と後の SQL で実行した。

| 段階 | 前(orig) | 後 | 継承の記録(後) |
| --- | ---: | ---: | ---: |
| 初回(空のキャッシュから) | 24.31〜24.61 s、6.9〜7.0 GB | 24.62〜24.79 s、6.7〜6.8 GB | 23.6〜26.2 s、6.7〜7.0 GB |
| うち `blob_files` / `blob_nodes` | 0.44〜0.45 / 12.63〜12.73 s(6,600 個) | 0.49〜0.51 / 12.78〜12.83 s(6,630 個) | — |
| 変更なし | 0.30〜0.32 s | 0.31 s | 0.29〜0.31 s |
| 1 ファイルを編集 | 0.28〜0.38 s | 0.39〜0.40 s | 0.33〜0.41 s |
| 10 ファイルを編集 | 0.42〜0.46 s | 0.41〜0.43 s | 0.37〜0.43 s |
| 3 ファイルの 1 コミット | 0.81〜0.86 s | 0.73〜0.86 s | 0.78〜3.22 s |
| HEAD の amend | 3.18〜4.05 s | 3.31〜4.00 s | 2.92〜3.27 s |
| 索引の変更 | 0.69〜0.70 s | 0.65〜0.74 s | — |

キャッシュのファイルは 619.8 → 628.0 MiB になった(blob のファイルの行 12,517 → 18,634)。キャッシュからの既存の問い(`unit_churn` 0.29〜0.30 s、`clone_change_consistency` 1.98〜2.02 s、`module_cycles` 2.65〜2.69 s など)は、前と振れの中で同じだった。

### `check.py`

- `check.py explain head`:前(`orig` の `check.py`)と後で、すべての行が同じだった(`unit_churn`・`clone_change_consistency`・`unit_fan_in_churn` の `READ_AST` 2・`READ_TEXT` 11、`module_cycles` の `READ_TEXT` 57 など)。
- `check.py refresh head`:2 回の更新のどちらでも、すべてのビュー(足した `snapshot_files`、`snapshot_units`、`snapshot_missing` を含む)と問いがライブと一致した(1 回目 0.33 s、2 回目 0.37 s。コマンド全体 858.6 s、子のプロセスの最大の RSS 23.7 GB)。
- `check.py fixture`:すべての段(fixture、history、lineage、scip、graph、counting、compare、compare graph、refresh)で通った(全体 143.9 s)。

## 6. 限界

- 後の側は常に HEAD である。前のコミットは `since..HEAD` の中の HEAD の祖先で、マージなら `include_merges` が要る。根のコミットは扱えない。
- スナップショットはマージが作った blob を要る(`blob_include_merges` の既定)。`since` を置くと、範囲のコミットが触れなかったファイルは `missing` になるので、全履歴のキャッシュで使う。
- 対応は `commit_order` の位置を比べる。位置は線形の履歴の上でだけ祖先の順と一致し、`compare_before` の後でマージされたがそれより前の日付を持つ横の枝のコミットは、前として扱われる。スイープではこれによる食い違いは出なかった(3.2 節)。
- blob にタグが無いので、前の側にはテストとマクロの印が無い。問いはそれらを比べない。
- `module_renames` はインラインのモジュールの改名をたどらない。インラインのテストのモジュールをファイルに移すと、名前の違うモジュールになる。
- `scip_before` がその木の索引であることは確かめない。索引は木のチェックアウトから作る必要があり、rust-analyzer は複製にビルドスクリプトの出力を書く(約 2.7 GB)。
- `compare_units` の `ambiguous` の後半(範囲の中の `ambiguous` なリンク)は、HEAD の unit の系譜全体で見るので、前の同一性への道に無いリンクでも立つ。
- fan-in の定義は `unit_fan_in_churn` と `unit_fan_in` の 2 か所にある。

## 7. 記録との関係

13 個目の増分の測った前提と食い違う数は無かった。新しいビューで測り直すと、`blob_include_merges := false` の `snapshot_missing` は e223bc20 で 1(`src/workspace/bindings.rs`)、46c37290 で 1(同じファイル)、fb16cd55 で 21、1e60bd32 で 21 で、既定(true)ではすべて 0 だった。blob の層の異なる blob は `head` で 6,600 → 6,630 個(13 個目の増分と同じ)、`end` で 5,766 → 5,776 個だった。

依頼文の見込みと違ったのは 2 点である。既存のキャッシュの最初の更新は、1 つ目のコミットだけなら見込みどおり blob を足すだけ(30 個)だったが、2 つ目のコミットのマクロが `derive_key` を変えるので、両方を通すとすべての unit の導出し直し(8.9 s)になる。preserve の見込み(`kept`/`renamed`/`moved` でコミット 1 以上)は、6 個が範囲の中で作られた unit で、15 個が範囲で変わっていない unit だった(3.3 節)。
