# コードデータベース:ファイルごとの構文木のキャッシュと、unit の木をその範囲のビューにする変更(2026-10-30)

この記録は、unit ごとに `parse_ast_list` で本文を解析していた前の増分(`code-db-unit-ast-2026-10-29.md`)を、ソースファイルごとに sitting_duck の `read_ast` で一度だけ解析し、その木を git の `blob_id` ごとに永続キャッシュに置き、unit の木をファイルの木のうち unit の範囲に入るノードのビューにする形に置き換えた増分の記録である。依頼はブランチ `code-quality-survey` の先端 a25d4933 からで、作業はワーカーのブランチで行った。設計文書(`docs/code-database-design.md`)の更新は依頼側に残している。

## 依頼と前提

前の増分では、キャッシュの構築が 43〜45 s かかり(ほぼすべてが 9,335 個の unit の解析)、入力が 1 か所でも変わるとキャッシュ全体を作り直していた。木のノードは 3,438,861 個で、46% がファイルのモジュール(ファイル全体)の木だった。

依頼側が比べた 3 つの案(依頼文による):

- **X**:ファイルを `read_ast` で一度だけ解析し、変化は `files.blob_id` で検出し、unit の木はファイルの木の範囲のビューにする。
- **Y**:unit ごとの解析を続け、`(blob_id, 範囲)` で使い回す。
- **Z**:unit ごとの解析を続け、unit の本文の md5 で使い回す。

オーナーは事前確認つきで X を選んだ(2026-10-30):「はい。お願いします。」。依頼文によれば、それより前のオーナーの指摘は、変化の検出は手で計算するハッシュではなく既存の事実に対するクエリであるべきこと、木は完全なまま格納し何を数えるかはクエリで決めること(前の増分の「普通に完全なツリーを全て格納すればいいのではないの？」)である。

依頼の順序は、(1) 事前確認(unit を単独で解析した木と、ファイルの木の範囲の部分木が一致するか。`unit_complexity` の数を変える差が残れば止まって報告)、(2) 構築、(3) 測定、(4) `check.py` の fixture、だった。

## 対象と方法

- ブランチの先端 a25d4933 を、ワーカーの作業ツリーの `target/code-db/clone` に clone し、`code-db index` を動かした(jscpd 2.4 s、rust-analyzer はルート 138.2 s、`preview-plugin` 53.0 s、他の 3 つは 6.3〜10.9 s)。rust-analyzer が作る各ワークスペースの `target/debug` は、下の「更新全体」の測定のあとに消した(判断の節)。
- DuckDB 2.0.0、sitting_duck v1.16.0、rust-analyzer 1.99.1。
- 機械はオーナーのデスクトップで、他のワーカーと共有している。時間はすべて `nice -n 19` での値である。前の増分の数(43〜45 s)は別の負荷の下の値なので、同じ clone の上で前の増分の SQL(clone 自身の `duckdb-ext/sql`)を今回もう一度測り、それと比べた。
- 測定用の SQL とスクリプトは作業ツリーの `target/scratch/` に置き、コミットしていない。前の増分の結果(`units`、`unit_ast`、`unit_complexity`、`unit_quality`、`files`)は、変更前の SQL で `target/scratch/prev.duckdb` に写してから比べた。

## 事前確認

`read_ast(paths, source := 'full', peek := 'none')` は、`parse_ast_list` に無い列 `start_column`、`end_column`、`start_byte`、`end_byte` を返す。小さなファイルで確かめると、列は 1 始まりで終わりは最後のバイトの次、バイトは 0 始まりで終わりを含まない。列の単位は UTF-8 のバイトで、SCIP の位置(`units.start_column` など)と同じ数え方である。そこで、unit の範囲 `(start_line, start_column)`〜`(end_line, end_column)` に収まるノードを、行と列で切り出した。

**ファイル全体**:537 ファイルを 0.67 s で解析し、1,584,713 ノードになった。ファイルのモジュール 532 個すべてで、前の増分の木(ファイル全体を単独で解析したもの)と、ノードの型・深さ・行を前順に並べた列が一致した。残る 5 ファイルは unit の無いファイルである。

**ファイルのモジュール以外の 8,803 個の unit**:前の木から根の `source_file` を除いて深さを 1 減らしたものと、範囲で切り出したノード(深さは切り出したノードの最小の深さを 0 とした)を、`型:相対深さ:親の型` を前順に並べた列で比べた。この列は木の形(前順と深さで木は決まる)と (親の型, 子の型) の組を両方含む。

| kind | symbol_kind | unit | 一致 | 不一致 |
| --- | --- | ---: | ---: | ---: |
| constant | Constant | 625 | 619 | 6 |
| constant | StaticVariable | 34 | 34 | 0 |
| function | Function | 4,919 | 4,919 | 0 |
| macro | Macro | 6 | 6 | 0 |
| method | Method | 1,525 | 1,525 | 0 |
| method | StaticMethod | 339 | 339 | 0 |
| method | TraitMethod | 37 | 37 | 0 |
| module | Module | 226 | 226 | 0 |
| type | AssociatedType | 30 | 30 | 0 |
| type | Enum | 261 | 261 | 0 |
| type | Struct | 743 | 743 | 0 |
| type | Trait | 15 | 15 | 0 |
| type | TypeAlias | 43 | 43 | 0 |

- 切り出したノードが無い unit は 0 個、切り出しの最上位のノードが 2 つ以上の深さにまたがる unit も 0 個だった。最上位は、範囲が覆う兄弟のノードの並び(前に付く doc コメントと属性は、tree-sitter では item の兄弟になる)である。
- ノードのファイルの行を前順に並べた列も、同じ 8,797 個で一致した。
- **不一致の 6 個**は、すべて `src/theme/scheme.rs` の定数(`BACKGROUND_DEFAULT`、`FOREGROUND_DEFAULT`、`CURSOR_DEFAULT`、`INFO_DEFAULT`、`TEXT_MUTED_DEFAULT`、`TEXT_SUBTLE_DEFAULT`)で、差は行末のコメント 1 つ(`line_comment` とその子の `//`)だけだった。前の増分は unit の行を丸ごと解析したので、定数の `;` の後ろの行末コメントが木に入った。範囲は `;` で終わるので、切り出しには入らない。前の記録が「終わりの行の unit より後に `,`・`;` 以外があるもの 6 個」として挙げたものと同じである。
- 依頼が候補に挙げた差のうち、単独解析の根 `source_file` は上のとおり前の木から除いて比べた(新しい木には無い)。範囲が行の途中で隣の item と接する unit は、行末コメントの 6 個のほかには無かった(範囲の始まりの列が 1 より大きい 3,860 個は字下げで、前に他のコードがあるものは前の記録のとおり 0 個)。属性マクロの付いた item と doc コメントは、すべて一致した側に入っている。
- **`unit_complexity`**:切り出した木の上で `views/quality.sql` の `unit_own_ast` と `unit_complexity` をそのまま読み直し、前の増分の行と比べると、9,335 行すべてが一致した(差 0 行)。行末コメントは何も数えないので、6 個の差は数に影響しない。

sitting_duck の読み(型と位置以外の列)は、形の一致した 8,797 個の unit の 1,854,657 ノードで比べた。`semantic_type`、`flags`、`name`、`signature_type`、`parameters`、`annotations`、`receiver`、`children_count`、`descendant_count` は全ノードで一致した。違ったのは 3 つで、どれもファイルの中で読んだことによる。

- `qualified_name` 23,051 ノード:ファイルの中では、囲むモジュールや impl が先頭に付く(例:`[tests, a_well_formed_grants_section_warns_about_nothing]`)。
- `sibling_index` 28,171 ノード:最上位のノードの、親(ファイルや impl の本体)の中での位置。単独解析では 0 から始まる。
- `modifiers` 2,037 ノード:ファイルの中の `function_item` の `modifiers` に、その item 自身のものではない属性が入る(単独で `[]` のものが `['#[derive(Default)]']`、`['#[test]']` のものが `['#[tokio::test]', '#[test]', '#[tokio::test(start_paused = true)]']` など)。同じ親の前の兄弟の属性を拾っているように見えるが、sitting_duck のコードは読んでいない。これらの列を読むビューとクエリは無い。

事前確認の停止条件(`unit_complexity` の数を変える差)には当たらなかったので、構築に進んだ。

## 作ったもの

- **`file_ast`**(`views/files.sql`):ソースファイルごとの完全な木で、ノード 1 つにつき 1 行。列は `read_ast` のもの(`source := 'full'`)から `file_path` と `peek` を除き、`file_id` を足したもの:`node_id`、`type`、`semantic_type`、`flags`、`name`、`qualified_name`、`signature_type`、`parameters`、`modifiers`、`annotations`、`receiver`、`language`、`start_line`、`end_line`、`start_column`、`end_column`、`start_byte`、`end_byte`、`parent_id`、`depth`、`sibling_index`、`children_count`、`descendant_count`、`scope`(struct のまま)。キャッシュが無ければ、読むたびに全ファイルを解析する。
- **`unit_ast`**(`views/units.sql`):`file_ast` のうち unit の範囲(行と列)に収まるノード。ファイルのモジュールの木はファイルの木そのもの。列は前と同じ並びに、ファイルの `start_column`、`end_column`、`start_byte`、`end_byte` を後ろに足した。`start_line`/`end_line` は unit の最初の行からの相対、`file_start_line`/`file_end_line` はファイルの行。`depth` は unit の最上位を 0 とする相対の深さ。`node_id`、`parent_id`、`scope_*` はファイルの木のノード id のままで、子孫が `node_id + 1 .. node_id + descendant_count` であることは変わらない(最上位のノードの親は unit の外になる)。
- `unit_own_ast` と `unit_complexity` は変えていない。
- **`refresh.sql`**:
  - 木のキャッシュは、入力の状態ごとのキャッシュファイルとは別のファイル `<cache_dir>/ast-<md5(構文の鍵)>.duckdb` に置き、`ast_cache` として ATTACH する。構文の鍵は `'file_ast 1, sitting_duck <読み込んだ版>'` で、版が変われば新しいファイルになる。
  - 表 `ast_cache.file_ast` は `blob_id` に `read_ast` の列を足したもの。ファイルが新しいときは、全ファイル(`blob_id` ごとに 1 パス)の `read_ast` から表を作る。
  - 読むたびに、`files` に無くなった `blob_id` の行を消し、表に根(`node_id = 0`)の無い `blob_id` のパスだけを `read_ast` に渡して足す。`read_ast` は空のリストを受けないので、存在しないパスを 1 つ後ろに付けて `ignore_errors := true` で読む。そのため、読めなかったファイルも黙って飛ばされうるので、そのあとで木の無いファイルがあれば `error()` で止める。
  - `ast_cache.parses(key, parsed_at, files_parsed, nodes_parsed, blobs_dropped, seconds)` に、解析か削除があった回を 1 行ずつ残す。`refresh.sql` の最後の結果にも、その回の `ast_files_parsed`、`ast_nodes_parsed`、`ast_blobs_dropped` を足した。
  - そのあと `file_ast` のビューを `ast_cache.file_ast` と `files` の `blob_id` での結合に置き換える。`unit_ast` はそのビューの上のビューのままで、入力の状態ごとのキャッシュに写す層からは外した。`unit_complexity` ほか他の層は前と同じで、鍵が変われば丸ごと作り直す。
- **`code-db`**(`code-db/src/main.rs`):読み込みのあとに古いキャッシュファイルを消す処理が、`ast-*.duckdb` とそれ以外をそれぞれの種類で最新の 1 つずつ残すようにした。前は `*.duckdb` のうち最新の 1 つだけを残したので、木のファイルが消されるか、入力の状態のファイルが消されるかのどちらかになった。
- **`check.py`**:キャッシュと生を比べるビューの一覧に `file_ast` を足した(`unit_ast` は比べ続ける)。`read_refresh` は木の 3 つの数も返す。fixture は下の節。
- `00_load.sql` と `README.md` の説明を合わせた。

## 前の増分の結果との一致

同じ clone の上で、前の増分の SQL で作った行と比べた。

- `unit_complexity`:9,335 行が行ごとに一致(どちら向きの `EXCEPT ALL` も 0 行)。
- `unit_quality`:9,335 行が行ごとに一致。`units` も一致。
- `queries/complexity.sql`(既定と `include_tests=true`):行の多重集合は一致した(並べ替えた CSV の md5 が同じ)。出力の順は、クエリの `ORDER BY` のキー(`decisions`、`max_nesting`、`loc`、`path`、`start_line`)の列が行ごとに一致し、違うのはキーが同じ行どうしの順だけだった(`TerminalHub.drain` と `TerminalHub.list_terminals` のように、1 つのマクロ呼び出しが同じ行に定義する method)。キーが同じ行の順は前の形でも決まっていない。
- `unit_ast` の行数は 3,438,861 から 3,430,046 になった。差の 8,815 は、ファイルのモジュール以外の unit の根 8,803 と、上の行末コメントの 12 ノードである。

## 測定

同じ clone、`check.py step REPO complexity --mode build|kept`(1 プロセスで、ビューの読み込み、`refresh.sql`、`complexity` のクエリ。RSS はそのプロセスのピーク)。各 3 回。

| | 前の増分(今回の再測定) | この増分 |
| --- | --- | --- |
| キャッシュの構築(cold、`refresh.sql`) | 25.8〜27.9 s | 2.05〜2.17 s(うち `read_ast` で 537 ファイル 0.69〜0.78 s) |
| そのときのピーク RSS | 2.64〜2.75 GB | 1.61〜1.64 GB |
| 1 ファイルの 1 行を変えたあと | 26.2 s(1 回。全体を作り直す) | 1.22〜1.65 s(うち木の更新 0.07〜0.10 s、11,267 ノード)、RSS 0.77〜0.88 GB |
| 3 ファイルの 1 行ずつを変えたあと | (同じく全体を作り直す) | 1.35〜1.48 s(うち木の更新 0.13〜0.19 s、27,548 ノード)、RSS 0.82〜0.83 GB |
| 何も変えないとき | 0.11〜0.15 s | 0.12〜0.17 s、RSS 0.25〜0.28 GB |
| キャッシュの大きさ | 90.5 MiB(1 ファイル) | 入力の状態のファイル 20.5〜21.0 MiB と、木のファイル 80.5〜83.5 MiB |
| キャッシュ無しの `complexity` | 49 s、2.9 GB(前の記録) | 2.2 s、1.8 GB |

- 変更は、ファイルの 10 行目の行末に `// one k` のようなコメントを足すもので、回ごとに違う内容にした(毎回新しい `blob_id` になる)。1 ファイルは `src/workspace/commands.rs`(1,263 行)、3 ファイルはそれに `crates/horizon-agent/src/tests.rs` と `src/palette.rs` を足したもの。`ast_cache.parses` には、1 ファイルで 3 回とも `files_parsed` 1、`blobs_dropped` 1、3 ファイルで 3 回とも 3 と 3 が残った。
- 変更のあとの時間の大部分(1.1〜1.5 s)は、入力の状態の鍵が変わったことによる他の層の作り直しである。
- **木のファイルの大きさ**:新しいファイルは 80.5〜83.5 MiB で、DuckDB の `pragma_database_size` では使用中のブロックが 169(42.2 MiB)、空きのブロックが 152〜165 だった。同じ行を別のファイルに `CREATE TABLE AS` で写すと 50.0 MiB(使用 169、空き 31)になったので、空きは `read_ast` から並列に書き込んだときにできたものと見られるが、原因は確かめていない。前の形と同じく、空きは後の書き込みで使い回される。上の 7 回の更新(1 ファイル 3 回、3 ファイル 3 回、下の「更新全体」1 回)のあと、ファイルは 82.7 MiB、使用中のブロックは 223(55.8 MiB)に増えた。消した行の分は詰め直さない。
- 7 回の更新のあとで `check.py refresh`(キャッシュのすべての層と各クエリを生と比べる)が通ったので、増分で更新した `file_ast` は全ファイルを解析し直したものと一致する。

**更新全体**(1 ファイルの 1 行を変えて、`code-db index` から `code-db query complexity` まで):

| 段階 | 時間 |
| --- | ---: |
| `code-db index`(jscpd と 5 つのワークスペースの rust-analyzer。ルート 37.8 s、`preview-plugin` 36.5 s、他は 2.8〜3.7 s) | 86.7 s |
| `code-db query complexity`(読み込み、`refresh.sql`、クエリ。その中の木の更新は 1 ファイル) | 3.7 s |

このときは各ワークスペースの `target/debug`(rust-analyzer のビルドスクリプトと手続きマクロ)が残っていた。cold の索引は上の 138.2 s などである。構文木の解析は、更新全体の時間のうち 0.1 s ほどになった。

## fixture

- 既存の制御構造の fixture(crate `flow`)は、`unit_complexity`、マクロの引数のトークン、`complexity` のクエリの期待値を変えずに通る。木の形を確かめる `trees` だけは、根の `source_file` が無くなったので書き換えた。根の有無の列を、深さ 0 のノードの型を並べた列(関数は `function_item`、インラインのモジュールは `mod_item`、ファイルのモジュールは `source_file`)と、その行が unit の範囲と一致するかの列にした。ノード数と自身のノード数は、ファイルのモジュール以外のすべての unit で 1 ずつ減った(根の分。`flow` は 740 のまま)。
- 構文木のキャッシュの段は、ファイルが複数ある最初の fixture(`shapes` と `app` など 8 ファイル)の既存の段に足した。最初の読み込みで 8 ファイルを解析し(ノード数は `file_ast` の行数と同じ)、2 回目の読み込み(`kept`)では何も解析せず、`shapes/src/rect.rs` を編集したあとの読み込みでは `(files_parsed, nodes_parsed, blobs_dropped)` が `(1, そのファイルのノード数, 1)` になることを確かめる。そのあと、別のキャッシュディレクトリで cold に作った接続と、キャッシュしたすべてのビュー(`file_ast`、`unit_ast` を含む)、すべてのクエリ、`unit_quality` の行を比べ、一致することを確かめる。
- `check.py fixture` は通った(2 分 39 秒)。`duckdb-ext/check.sh`(サブワークスペースの fmt・clippy・テスト、拡張のビルド、`code-db` の fmt・clippy・テスト)も通った。

## 依頼文が決めていなかった判断

- **木のキャッシュを別のファイルにした。** 入力の状態ごとのファイルは鍵が変わるたびに新しく作るので、その中に置くと `blob_id` で使い回せない。ファイル名に構文の鍵(sitting_duck の版と表の形の版)の md5 を入れ、版が変わったら新しいファイルから作る形にした。同じファイルの中で行を消して作り直す形だと、版によって `read_ast` の列が変わったときに表の形が合わなくなるためである。そのため `code-db` の古いファイルを消す処理を種類ごとにした(Rust の変更)。
- **鍵は `blob_id` だけで、言語を入れていない。** `read_ast` は拡張子から文法を選ぶ。同じ内容が拡張子の違う 2 つのパスにあり、別の言語に対応するときは、片方の木を両方に使ってしまう。`languages` が `rs` だけの今は起きない。同じ `blob_id` のパスが複数あるときは、パスの最小の 1 つだけを解析する。
- **`unit_ast` を入力の状態ごとのキャッシュに写さない。** 依頼文の「`unit_ast` はビューになる」に合わせた。読むのは `unit_complexity` で、それは写す層のままなので、キャッシュからのクエリは範囲の結合を計算しない。
- **`unit_ast` の `node_id`、`parent_id`、`scope_*` はファイルの木の id のままにし、unit の中で振り直さない。** 振り直すと最上位のノードの親が範囲の外の値になるのは同じで、`file_ast` と結合できる値を残した。深さは最上位を 0 にし、前の形の `source_file`(深さ 0)に当たる仮の根は作らない。
- **範囲は行と列で切った。** SCIP の範囲は行と列で、バイトは無い。列はどちらも UTF-8 のバイトなので、バイトに直さずに比べた。
- **`unit_ast` の列は前の並びを保ち、列とバイトの 4 列を後ろに足した。** `file_ast` の `scope` は `read_ast` の struct のまま、`unit_ast` は前と同じく `scope_*` に開いた。`file_ast` には `read_ast` の `language` を残した。
- **`file_ast` は `views/files.sql` に置いた。** ファイルの事実(`sources`、`source_lines`)と同じ場所で、`code-db` の `VIEW_FILES` は変わらない。生のビューは `ignore_errors` を使わず、読めないファイルはエラーになる。
- **新しい木のファイルは、全ファイルの `read_ast` からの `CREATE TABLE AS` で作る。** 最初は 0 行の表を作ってから `INSERT` していた。ファイルの大きさを調べる途中で変えたが、大きさは変わらなかった(80.5 MiB と 83.5 MiB)。新しいファイルを 1 つの文で作れるのでそのままにした。
- **消した行の詰め直しはしていない。** 使用中のブロックは更新のたびに少しずつ増える(上の測定)。木のファイルを消せば次の読み込みで作り直す(約 2 s)。
- **`ast_cache.parses` と `refresh.sql` の最後の結果の 3 列を足した。** fixture が「解析したのはそのファイルだけ」を行数で確かめるためである。
- **fixture の段は最初の fixture に足した。** `flow` はソースファイルが 1 つだけで、「そのファイルだけを解析した」ことを他のファイルと区別できない。`flow` にファイルを足すと期待値がほぼすべて動く。
- **スナップショットと内容の読み込みの間の競合**:`blob_id` は `files` のスナップショットのもので、`read_ast` はその時点のディスクの内容を読む。間にファイルが変わると、古い `blob_id` に新しい内容の木が付く。`sources` が内容を生で読むのと同じ性質で、対処していない。
- **clone の `target/debug` は、更新全体の測定のあとに消した。** 依頼は索引のあとに消すことだったが、1 ファイルの変更のあとの索引の時間を、ビルドスクリプトが残っている普段の状態で測るために残した。

## 残る点

- 木のファイルの空きブロックの原因(並列の書き込み)と、消した行の分の増え方。
- 入力の状態の鍵は `blob_id` を含むので、1 ファイルの変更でも他の層(1.1〜1.5 s)は作り直す。依頼の範囲外として変えていない。
- sitting_duck の `modifiers` がファイルの中で他の item の属性を含むこと(上の事前確認)。今は読むビューが無い。
