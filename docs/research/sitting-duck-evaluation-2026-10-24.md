# sitting_duck を構文層として評価した記録(2026-10-24)

オーナーの方針は「既存を採用できるものを選定しつつ、拡張としての実装をしていきたい」である。2026-10-24 の判断で、既存の拡張が持ちこたえる部品はそれを採用し、残りを Rust の拡張として作ることになった。構文層の候補は community 拡張の sitting_duck で、持ちこたえるなら採用する。本記録はその判定の材料であり、観察だけを記す。推奨は書かない。

比較の相手は Python のスパイク `scripts/code-db/`(`origin/code-quality-survey` の ed63e944)の `nodes`・`units`・`tags` である。スパイクの設計は `scripts/code-db/README.md`、スパイク自体の測定は [code-db-spike-2026-10-24.md](code-db-spike-2026-10-24.md)、sitting_duck について事前に分かっていたことは [duckdb-extension-options-2026-10-24.md](duckdb-extension-options-2026-10-24.md) の「既存拡張の一覧」と「sitting_duck の詳細」にある。

すべて worker の作業ツリー(origin/main に ed63e944 を fast-forward したもの)で、Python の `duckdb==2.0.0` を使って実行した。システムの `duckdb` CLI(1.5.0)は使っていない。機械は AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB、Linux 7.2.9(Void、glibc 2.41)である。測定用のスクリプトは `target/code-db/sd/` に置いた(コミットしていない)。再利用できる比較スクリプトとビューは `scripts/code-db/views/` にコミットした。

## 主要な観察

- **木は 1 行も違わない。** 515 ファイル・1,514,979 ノードのすべてで、`(file, node_id, parent_id, type, start_byte, end_byte, 行, 列, depth)` がスパイクの `nodes` と一致した。`node_id` はスパイクと同じ preorder の番号で、`descendant_count`・`children_count`・`sibling_index` も木の構造と矛盾しない。理由は、grammar がどちらも tree-sitter-rust v0.24.2 で同じだからである(ただし sitting_duck 側は grammar にパッチを当て、tree-sitter CLI 0.26.0 で `parser.c` を生成し直している)。
- **`units` と `tags` は、ノード表の上の SQL ビューで全件一致した。** units 8,878 件は全 9 列が一致し、tags 14,414 件は集合として一致した。端の場合を集めた fixture(後述)でも一致した。
- **`read_ast` は速い。** 515 ファイルを表にするのは全スレッドで 0.5〜1.9 s、1 スレッドで 6.0 s である。`ast_select*` の遅さ(1 回 4.6〜4.8 s、1.5.6 では 0.2〜0.45 s)はこの機械でも再現した。
- **既定の `context := 'native'` は、子の多いノードで超線形に遅くなる。** 1 つのブロックに 9,000 文を並べた 81,019 ノードのファイルは、`native` で 19.9 s かかり、`none` では 0.18 s、`normalized` では 0.26 s だった。
- **`ast_function_metrics` を複数ファイルにまとめて渡すと終わらない。** 原因はマクロの区間結合に `file_path` の等号がないことである(定義を読んで確かめた)。1 回目はメモリ上限なしで走らせ、プロセスは SIGKILL(exit 137)で終わった。2 回目は `memory_limit = '4GB'` で走らせ、23 GB を一時ファイルに書き出したところで止めた。ファイルごとに呼べば 515 ファイルで 9.6 s だった。
- **文字列を parse する関数はある。** `parse_ast(VARCHAR, language)` は byte offset を返すが、引数はリテラルに限られる。`parse_ast_list(VARCHAR, language)` は列を受け付けるが、byte offset と列番号を返さない。`git://` は sitting_duck 単体では開けず、duck_tails には 2.0.0 のバイナリがまだない。

## 1. インストールと読み込み

```sql
INSTALL sitting_duck FROM community; LOAD sitting_duck;
```

| 項目 | 値 |
| --- | --- |
| `PRAGMA version` | v2.0.0、d54cbbe3df(Cyanoptera) |
| `PRAGMA platform` | linux_amd64(Python の `platform.platform()` は `Linux-7.2.9_1-x86_64-with-glibc2.41`) |
| `duckdb_extensions()` | `sitting_duck`、loaded、installed、`extension_version` v1.16.0、`install_mode` REPOSITORY、`installed_from` community、`install_path` `~/.duckdb/extensions/v2.0.0/linux_amd64/sitting_duck.duckdb_extension` |
| レジストリのビルド元 | `duckdb/community-extensions` の `extensions/sitting_duck/description.yml` で、`ref` も `variegata` も b49238e625ec6c8be1d4c4dca92f9af86964a5b0 |
| tree-sitter-rust | b49238e の時点のサブモジュール `grammars/tree-sitter-rust` は 77a3747266f4…。これは tree-sitter-rust の v0.24.2 タグのコミットで、スパイクが固定している `tree-sitter-rust==0.24.2` と同じ版である |
| grammar への手当て | `patches/fix-rust-grammar-regex.patch` は、char literal と string のエスケープにある正規表現 `/u\{[0-9a-fA-F]+\}/` を `seq('u{', /[0-9a-fA-F]+/, '}')` に置き換える。`generated_parsers/MANIFEST` には「tree-sitter version: tree-sitter 0.26.0」(生成日 2026-08-31)とある |
| tree-sitter ランタイム | サブモジュール `third_party/tree-sitter` は eaa10b279f20…(tree-sitter の master の 2025-06-17 のコミットで、タグは付いていない)。スパイクは py-tree-sitter 0.25.2 を使う |

パッチが入っていても、`'\u{1F600}'` は `parse_ast` で `char_literal` の葉 1 個になった。スパイクの grammar でも葉 1 個であり、この repo には差として現れていない(4 節)。

`LOAD sitting_duck` は、既定のデータベースが `read_only=True` で開かれていると失敗する。初期化の途中で `semantic_predicates.sql` のマクロを `CREATE` しようとするためで、エラーは「Cannot execute statement of type "CREATE" on database "sd" which is attached in read-only mode!」だった。対処として、メモリ上のデータベースで LOAD してから、対象を `ATTACH … (READ_ONLY)` した。

### 文書から読み取ったこと

`docs/reference/functions.md`・`output-schema.md`・`parameters.md`・`css-selectors.md` を b49238e の時点で取得して読んだ。`duckdb_functions()` の実際の引数一覧とも照合した。

- **`read_ast` の形。** `read_ast(file_path | LIST(VARCHAR) [, language])` に名前つき引数が付く。実装上の名前つき引数は `ignore_errors`、`context`(`none`/`node_types_only`/`normalized`/`native`、既定 `native`)、`source`(`none`/`path`/`lines_only`/`lines`/`full`、既定 `lines`)、`structure`(`none`/`minimal`/`full`)、`peek`(`none`/`smart`/`full`/整数、既定 `smart`)、`batch_size`、`max_depth`、`prune`、`max_source_bytes`、`parse_timeout_ms`、`max_parse_nodes`、`peek_size`、`peek_mode` である。
- **列。** `node_id`、`type`、`semantic_type`、`flags`、`name`、`signature_type`、`parameters`、`modifiers`、`annotations`、`qualified_name`(`{semantic_type, name, index}` の LIST)、`receiver`、`scope`(`{current, function, class, module, stack}`)、`file_path`、`language`、`start_line`、`end_line`、`parent_id`、`depth`、`sibling_index`、`children_count`、`descendant_count`、`peek` がある。`start_column`・`end_column`・`start_byte`・`end_byte` は `source := 'full'` のときだけ現れる。`receiver` は文書の表になく、実際の出力にだけあった。
- **tree-sitter の field 名と `is_named` は出ない。** `flags` は sitting_duck 独自のビット(IS_CONSTRUCT など)で、`is_named` には対応しない(4 節)。
- **ノードのテキスト。** `source := 'full'` が足すのは位置の列だけで、テキストは保持しない(文書に明記がある)。テキストを得る手段は `peek` しかない。`peek := 'full'` なら全文が入るが、ルートのノードはファイル全体を持つことになる。本評価では `peek := 'none'` にし、`read_text` の内容を byte offset で切り出した(5 節)。
- **座標の基点。** `start_line`/`end_line`/`start_column`/`end_column` は 1 始まりで、`end_line` は終端の行を含む。`start_byte`/`end_byte` は 0 始まりの半開区間である。`sibling_index` と `depth` は 0 始まりである。列は文字ではなく byte で数える(4 節で、tree-sitter の byte 列 + 1 と全件一致した)。
- **文字列・blob の parse。** 3 節にまとめた。

## 2. スパイクの参照用の表

```sh
uv run --locked --script scripts/code-db/build.py build . --out target/code-db/horizon.duckdb --languages rust
```

`build_info` の値は次のとおりである。files 515(6,826,322 byte、parse_error 0)、nodes 1,514,979、units 8,878、tags 14,414、commits 1,586。依頼文にある coordinator の値と一致した。時間は total 71.6 s、そのうち syntax 10.5 s だった。この機械の他の負荷の下での値であり、スパイクの記録では 5.2 s だった。git HEAD は ed63e944、dirty は false である。

## 3. 文字列・blob の parse と `git://`

`target/code-db/sd/string_probe.py` で、`git show HEAD~50:src/workspace/mod.rs` の内容を表の列に入れて試した。

| 呼び出し | 結果 |
| --- | --- |
| `parse_ast_list(content, 'rust')`(列を渡す) | 動く。5,026 ノードで 0.13 s。要素の struct が持つのは `node_id, type, semantic_type, flags, name, qualified_name, signature_type, parameters, modifiers, annotations, receiver, file_path, language, start_line, end_line, parent_id, depth, sibling_index, children_count, descendant_count, scope, peek` で、**`start_byte`/`end_byte`/列番号はない**。引数は 2 つで、`source` を指定できない |
| `unnest(parse_ast_list(...), recursive := true)` を LATERAL で使う | 動く(5,026 行) |
| `parse_ast((SELECT content FROM blobs), 'rust')` | 「Table function cannot contain subqueries」で失敗する。文書のとおり、引数はバインド時のリテラルでなければならない |
| `parse_ast('<リテラル>', 'rust', source := 'full')` | 動く。5,026 ノード、`max(end_byte)` 28,375、0.02 s |
| `parse_ast_list_table(code, language)` | 文書では `parse_ast_list` を包む表マクロで、列を受け付けると書かれている。列の形は `parse_ast_list` と同じで、byte offset は含まない(未実行) |

したがって、履歴の blob をチェックアウトせずに parse して行範囲だけを得ることは、SQL の中でできる。byte offset まで欲しい場合は、SQL の文字列にリテラルとして埋め込んで `parse_ast` を呼ぶことになる(Python から 1 blob ずつ文を組み立てる)。

`git://`:sitting_duck は `git://path@rev` を自分では開かない。duck_tails なしで `read_ast('git://Cargo.toml@HEAD')` を呼ぶと、`IO Error: … Cannot open file "git://Cargo.toml@HEAD": No such file or directory` で失敗した。issue #282(open)によると、sitting_duck がやっているのは言語を判定するために URI から `@rev` を剥がすことだけで、ファイルを開くのは DuckDB の VirtualFileSystem に登録された duck_tails である。`INSTALL duck_tails FROM community` は、2.0.0 では `HTTP 404`(`…/v2.0.0/linux_amd64/duck_tails.duckdb_extension.gz`)のままだった。

## 4. `read_ast` の測定とノードの比較

### 時間

測定は `target/code-db/sd/measure.py` で行った。毎回新しいプロセスで、スパイクの `files` 表から 515 個のパスのリストを作り、次を実行した。

```sql
CREATE TABLE sd_nodes AS
SELECT file_path, node_id, parent_id, type, [semantic_type, flags, name, qualified_name,]
       start_line, end_line, start_column, end_column, start_byte, end_byte,
       depth, sibling_index, children_count, descendant_count
FROM read_ast([<515 paths>], 'rust', source := 'full', peek := 'none', context := <ctx>)
```

結果は毎回 1,514,979 行、`count(DISTINCT file_path)` = 515 だった。時間は `CREATE` の前後で測った wall time、ピーク RSS は `ru_maxrss` で、Python とスパイクの DB の ATTACH を含む。

| 条件 | wall time(s) | ピーク RSS(MB) |
| --- | --- | --- |
| native、24 スレッド、1 回目 | 1.91 | 994 |
| native、24 スレッド、その次の 3 回 | 1.25 / 1.18 / 1.24 | 985 / 1018 / 1025 |
| native、`peek := 'smart'`(既定) | 1.20 | 1094 |
| native、`SET threads = 4` | 1.70 | 531 |
| native、`SET threads = 1` | 5.99 | 294 |
| 後で通しで測った値(`measure_contexts.sh`):none ×3 | 0.56 / 0.53 / 0.52 | 900 前後 |
| 同:normalized ×3 | 0.70 / 0.67 / 0.70 | 1015 前後 |
| 同:native ×3 | 0.73 / 0.69 / 0.72 | 1005〜1030 |

「1 回目」は、プロセスとして初めて測った回という意味である。直前にスパイクの構築と失敗した試行が同じファイルを読んでいるので、ページキャッシュは冷えていない。root がないのでキャッシュは落としていない。native の 1.2 s と 0.7 s の差は、同じ条件で時間を置いて測った差で、原因は切り分けていない。比較のために書くと、スパイクの syntax 段階(nodes・units・tags の抽出と挿入、1 スレッドの Python)は 10.5 s だった。

`ast_select*` の遅さは `target/code-db/sd/select_timing.py` で測った。1 回ずつの測定で、背景情報である。

| 呼び出し | DuckDB 2.0.0 | DuckDB 1.5.6(sitting_duck `803b35b`) |
| --- | --- | --- |
| `ast_select('src/workspace/mod.rs', 'impl_item > declaration_list')`(4 行) | 4.61 s / 4.66 s(別プロセスでは 4.75 / 4.79 s) | 0.21 s / 0.45 s |
| `ast_select_from(<全列の read_ast 表 1.5 M 行>, 同じセレクタ)`(556 行) | 4.70 s | 未測定 |
| 同じ内容の素の SQL(`parent_id` の結合) | 0.01 s(556 行。1 ファイルに絞ると 4 行) | — |

1 ファイルでも 1.5 M 行でも 1 回あたり約 4.7 s かかり、入力の大きさによらない。メンテナが「PLANNING, independent of input size」と書いたとおりの形である。`ast_select_from` には、`read_ast` の全列(`peek`・`modifiers` など)を持つ表を渡す必要がある。列を絞った表を渡すと、「Referenced column "peek" not found」や「does not have a column named "modifiers"」で失敗した。

### ノードの比較

比較は `target/code-db/sd/sql/n1.sql`・`n2.sql`・`n3.sql` と、`scripts/code-db/views/evaluate.py` の `nodes` 節で行った。`file_path` は `files.path` で `file_id` に対応づけた。

| 検査 | 結果 |
| --- | --- |
| ファイルごとのノード数 | 515 / 515 ファイルで一致(差の合計 0) |
| `(file, type, start_byte, end_byte)` の多重集合 | 差 0 / 0 |
| `(file, node_id, parent_id, type, start_byte, end_byte, depth)` | 差 0 / 0。**node_id と parent_id の番号はスパイクの preorder と同一である** |
| 行:`start_line = start_row + 1`、`end_line = end_row + 1` | 1,514,979 件すべて |
| 列:`start_column = start_col + 1`、`end_column = end_col + 1` | 1,514,979 件すべて。`start_col` は tree-sitter の byte 列なので、sitting_duck の列も 1 始まりの byte 列である |
| `descendant_count = children_count + Σ 子の descendant_count` | すべて一致 |
| preorder の区間:`node_id + descendant_count` のノードが部分木の中にあり、その次のノードは外にある | すべて成り立つ。したがって `descendant_count` = 最後の子孫の id − 自分の id である |
| `children_count` = `parent_id` で数えた子の数 | すべて一致 |
| `sibling_index` = 親の中での 0 始まりの順位(node_id の順) | 根以外の 1,514,464 件すべてで一致 |
| ERROR・MISSING ノード | この repo には 0 件。fixture の `src/broken.rs`(ERROR 4 個を含む 48 ノード)も一致した |

匿名トークンは含まれる(`fn`、`(`、`::` など)。ただし `is_named` は出力されない。`flags` との対応も取れない。たとえば `is_named = false` で `flags = 0` が 266,813 件、`flags = 1` が 427,595 件ある。`is_named = true` でも `flags = 1` が 10,398 件あり、内訳は `self` 4,868、`crate` 3,344、`super` 2,186 である。この repo のデータでは、同じ型名が名前つきと匿名の両方に現れることはなかった(`n3.sql`)。それでも、型名だけで `is_named` を決めることは grammar 一般ではできない。たとえば `block` や `lifetime` は、名前つきのノード型であると同時に、macro_rules の fragment specifier では匿名のトークンになる。

ファイルあたりのノード数の最大は 43,199 だった。旧版の uint16 の折り返し(65,536 超)はこの repo では起こらない。そこで fixture に 81,019 ノードのファイルを入れて試し、一致した。

### 端の場合の fixture

`target/code-db/sd/make_fixture.py` で、12 ファイルの git リポジトリを `target/code-db/fixture-repo` に作った。中身は、負の impl、impl ヘッダ内のコメント、`unsafe impl`、総称つきの trait impl、関連型と関連定数、`extern` ブロック、macro_rules の fragment specifier、`'\u{1F600}'`、日本語の文字列、各種の test 属性(`#[tokio::test]` の後に doc コメントと別の属性、`rstest::rstest`、`test_case` の連続、`bench`、`my::test::helper`)、`cfg(not(test))`、`cfg(all(test, unix))`、`cfg(all(unix, test))`、`cfg(any(test, …))`、`cfg(feature = "test")`、関数の中の `cfg(test)` の関数、`#![cfg(test)]` と `#![cfg(all(test, …))]` と `#![cfg(not(test))]`、パスのグロブ(`tests/`、`foo_tests.rs`、`test_util.rs`、`crates/test_dir/lib.rs`、`tests.rs`、`contests.rs`)、構文エラーのファイル、81,019 ノードのファイルである。

`build.py build target/code-db/fixture-repo` の結果は nodes 82,171、units 55、tags 30 だった。これに対して `evaluate.py` を fixture のルートから走らせ、ノード・units・tags のすべてで差が 0 だった。

このとき、81,019 ノードのファイル `src/big.rs`(1 つのブロックに `let` を 9,000 文)だけで `read_ast` が 19.3 s かかった。`target/code-db/sd/big_probe.py` で条件を変えて測った結果は次のとおりである。

| 条件 | 時間 |
| --- | --- |
| `context := 'none'` | 0.18 s |
| `context := 'node_types_only'` | 0.20 s |
| `context := 'normalized'` | 0.26 s |
| `context := 'native'`(既定) | 19.92 s |
| `context := 'none', structure := 'minimal'` | 0.11 s |

ブロック内の文の数を 1,000 / 2,000 / 4,000 / 8,000 と増やして `parse_ast`(native)を測ると、0.19 / 0.81 / 3.27 / 14.24 s で、ほぼ 2 乗で増えた。どの処理が 2 乗になっているかは調べていない。この repo で最も大きいファイル(src/workspace/mod.rs、5,102 ノード)は native で 0.02 s だった。

## 5. `units` と `tags` を SQL ビューで再現する

`scripts/code-db/views/rust_units.sql` と `rust_tags.sql` は、`sd_nodes`(read_ast の出力)と `sd_source(file_path, content)`(`read_text`)だけを入力にする。`build.py` には組み込んでいない。比較は次のコマンドで行う。

```sh
uv run --script scripts/code-db/views/evaluate.py target/code-db/horizon.duckdb [nodes|units|tags|builtin]
```

ビューの実行時間は units 0.33 s、tags 1.24 s で、`read_ast` は 0.6 s だった。

### 結果

| units の kind | スパイク | ビュー |
| --- | --- | --- |
| function | 4,795 | 4,795 |
| method | 1,821 | 1,821 |
| impl | 556 | 556 |
| type | 1,002 | 1,002 |
| module | 697 | 697 |
| macro | 7 | 7 |

`(file, node_id, parent の node_id, kind, name, qualified_name, start_row, end_row, loc)` の `EXCEPT ALL` は、両方向とも 0 件だった。

| tag | スパイク | ビュー |
| --- | --- | --- |
| generated_or_macro | 11,179 | 11,179 |
| test | 3,021 | 3,021 |
| trait_impl | 214 | 214 |

`(file, node_id, tag)` の集合の差は、両方向とも 0 件だった。test の内訳(ノード型別)は、function_item 2,608、mod_item 272、source_file 64、use_declaration 43、const_item 18、struct_item 11、impl_item 3、type_item 1、enum_item 1 である。この repo には、負の impl、impl ヘッダ内のコメント、名前が metavariable の function_item はいずれも 0 件だった。これらは fixture で確かめた。

### 書き換えの方法

- **field 名の代わり。** `name: (identifier)` などは「その型を持つ最初の直接の子」と読み替えた。対象の item では、ほかの直接の子がその型を持たない。impl の `trait:` と `type:` は、匿名の `for` トークンの直前と直後にある、コメントでない子とした。`for` がない impl の `type:` は、`impl` の後にあって `type_parameters` でない最初の子とした。`trait_impl` は「`for` を直接の子に持つ impl_item」とした。
- **テキスト。** DuckDB 2.0 には BLOB の `substring` がない。VARCHAR の `substring` はコードポイント単位で数える。そこで `decode(unhex(substring(hex(content), 2*s+1, 2*(e-s))))` で byte 範囲を切り出した。名前の空白の正規化は `regexp_replace(t, '\s+', ' ', 'g')` と `trim` で行った。スパイクは Python の `\s`(Unicode の空白)を使うのに対し、RE2 の `\s` は ASCII だけである。この repo では差が出なかった。
- **入れ子と qualified_name。** 包む unit を、木の中の unit の祖先として求めた。判定は区間 `a.node_id <= u.node_id <= a.node_id + a.descendant_count` で行い、再帰 CTE は使っていない。`qualified_name` は、祖先の名前を node_id の順に `.` でつないだものである。
- **loc。** `string_split(content, chr(10))` で行に分け、`[ \t\r\x0b\x0c]*` に全一致しない行を数えた累積和の差とした。
- **アンカー `.` と `*`。** `(attribute_item …) . [(attribute_item) (line_comment) (block_comment)]* . (item)` は、「item の直前に、兄弟として連続して並ぶ attribute_item とコメントの列の中に、条件を満たす attribute_item が 1 つある」と読み替えた。アンカーは名前つきの兄弟だけを数えるので、兄弟の型が `[a-z_]+` に全一致しないもの(句読点)を除いた。
- **`RUST_CFG_TEST` の `token_tree . (identifier) .`。** これは「名前つきの子がちょうど 1 つ」を意味する。名前つきかどうかは型名から推した。`[a-z_]+` に全一致し、かつ tree-sitter-rust 0.24 で匿名になるキーワードとプリミティブ型名の一覧(`fn`、`for`、`u8`、`_`、fragment specifier のうち名前つきの型と衝突しないものなど)に入らないものを名前つきとした。

### ノード表だけでは表せなかったこと、表しにくかったこと

- **`is_named`。** read_ast は出さず、`flags` からも導けない(4 節)。アンカーの意味と「名前つきの子が 1 つ」は、型名の一覧で近似した。この近似は grammar の `node-types.json` を知っていて初めて書ける。`block`・`lifetime`・`item` のように、同じ名前が名前つきの型と匿名のトークンの両方にある場合は、型名では区別できない。この repo と fixture では、近似による差は出なかった。
- **field 名。** read_ast は出さない。上のように子の型と位置で読み替えた。読み替えの正しさは item の文法ごとの知識に頼っている。`.scm` を差し替えるだけでは別言語に移せない。
- **ノードのテキスト。** read_ast の行には入っていない(`peek` は既定で切り詰められる)。元のファイルを `read_text` で別に読み、hex を経由して byte で切り出す必要があった。履歴の blob でも、同じ内容をどこかから持ってくる必要がある。`parse_ast_list` は byte offset を返さないので、列で渡した blob に同じ方法は使えない(3 節)。
- **述語(`#eq?`、`#any-of?`)。** 切り出したテキストの比較で表せた。
- **`.scm` のクエリ。** sitting_duck は実行しない。languages.py のクエリは、手で SQL に書き直すことになる。書き直した量は、コメントを含めて units が 143 行、tags が 139 行の SQL である。

## 6. sitting_duck 自身の unit 判定

`evaluate.py builtin`、`target/code-db/sd/sql/b1.sql`・`b2.sql`・`b4.sql`、`metrics_perfile.py` で調べた。

`is_function_definition(semantic_type)` が真になるノード型は次のとおりである。

| type | 件数 |
| --- | --- |
| `fn`(匿名のキーワードトークン) | 6,623 |
| function_item | 6,579 |
| closure_expression | 2,964 |
| function_signature_item | 37 |
| macro_definition | 7 |

- スパイクの function 4,795 件と method 1,821 件は、すべて `is_function_definition` に含まれた。`name` も 6,616 件すべてで一致した。一方で、匿名の `fn` トークン 6,623 件(名前は `'fn'`)とクロージャ 2,964 件も同じ分類になる。function と method の区別は `semantic_type` にはない(どちらも DEFINITION_FUNCTION)。impl と型はどちらも DEFINITION_CLASS で、module は DEFINITION_MODULE である。
- unit の全 kind について `name` を比べた(`evaluate.py builtin` の最後の表)。function・method・type・module・macro は全件一致した。impl だけが 556 件中 330 件の一致だった。`b4.sql` による impl の内訳は次のとおりである。

| impl の形 | 件数 | sitting_duck の `name` |
| --- | --- | --- |
| 固有 impl、型名がそのまま | 330 | スパイクと同じ |
| 固有 impl、総称つき(例 `ToolCompletion<R>`) | 12 | 空 |
| trait impl | 148 | trait 名(例 `Drop for WriterExit` → `Drop`) |
| trait impl | 59 | 型名(例 `std::fmt::Display for SaveError` → `SaveError`) |
| trait impl | 7 | 空(例 `serde::Deserializer<'de> for Fields<'_>`) |

  `Trait for Type` の形の名前になる impl は 1 件もない。trait 名と型名のどちらが選ばれるかは、trait が単純な識別子か、パスつきか総称つきかで分かれているように見えるが、規則は確かめていない。
- `qualified_name` を比べた。segment の `name` を `.` でつなぐと、スパイクと一致したのは function 4,792 / 4,795、method 1,478 / 1,821、type 987 / 1,002、module 697 / 697、macro 7 / 7、impl 330 / 556 だった。不一致は、親が trait impl の method(316 件すべて)と関連型(12 件すべて)、および固有 impl の総称つきの型に集中している。原因は上の impl の名前の違いである。sitting_duck の segment には同名の衝突を区別する `index` が付く(`C[Registration][2] F[new]`)。
- `ast_function_metrics(source, language)` の出力は、`file_path, name, language, start_line, end_line, lines, return_count, conditionals, loops, cyclomatic, max_depth` で、`node_id` がない。515 ファイルを 1 つのリストで渡すと終わらなかった(主要な観察を参照)。マクロの定義を `duckdb_functions().macro_definition` で読むと、`functions` と `ast` の区間結合が `nf.node_id > f.func_id AND nf.node_id <= f.func_id + f.descendant_count` だけで、`file_path` の等号がない。node_id はファイルごとの番号なので、複数ファイルでは別ファイルのノードとも結合する。ファイルごとに呼ぶと、515 ファイルで計 9.6 s、ピーク RSS 309 MB、13,246 行だった。内訳は、`fn` トークンの行 6,623(name `fn`)、function_item 6,579、function_signature_item 37、macro_definition 7 である。スパイクの function と method は、(file, start_line, end_line, name) ですべてに対応する行があった。
- 文書の例は `s -> …` の lambda を使っているが、DuckDB 2.0 は既定でこれを拒否する(「Deprecated lambda arrow (->) detected」)。`lambda x: …` に書き換えて使った。
- `ast_qualified_name_as_string` はマクロで、内部で lambda 変数 `s` を使う。呼び出し側のクエリに `s` という表別名があると、`s.name` が呼び出し側の列に解決され、誤った文字列を返した。`b1.sql` で、スパイクの units を `s` と別名にしたときに起きた。別名を変えると正しく返る(`b3.sql`)。

## 再現の手順

```sh
git merge --ff-only origin/code-quality-survey
uv run --locked --script scripts/code-db/build.py build . --out target/code-db/horizon.duckdb --languages rust
uv run --script scripts/code-db/views/evaluate.py target/code-db/horizon.duckdb
# 時間の測定(コミットしていない補助スクリプト)
uv run --with duckdb==2.0.0 python -I target/code-db/sd/measure.py target/code-db/sd.duckdb [peek] [threads] [context]
```

## 未検証

- ページキャッシュが冷えた状態での `read_ast` の時間(root がなく、キャッシュを落としていない)。native の 1.2 s と 0.7 s の差の原因。
- `context := 'native'` が 2 乗で遅くなる箇所(name 抽出か、scope か、qualified_name の index 付けか)。上流の issue があるか。
- ERROR ノードが多いファイルや、MISSING ノードを含むファイルでの比較。fixture の構文エラーは 1 ファイルで、MISSING は 0 件だった。
- `parse_ast_list_table` の実行と、それが byte offset を返さないことの実測(文書の記述だけに拠っている)。
- 文字列から parse したときに、ファイルから読んだときと木が同じになるか(行数とノード数だけを見た)。
- `peek := 'full'` の時間とメモリ。
- Rust 以外の言語(Go・TypeScript)での、スパイクとの同じ比較。
- 1.5.6 で `ast_select_from` と `read_ast` を測った値(1.5.6 で測ったのは `ast_select` 1 ファイルの 2 回だけ)。
- trait impl の `name` が trait 名になるか型名になるかの規則。issue #276 や #147 との関係。
- `ast_definitions`(keymap.rs で 63 行を返すことだけを確かめた)と `ast_function_scope` の内容。
- `is_named` を型名から推す近似が、grammar 全体で成り立つ範囲(`node-types.json` との照合)。
- 2.0 のレジストリのビルドが `skip_tests: true` である点。本評価では、使った経路が動くことを見ただけである。

---

**追記(2026-10-24)**: ここで参照している Python のスパイク `scripts/code-db/`(`build.py`、`views/` の SQL、比較スクリプトを含む)は、実装の形をオーナーに相談せずに作ったものだったため、同日オーナーの指示「python版はそもそも相談されずに勝手に作ったものなので捨ててほしいです」により削除した。コードは履歴(最終 475599d5)に残る。本記録の数値と観察は削除前の測定として有効である。
