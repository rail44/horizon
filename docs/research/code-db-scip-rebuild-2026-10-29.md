# コードデータベースを今の木の SCIP 索引の上に作り直す(2026-10-29)

この記録は、コードデータベース(`docs/code-database-design.md`)を、HEAD の SCIP 索引から作る unit と、今の木についての品質の軸のビューに作り直し、履歴の仕組みを取り除いた増分の記録である。依頼はブランチ `code-quality-survey` の af0d26a7 からで、作業はワーカーのブランチで行った。設計文書の更新は依頼側に残している。

前提として決まっていたこと(2026-10-29 のオーナーの決定。依頼文による):

- 品質は「継続的にメンテナンスをする上での障害の少なさや機能の足しやすさ」であり、プロジェクトに依存する判断はビューではなく `.code-db.toml` に置く。
- 「SCIPが主でいいと想いますね」、そして「scipは推奨の形で導入しましょう」。unit は HEAD の SCIP 索引から作る。推奨の形として受け入れられたもの:impl は unit にしない(メソッドは `impl#[Type]`・`impl#[Type][Trait]` の記述子の下に掛かり、型への集約はそれを通す)、`mod x;` の宣言は unit にしない(ファイルのモジュールと SCIP のインラインのモジュールは unit)、`const`/`static` と関連型は unit、unit の範囲は SCIP の `enclosing_range`(前置きの doc と属性を含む)、限定名は SCIP の記述子に従う、crate は SCIP の package、テストはパスの規則と unit の範囲の本文への正規表現(`#[test]`、`#[cfg(test)]`、名前が `test` で終わる属性、`.code-db.toml` の `tests.paths`/`tests.extra_attributes`)を範囲の包含で継承し、`#[cfg(test)] impl` の中のメソッドを拾うために impl の見出しの行への規則を足す、`via_macro` はやめる、cfg で無効なコードは欠けてよい、ルートのワークスペースの外のファイルは Cargo のワークスペースごとに索引を作ってすべてを読む。
- 「tree-sitterはオミットしてもいいぐらいかな」、続いて履歴そのものについて「はい。それでよいです。」。blob の層、`unit_changes`、系譜、churn、co-change、fan-in × churn、クローンの片側、前後の比較(`compare_*`、`views/snapshot.sql`)と後ろ向きの確認のスクリプトは仕組みから外し、履歴は具体的な問いが要るときに足し直す。前後の比較は、のちに 2 つの SCIP 索引の障害の一覧の差として作り直す(この増分ではない)。複雑さと入れ子の深さは後の増分である(「保存したSCIPのクエリで表現できそう」)。
- 設定ではなく導くもの(先の決定):契約の型の候補は多くの crate から参照される型、意図した写しは両側の間の変換の impl(どちら向きでも `impl#[A][From<B>]` の形の記述子)で構造から印を付ける。網羅的な `match` の確認はしない(構文が要る)。
- 使わなくなった git のテーブル関数は取り除く(推奨が受け入れられた)。残るビューやクエリが読むもの(ファイルの集合の `git_tracked_files`)だけを残す。

## 方法と環境

- 事前確認と測定は、ワーカーの作業ツリーの `target/code-db/clone` に作った af0d26a7 の clone で行った(`git clone --no-checkout` のあと af0d26a7 を取り出した)。af0d26a7 と、tree-sitter の役割の記録が測った 7e84967d の間で変わった `.rs` は `duckdb-ext/code-db/` だけで、ルートのワークスペースの木は同じである。
- rust-analyzer 1.99.1(58cf8f9 2026-10-13)、DuckDB 2.0.0(CLI は `~/.local/bin/duckdb2`、Python の `duckdb` 2.0.0)、jscpd 5.3.1(リファクタリング監査の固定版)。索引を作ったあとは各ワークスペースの `target/debug` を消した。
- 機械はオーナーのデスクトップで、他のワーカーと共有している。時間は他の負荷の下での値で、すべて `nice -n 19` で測った。
- 測定用の SQL とシェルは作業ツリーの `target/code-db/scripts/` に置き、コミットしていない。

## 要約

- **事前確認は記録と同じ数になった。** af0d26a7 の SQL(tree-sitter の unit)と HEAD の索引で、索引にある 509 ファイルの unit 8,816 個のうち 7,732 個が SCIP の定義と 1 対 1 に対応し、種類は全件一致、`enclosing_range` の始まりは `lead_start_line` と全件一致(前置きを持つ 5,450 個を含む)、限定名はそろえたあと 7,680 個が一致した。記録の値と一つも違わない。
- **unit は SCIP の定義から作った。** 5 つのワークスペース(ルート、`duckdb-ext/`、`duckdb-ext/code-db/`、`duckdb-ext/host-check/`、`preview-plugin/`)の索引から、543 ファイルのうち 538 ファイルに 9,497 個の unit がある(function 4,983、method 1,948、type 1,129、module 766、constant 665、macro 6)。unit の無い 5 ファイルは cfg で無効なもの 3 個とワークスペースの外の fixture 2 個である。
- **テストの判定は、両方にある 8,243 個の unit で旧来のタグと全件一致した**(テスト 3,797、テストでない 4,446、食い違い 0)。tree-sitter の役割の記録が残した 2 個の取りこぼし(`#[cfg(test)] impl` の中)と 7 個の誤り(doc コメントの中の `#[cfg(test)]`)は、見出しの行の規則とコメント行を飛ばす規則で消えた。
- **impl の型は 571 個のうち 565 個が型の unit に解決した**(559 個は見出しの行の参照の出現から)。旧来の木の規則は 587 個のうち 549 個だった。
- **今の木の軸を 7 つのビューの層と 11 のクエリにした。** `size`、`duplication`、`coupling`、`module_graph`、`module_cycles`、`layer_violations`、`interface`、`public_surface`、`intent`、`unreferenced`、`unit_dependencies`。軸ごとの列を並べたビュー `unit_quality` は、どの列も組み合わせない。
- **履歴の仕組みを取り除いた。** SQL のビューとキャッシュの 8 ファイル、クエリ 12 本、`retro/` の 7 ファイル、Rust の git の 5 関数とその probe・テスト・パリティのスクリプト。削除は合わせて約 12,600 行、追加は約 2,600 行(新しいファイルを除く)。
- **永続キャッシュは、入力の状態ごとのファイルに導出の 19 層を写す形にした。** 作るのに 1.0〜2.3 s、`unit_quality` は生で 16〜19 s、写しの上で 0.13〜0.15 s だった。
- **索引はワークスペースごとに冷えた状態で合計 5 分 26 秒**(`code-db index`、ルート 186 s、`preview-plugin` 98 s、他は 7〜20 s、jscpd 2.8 s)。

## 1. 事前確認

af0d26a7 の clone で `rust-analyzer scip .` を走らせ(175.1 s、30,385,734 byte。記録と同じ大きさ)、af0d26a7 の SQL のビューを `since := 'HEAD'`(blob の層を空にするため)で読み、記録の 2.2〜2.4 節と同じ規則で比べた。対応の条件は、同じファイル、SCIP の種類を unit の種類に写したものが同じ、`display_name = name`、名前の位置が unit の範囲の中、`enclosing_range` の終わりの行 = `end_line`、そして 1 対 1 であること。

| unit の種類 | 索引にあるファイルの unit | SCIP と対応 | 種類が一致 | 始まり = `lead_start_line` | 前置きを持つ |
| --- | --- | --- | --- | --- | --- |
| function | 4,759 | 4,741 | 4,741 | 4,741 | 3,664 |
| impl | 551 | 0 | 0 | 0 | 0 |
| macro | 6 | 6 | 6 | 6 | 1 |
| method | 1,811 | 1,782 | 1,782 | 1,782 | 741 |
| module | 694 | 220 | 220 | 220 | 219 |
| type | 995 | 983 | 983 | 983 | 825 |
| 計 | 8,816 | 7,732 | 7,732 | 7,732 | 5,450 |

限定名(記述子から作り、impl の名前の型引数と経路をそろえたもの)は function 4,726、method 1,764、type 967、module 220、macro 3、計 7,680 / 7,732 が一致した。違う 52 個は記録のとおり、関数の中の item(記述子が囲む関数を持たない)と、属性マクロの下のトレイトの impl と、トレイトの別名だった。いずれの数も記録と一致したので、新しい `units` は旧来の unit の立っていた所から始まる。

## 2. 作ったもの

### 2.1 事実の源

- `00_load.sql`:拡張 2 つの `LOAD`(sitting_duck の `INSTALL`/`LOAD` は取り除いた)、ファイルの集合(`git_tracked_files` に `languages` と `scope.exclude` をかけたもの)、索引の一覧 `scip_indexes`(`target/code-db/scip/*.scip`)、jscpd の報告の一覧 `clone_reports`(あるものだけ)、設定の表。
- `views/files.sql`:`files`、`sources`、`source_lines(file_id, line, text, is_blank, is_comment, nonblank_through)`。
- `views/scip.sql`:索引の一覧を読み、各索引の文書の相対パスを、その索引の metadata の `project_root` から求めた接頭辞でリポジトリの相対パスにする(`scip_index_roots`)。ワークスペースの間で同じ文書を持つ索引は無かった(各索引は自分のワークスペースのファイルだけを持つ)。
- `scip_index` 拡張を変えた:3 つの関数は 1 つのパスかパスのリストを取り、行の最後に `index_path` の列を持つ。`read_scip_symbols` は `signature`(`signature_documentation` の `text`)の列を持つ。`scip print --json` とのパリティ(`cli_parity.rs` の無視しているテストを含む)と、拡張と `scip-probe` の一致(`scip_ext_parity.py`、301,687 / 42,839 / 1 行)は、HEAD の索引で保たれた。

### 2.2 `units`

`units(unit_id, file_id, path, crate, kind, symbol_kind, name, qualified_name, symbol, parent_unit_id, start_line, start_column, end_line, end_column, name_line, name_column, loc, is_test, visibility, parameters, signature, has_doc, is_file_module, member_of, impl_key, impl_type, impl_trait, impl_header_line)`。

- 1 つの unit は、ソースファイルの中の、局所でない記号の定義の出現 1 つである。SCIP の種類の写し方:Function → `function`、Method・StaticMethod・TraitMethod → `method`、Struct・Enum・Union・TypeAlias・Trait・AssociatedType → `type`、Module → `module`、Macro → `macro`、Constant・StaticVariable → `constant`。
- 範囲は `enclosing_range`、名前の位置は `name_line`/`name_column`、`parent_unit_id` は範囲を包む他の unit のうち最も内側のもの。`loc` は範囲の空でない行。
- `qualified_name` はファイル自体のモジュールの記号より下の記述子を `.` で結んだもの(`Type.m`、`Trait for Type.m`、`tests.f`)。
- `visibility` と `parameters` は `signature` から取る。`has_doc` は SCIP の `documentation` の有無。
- impl のメンバーは記述子の `impl#[...]` を `impl_key`・`impl_type`・`impl_trait` として持ち、`member_of` は `impl`・`trait_impl`・`trait`(トレイト自身の item)である。
- `is_test` は依頼文の規則どおり(細部の判断は 3 節の 9)。

### 2.3 グラフ

`views/graph.sql` の `unit_occurrences`、`symbol_units`、`unit_references`、`unit_edges`、`unit_contains`、`unit_ancestors`、`impls`、`impl_targets`、`impl_for`、`implements`、`unit_impl_type`、`modules`、`unit_module`、`module_edges`、`crate_edges`、`module_cycle_edges`、`module_cycles`、層の 4 つ、`crate_cycles`。出現は unit の範囲(行と列)の包含で最も内側の unit に置く。旧来の列のうち `via_macro`、`macro_occurrences`、`macro_unit_edges` は無くなった。`impl_for`・`implements` は impl の記述子の名前から解決する(3 節の 7、8)。

### 2.4 軸のビューとクエリ

`views/clones.sql`(`clone_pairs`、`clone_sides`)と `views/quality.sql`(`unit_scope`、`unit_fan_in`、`unit_fan_out`、`unit_reference_status`、`unit_debt_markers`、`unit_clones`、`unit_quality`)が unit ごとの事実を持ち、クエリが軸ごとの問いを聞く。どのクエリの見出しも、問い、軸、証拠の強さ(軸の記録の 5 節)、設定から読む判断、パラメータを書く。

| 軸 | クエリ | 列 | 証拠 | 設定から読む判断 |
| --- | --- | --- | --- | --- |
| 大きさ | `size`(`level` が unit・file・crate) | level, crate, path, kind, qualified_name, start_line, loc, units, is_test, test_loc_pct | 中 | scope、tests |
| 重複 | `duplication` | pair_id, source, lines, tokens, 両側の file・行・kind・unit・is_test, same_unit, conversion_pair | 中 | scope(jscpd の範囲も)、tests |
| 結合 | `coupling`、`module_graph` | fan_in_files/crates/units, fan_out_files/crates/units, contract_candidate(`min_crates`、既定 3) | 中 | scope、tests |
| 独立性 | `module_cycles`(`violating_edges`)、`layer_violations` | 旧来と同じ列からマクロの列を除いたもの | 弱 | `[[layers]]`、`may_depend_on` |
| interface | `interface`、`public_surface` | parameters, visibility, fan_in_crates, signature / pub_units, used_from_other_crates, pub_unused_outside, other_crates | 弱 | scope、tests |
| 意図(機械的な部分) | `intent` | has_doc, debt_lines, fan_in_crates, fan_in_files | 弱〜なし | scope、tests |
| 参照されない定義 | `unreferenced` | status(`unreferenced`、`test`、`trait_impl`、`entry_point`、`crate_root`、`trait_item`) | 弱 | scope、tests |

`unit_quality` は unit ごとに、同一性、`loc`、クローンの組の数、fan-in・fan-out、モジュールの循環と層、可視性と引数、文書と負債の印、参照の数と状態を並べる。点数は作っていない。

### 2.5 キャッシュ、`code-db`、確認

- `refresh.sql`:鍵(`repo`、ファイルの集合、各索引のパスと内容のハッシュ、テストの 2 つの変数、すべてのビューとマクロの本文)の md5 を名前にしたファイル `target/code-db/cache/<md5>.duckdb` に、`scip_occurrences` から `unit_debt_markers` までの 19 層を順に写す。同じ鍵のファイルがあれば付けるだけで、鍵が変われば新しいファイルを作る。`code-db` は読み込みのあと他の鍵のファイルを消す。
- `code-db index`:`00_load.sql` と `views/files.sql` でファイルの集合と追跡されている `Cargo.toml` を取り、jscpd にそのファイルを渡し(`jscpd.sh` の既定の走査パスは無くなった)、`[workspace]` を宣言してソースファイルを持つディレクトリごとに `rust-analyzer scip` を走らせて `target/code-db/scip/root.scip`・`ws-<パス>.scip` に書く。`query` は、見出しに「Needs the SCIP indexes」「Needs the jscpd reports」と書いたクエリについてだけ入力を確かめる。重複を読まないクエリが jscpd の報告を要求する問題はこれで直った。
- `check.py`:fixture を作り直した(2 つの crate のワークスペースと入れ子のワークスペース、ワークスペースの外のファイル、除外するファイル。rust-analyzer の本物の索引、jscpd の形の報告、設定の表)。`files`、`units`、`impl_targets`、`unit_edges`、`modules`、`crate_edges`、`module_cycles`、`unit_quality` とすべてのクエリの全行、層を空にしたときと二重にしたとき、キャッシュと生の一致、鍵の保持と作り直し、索引と報告が無いときの空のビューを確かめる。`repo`、`refresh`、`measure` のコマンドを残し、他は取り除いた。
- `code-db` の end-to-end テストは `index` を、代わりの jscpd(引数を記録して報告を書くシェル)と本物の rust-analyzer で走らせ、2 つの索引、jscpd に渡ったファイル(除外したファイルが無い)、層の違反、循環の `violating_edges`、設定のテストの規則、重複の組、報告の無いときに `size` が動き `duplication` が止まることを確かめる。

## 3. 依頼文が決めていなかった判断

1. **unit の種類の名前。** Constant と StaticVariable をまとめて `constant`、AssociatedType を `type` にした。SCIP の種類は `symbol_kind` に残した。
2. **マクロが作る item。** `actions!` の Struct のように SCIP が定義を持つものは unit にした。範囲はマクロの呼び出し全体で、同じ範囲の unit は親子にせず兄弟とした(出現は unit_id の大きい方に置かれる)。
3. **crate の根の名前。** 根のモジュールの `display_name` は空なので、`name` と `qualified_name` を package の名前にした。ファイルのモジュールの `qualified_name` はその名前、他の unit はファイルのモジュールより下の記述子である(関数の中の item は関数の名前を持たないので、同じファイルで名前が重なることがある。`symbol` は一意)。
4. **出現の置き場所。** 行だけでなく列も見る包含にした。ファイルのモジュールが unit になったので、どの item にも入らない出現(`use`、impl の見出し、ファイルの水準のマクロ呼び出し)はファイルのモジュールに置かれる。
5. **モジュールの記号への参照は辺にしない。** パスの途中(`crate::a::b::f` の `a`・`b`)や `use crate::a;`、`mod a;` はモジュールの記号の参照として索引にあり、辺にすると通るモジュールすべてへの辺になる(試すと horizon-agent の循環が 110 モジュールになった)。旧来の規則ではファイルのモジュールの定義がどの unit にも入らなかったので、同じ効果がある。
6. **`module_path` はパスの規則のまま。** SCIP は package のどのターゲットの根も `crate/` と名付けるので、記述子で入れ子を決めると結合テストの根がライブラリのすべてのモジュールの祖先になる。モジュールの名前(`<path>`、`<path>::<name>`)も旧来のままで、層のパターンはそのまま使える。
7. **impl の見出しの行。** impl のメンバーごとに、その上にある最も近い `impl` で始まる行で、`impl_type` の名前を含み、メンバーを含まない他の関数の本体の中にないものを見出しとした。`#[cfg(test)]` の判定と `impl_for` の解決の両方に使う。
8. **`impl_for`・`implements` の解決。** 名前(型引数・経路・`&`・`dyn` を除いたもの)が同じ型の unit のうち、見出しの行で参照されているもの、なければ同じファイル、同じ crate、リポジトリでただ 1 つのもの。どれでもなければ NULL。
9. **テストの規則の細部。** 先頭の範囲(範囲の始まりから名前の行まで)のうちコメントの行を飛ばして属性を探す。依頼文の挙げた名前のほかに `cfg(all(…test…))` を含めた。旧来の規則にあった `test_case` と `bench` は「名前が `test` で終わる」に入らないので外した(このリポジトリには無い)。ファイルのモジュールは、ファイルのどこかに `#![cfg(test)]` があればテストとした。
10. **`visibility` と `has_doc` の NULL。** トレイト自身の item とトレイトの impl のメンバーの可視性はトレイトに従うので NULL。rust-analyzer はトレイトの impl のメンバーにトレイトの item の doc を付けるので、その `has_doc` も NULL にした(fixture の `From::from` で確かめた)。
11. **引数の数。** `self` を含め、シグネチャの一番外側の括弧の中のカンマで数えた(`->` と `=>` は括弧の深さから外した)。
12. **TODO/FIXME。** 行のどこにあっても数え(コメントと文字列を区別しない)、その行を含む最も内側の unit に数える。item の前の `//` のコメントは `enclosing_range` に入らないので、囲む unit に数える。
13. **参照されない理由の分類。** 依頼文のトレイトの impl のメンバー・`main`・テストのほかに、crate の根のモジュール(`crate_root`)とトレイト自身の item(`trait_item`。impl がそれを実装し、具体的な型の上の呼び出しは impl の item に解決される)を足した。
14. **fan-in と fan-out の範囲。** unit 自身、それが含む unit、型ならその impl のメンバー(とそれが含むもの)を unit の範囲(`unit_scope`)とし、範囲の外からの参照を数える。モジュールの fan-in はそのモジュールの中の何かを参照するファイルの数になる。
15. **契約の型の候補の既定。** `min_crates` の既定は 3(自分の crate を除く crate の数)。クエリのパラメータで、設定ではない。
16. **変換の印。** `From`・`Into`・`TryFrom`・`TryInto` の impl で、片側の型(型そのもの、またはその impl の中の unit)ともう片側の型の名前が impl の型と型引数に一致するもの。もう片側は名前で照らしている。
17. **大きさのクエリ。** 1 本で `level` の 3 段にした。crate の水準の `test_loc_pct` は、一番外側のテストの unit(テストのファイルのモジュール、`mod tests`、テストでないモジュールの中のテスト関数)の `loc` の割合である。
18. **公開面を別のクエリにした。** `interface` は unit ごと、`public_surface` は crate ごとの集計である。
19. **キャッシュの形。** 層ごとの差分の更新はやめ、入力の状態ごとのファイルを丸ごと作る形にした。作るのに 1.0〜2.3 s で、差分の仕組みに見合わない。セッションだけのキャッシュ `cache.sql` は取り除いた。
20. **索引の名前と見つけ方。** ルートは `root.scip`、他は `ws-<パスの / を + にしたもの>.scip`。`[workspace]` か `[workspace.…]` の行を持つ追跡された `Cargo.toml` のディレクトリで、ソースファイルを 1 つ以上持つもの。入れ子のワークスペースもそれぞれ索引にする。古い `target/code-db/index.scip` はもう読まない。
21. **報告と索引が無いとき。** `00_load.sql` があるものだけを一覧にし、ビューはどれも読める(空になる)。`read_json` と `read_blob` は少なくとも 1 つのファイルを要るので、重複の無い報告 `duckdb-ext/sql/empty-jscpd-report.json` をいつも一覧に足している。
22. **拡張の引数。** リストを受けるために、引数の型を `ANY` にして bind のときに型を見る形にした。用意された文の `?` でもそのまま動く(host-check で確かめた)。
23. **gix の機能。** 履歴の関数だけが使っていた `blob-diff` と `revision` を外した。`max-performance` は残した。
24. **`tracked_parity.py` も取り除いた。** 依頼文は `gix-probe` を取り除くものに挙げており、このスクリプトはその出力を読む。`git_tracked_files` の git CLI との比較は `git-history-core/tests/tracked_files.rs` の fixture に残る。実際のリポジトリでのパリティは、これからは測れない。

## 4. 測定

### 4.1 unit の数

| ワークスペース | ファイル | unit |
| --- | --- | --- |
| ルート | 509 | 8,910 |
| `duckdb-ext/` | 18 | 426 |
| `duckdb-ext/code-db/` | 8 | 148 |
| `duckdb-ext/host-check/` | 2 | 12 |
| `preview-plugin/` | 1 | 1 |
| 計 | 538(543 のうち) | 9,497 |

| SCIP の種類 | unit の種類 | 数 |
| --- | --- | --- |
| Function | function | 4,983 |
| Method / StaticMethod / TraitMethod | method | 1,558 / 353 / 37 |
| Struct / Enum / TypeAlias / AssociatedType / Trait | type | 765 / 261 / 44 / 43 / 16 |
| Module | module | 766(うちファイルのモジュール 538) |
| Constant / StaticVariable | constant | 630 / 35 |
| Macro | macro | 6 |

unit の無いファイル:`crates/horizon-sandbox/src/macos/denials.rs`、`crates/horizon-sandbox/src/macos/mod.rs`、`src/preview/guest.rs`(cfg で無効)、`scripts/refactor-audit/fixtures/boundaries.rs`、`scripts/refactor-audit/fixtures/clone.rs`(どのワークスペースにも無い)。`preview-plugin/src/lib.rs` は wasm のゲストのマクロ 1 つだけのファイルで、ファイルのモジュールだけが unit になった。

### 4.2 テストの判定

旧来の unit(af0d26a7 の tree-sitter の unit と `unit_tests`、全 535 ファイルで 9,427 個、テスト 3,948)と新しい unit(9,497 個、テスト 3,945)を、同じパス・種類・名前・終わりの行・始まり(旧来の `lead_start_line`)で結ぶと 8,243 組が 1 対 1 に対応した。そのうちテストは両方で 3,797、テストでないものは両方で 4,446、食い違いは 0 だった。種類別のテストの数は、新しい側で function 3,241、method 221、module 300、type 115、constant 68。

### 4.3 impl の解決とグラフ

| 部分 | 解決の仕方 | 数 |
| --- | --- | --- |
| type | 見出しの行 / 同じファイル / 名前がただ 1 つ / 解決しない | 559 / 5 / 1 / 6 |
| trait | 見出しの行 / 同じファイル / 同じ crate / 解決しない(外部のトレイト) | 38 / 4 / 7 / 164 |

unit の辺 40,523 本(曖昧 642)、モジュールの辺 3,840 本(入れ子 666、同じ型の impl の間の辺を持つもの 71)、モジュールの循環 25、crate の循環 0。

ルートのワークスペースの範囲で旧来のモジュールの辺と比べると、旧来の 3,517 本のうち 3,516 本が新しい側にもあり(`nested_modules` の値も全件一致)、旧来にしか無い 1 本は `session/setup.rs::tests::real_git` → `session/setup.rs::tests` だった。新しい側にしか無い 264 本は、ファイルのモジュールから始まるもの 215 本(`use`、`pub use`、impl の見出しの参照)と、constant を宛先にするもの(関数から 57 本など)が大部分で、どちらも旧来の unit には無かった出発点・宛先である。最大の循環は 25 モジュール(`judge/handle.rs`)から 40 モジュールになり、そのうち旧来に無い辺は 4 本(`event_log/mod.rs` の再公開、`tools/bash/mod.rs` の再公開、`completion.rs` と `catalog.rs` から定数への参照)だった。旧来の `providers/rig/clearing.rs`(5)と `config.rs`(3)の循環はこの 40 の中に入り、新しく `auxiliary.rs`(5)と `horizon-agentd/src/main.rs`(2)の循環が出た。

### 4.4 軸ごとの上位(判断はしない)

af0d26a7 の木で `code-db query … LIMIT=10` の先頭を見た。並びが軸の意味どおりかの確認であり、障害かどうかは読んでいない。

- `size`(unit):`build_definitions`(344 行)、`WorkspaceShell.render_node`(326)、`Render for WorkspaceShell.render`(291)、`box_draw_geometry`(286)、`paint_terminal`(266)、`run_session`(253)、`core_commands`(245)、`respond_to_user_message`(241)、`Mapper.map_event`(233)、`scheme_from`(229)。file の水準は `persistence/event_log/writer.rs`(1,687)、`session/resume.rs`(1,552)、`terminal/scrollback.rs`(1,529)。crate の水準は horizon-agent(62,209、テストの割合 51.8 %)、horizon(41,737、31.8 %)、horizon-agentd(16,907、54.9 %)。
- `duplication`:normalized の先頭は同じ unit の中の自己重複(`long_report` の 636〜819 行と 637〜820 行、`same_unit`)と、`src/runtime/agent.rs` と `terminal.rs` のモジュールの水準の 146 行。exact の先頭は `transcript/receipt.rs` の `aggregate_receipt` と `src/agent/model/transcript.rs` の同名の関数(45 行)、同じ組の `affected_files`、`group_into_turns`、そして `palette.rs` と `view_chooser.rs`・`session_manager.rs` のモジュールの水準の組。
- `coupling`:`contract.rs` のモジュール(144 ファイル)、`SessionId`(107 ファイル、4 crate、`contract_candidate`)、`Event`(89)、`ToolCallId`(68)。型に絞ると上位 10 のうち `contract_candidate` は `SessionId` だけで、他は 1 crate からの参照だった。
- `module_graph`:テストのモジュールから本体への辺が上位を占める(`horizon-terminal-core/src/tests.rs` → `core.rs` の 357 本など)。
- `module_cycles`:4.3 節の 40 モジュールの循環で、unit の辺が 1 本だけのモジュールの辺が先頭に並ぶ。`layer_violations` は設定が無いので 0 行。
- `interface`:`paint_scrollback_window`(15 引数)、`run_session`(12)、`complete_rig_turn`(10)、`SessionLoopState.new`(10)、`run_provider_stream`(9)。
- `public_surface`:horizon-agent は `pub` の unit 344 のうち 211 が他の crate から参照され、134 は参照されない。horizon-wire は 48 / 40 で 9 crate から使われる。
- `intent`:負債の印のある unit は 3 個だけで、すべてテストの中だった(既定では出ない)。先頭は他の crate から使われる文書の無い `pub` の unit(`SessionId.new`、horizon-board の `Item`・`Comment` など)。
- `unreferenced`:`code-db` と horizon-cli の `USAGE` 定数(`"{USAGE}"` のように書式文字列の中だけで使われ、rust-analyzer はそこに参照を記録しない)、`deserialize_lenient_f64`(serde の `deserialize_with` の文字列から使われる)、`src/preview/schema.rs` の属性マクロの下のメソッド。この種の誤りの仕組みをクエリの見出しに書き足した。

### 4.5 時間

`check.py measure`(af0d26a7 の clone、1 回ごとに新しいプロセス、3 回の最小〜最大)。「生」は `refresh.sql` を読まない、「作る」はキャッシュのディレクトリを空にしてから読む、「保持」は同じ鍵のファイルがある状態で読む。読み込み(ビューのファイル)は 1.1〜3.5 s、キャッシュを作るのは 1.0〜2.3 s、付けるだけなら 0.08〜0.24 s だった。

| クエリ | 行 | 生 | 作ったあと | 保持 | 生のピーク RSS |
| --- | --- | --- | --- | --- | --- |
| `size` | 5,086 | 0.43–0.44 s | 0.03–0.04 s | 0.04 s | 0.6 GB |
| `duplication` | 370 | 3.06–3.07 s | 0.22–0.24 s | 0.22–0.25 s | 3.0 GB |
| `coupling` | 5,453 | 8.14–8.26 s | 0.02 s | 0.02–0.03 s | 5.5 GB |
| `interface` | 3,469 | 4.01–4.03 s | 0.02 s | 0.02 s | 3.5 GB |
| `public_surface` | 24 | 0.76–0.81 s | 0.03–0.04 s | 0.03 s | 1.0 GB |
| `intent` | 247 | 4.24–4.29 s | 0.01 s | 0.01 s | 3.7 GB |
| `unreferenced` | 27 | 0.47–0.53 s | 0.00–0.01 s | 0.01 s | 0.8 GB |
| `module_graph` | 3,840 | 3.01–3.12 s | 0.02 s | 0.02 s | 2.8 GB |
| `module_cycles` | 205 | 13.69–14.12 s | 0.07–0.08 s | 0.07–0.08 s | 6.1 GB |
| `layer_violations`(層なし) | 0 | 9.68–19.13 s | 0.02–0.04 s | 0.02 s | 6.8 GB |
| `unit_dependencies` | 22 | 4.75–5.46 s | 0.02–0.03 s | 0.03 s | 5.5 GB |
| `unit_quality` | 9,497 | 15.86–18.91 s | 0.14 s | 0.13–0.15 s | 7.7 GB |

キャッシュを読んだプロセスのピーク RSS は 0.2〜0.75 GB だった。`code-db query … LIMIT=10` は 1 回 2.9〜4.4 s(4.4 節の各行)で、その大部分は読み込みである。

### 4.6 索引の時間と大きさ

| ワークスペース | 最初の索引(手で、冷えた状態) | `code-db index`(冷えた状態) | 索引の大きさ |
| --- | --- | --- | --- |
| ルート | 175.1 s | 186.4 s | 30,385,734 byte |
| `duckdb-ext/` | 12.4 s | 19.9 s | 1,362,542 byte |
| `duckdb-ext/code-db/` | 8.6 s | 12.1 s | 465,071 byte |
| `duckdb-ext/host-check/` | 7.4 s | 6.5 s | 57,514 byte |
| `preview-plugin/` | 63.5 s | 98.2 s | 713 byte |

`code-db index` は jscpd(543 ファイル)2.8 s を含めて合計 325.9 s だった。メモリの上限は `prlimit --as`(MemAvailable の 80 %、33.6 GB)で、この環境では `systemd-run --user` が使えなかった。2 つの列の違いは他の負荷によると考えるが、切り分けてはいない。

## 5. 取り除いたもの

| 場所 | ファイル(行数) |
| --- | --- |
| SQL のビュー | `views/nodes.sql`(28)、`views/blobs.sql`(94)、`views/history.sql`(23)、`views/unit_changes.sql`(184)、`views/lineage.sql`(465)、`views/snapshot.sql`(116)、`lang/rust.sql`(447)、`cache.sql`(123) |
| クエリ | `unit_churn`(93)、`unit_cochange`(35)、`cochange_without_reference`(26)、`file_cochange`(53)、`file_cochange_thresholds`(47)、`unit_fan_in_churn`(61)、`clone_change_consistency`(199)、`clone_site_tags`(45)、`clone_site_tags_summary`(42)、`compare_units`(144)、`compare_graph`(55)、`compare_cycles`(77) |
| 後ろ向きの確認 | `retro/changes.sql`(24)、`retro/compare.sql`(134)、`retro/lineage.sql`(158)、`retro/signals.sql`(341)、`retro/signals_by_name.sql`(16)、`retro/september-2026-record-preserves.csv`(15)、`retro/september-2026-record-targets.csv`(12) |
| 書き直したもの | `refresh.sql`(617 → 122 行。blob・履歴・系譜の層と差分の更新を除いた)、`check.py`(2,996 → 1,031 行。履歴・系譜・数え方・比較・blob の fixture と `cli`・`explain`・`measure-refresh` のコマンドを除いた)、`views/graph.sql`(履歴の co-change、前の側のグラフ、`graph_*_over` のマクロを除いた) |
| Rust | `git-history-core/src/blobs.rs`(219)、`src/tree.rs`(70)、`src/lib.rs` の履歴の部分(490 → 20 行)、`tests/cli_parity.rs`(592)、`tests/changed_blobs.rs`(613)、`tests/tree.rs`(256)、`git-history/src/functions.rs` の 5 関数(765 → 195 行)、`gix-probe/`(161)、`host-check/tests/load.rs` の `git_hunks`・`git_commits`・`git_tree` の部分 |
| スクリプト | `scripts/gix_parity.py`(317)、`cli_hunks.py`(48)、`cli_file_changes.py`(55)、`tracked_parity.py`(122)、`load_smoke.py`(21)、`perf.py`(22) |

残った git のテーブル関数は `git_tracked_files` だけで、`00_load.sql` のファイルの集合と `code-db index` の `Cargo.toml` の一覧が読む。

## 6. 記録や依頼文と食い違った点、気づいた点

- 依頼文の「履歴を仕組みから外す」に対して、`refresh.sql` の差分の更新の仕組み(ファイルごと、blob ごと)は残す理由が無くなった。キャッシュ全体を作り直しても 1.0〜2.3 s である。
- tree-sitter の役割の記録 2.8 節の取りこぼし 2 と誤り 7 は、見出しの行の規則とコメント行を飛ばす規則で 0 になった(4.2 節)。記録の時点の規則より範囲を絞っている(範囲全体でなく、範囲の始まりから名前の行まで)。
- 記録 2.6 節は「`impl_for` を SCIP だけで作ると 526 個のうち 491 個が一意、31 個が曖昧」とした。見出しの行の参照の出現を使うと 571 個のうち 559 個が一意に決まり、曖昧なまま残るものは無かった(解決しない 6 個は候補が無い)。見出しの行は本文の規則で見つけており、構文は使っていない。
- モジュールの辺が 264 本増え、最大の循環が 25 から 40 モジュールになった(4.3 節)。増えた辺はファイルの水準の参照(`pub use` の再公開、impl の見出し)と定数への参照で、旧来の unit がどちらも持たなかったものである。どちらを数えるのが目的に合うかは判断していない。
- rust-analyzer はトレイトの impl のメンバーにトレイトの item の doc を付ける(`has_doc` を NULL にした理由)。書式文字列の中の識別子(`"{USAGE}"`)と属性の文字列(`deserialize_with`)には参照の出現を書かない(`unreferenced` の誤りの源)。
- `preview-plugin/` の索引は 713 byte で、unit はファイルのモジュール 1 つだった。ホストの cfg では、このワークスペースのコードはほとんど無効である。

## 7. 限界

- 測ったのは af0d26a7 の木 1 つだけである。
- 新しい unit と旧来の unit の対応は、テストの判定を比べるための結び方(パス・種類・名前・行)で、記録の事前確認と同じ 1 対 1 の条件ではない。旧来にしか無い unit(impl、`mod x;`)と新しい側にしか無い unit(定数、関連型、他のワークスペース)は比べていない。
- 変換の印は名前で照らしているので、同じ名前の型が複数あると誤る。fixture では 1 組だけ確かめた。
- 軸の上位は読んだだけで、障害かどうかの判定は行っていない。
- 複雑さと入れ子の深さ、網羅的な `match` の確認はこの増分に無い。
