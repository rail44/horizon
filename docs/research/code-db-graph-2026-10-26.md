# 参照・構造・モジュールの辺をビューにした記録(2026-10-26)

コードデータベースの SQL 層の 6 つ目の増分である。これまでの層には集計(fan-in の数、churn、co-change の数)しかなく、辺が無かった。この増分では、すでにある事実(`units`、`tags`、`scip_occurrences`、`unit_changes`)の上に、unit から unit への参照、入れ子・impl の対象・実装するトレイトという構造の辺、モジュールとクレートの辺と循環、辺の上の co-change をビューとして導き、グラフをたどるクエリを足した。観察だけを記し、推奨は書かない。

オーナーは 2026-10-25 に目的をこう述べた。「そもそも、作りたいのってリファクタリングの対象を探したりするために、コードを依存や構造のグラフで把握したうえで判定することですよね」。依頼側が示したこの増分の形に、オーナーは「この形で良いので進めましょうか」と答えた。

前提は [設計文書](../code-database-design.md)、前の記録 [code-db-scip-2026-10-25.md](code-db-scip-2026-10-25.md)(以下「SCIP の記録」)と [code-db-unit-changes-2026-10-25.md](code-db-unit-changes-2026-10-25.md) である。作業は worker の作業ツリーで `origin/code-quality-survey` の 0ffe4645 から始めた。2026-10-25 に始め、依頼側のログインの期限切れで作業が一度中断し、2026-10-26 に再開して終えた。中断の前後で作業ツリーの変更は失われていない(再開時に `git status` で確かめた)。中断の前に取ったのは `check.py repo`・`refresh`・`cli`・`explain` と `measure` の各ビュー・クエリの時間と本文の数で、後に取ったのは `measure` の co-change の 3 項目の再測定、`measure-refresh`、fixture と `repo` のやり直しである。どれも同じ木・同じ索引の上で、中断の直前に変えたのは 2 つのクエリの並びの鍵(5 節)と反結合の書き方(6 節)、中断の後に変えたのは `check.py repo` にクレートの循環が空であることの検査を足したことだけである。

機械は前回までと同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0、sitting_duck は v1.16.0、rust-analyzer は 1.99.1 である。Rust は変えていない。

## 主要な観察

- **unit の辺は 34,195 本(自己辺 58 本を含む)、モジュールの辺は 3,517 本、クレートの辺は 35 本だった。** 参照の行(記号ごと)は 53,812 行、出現の数は 73,271 である。
- **モジュールの循環は 31 個、計 166 モジュール、内側の辺 354 本だった。** 最大は `crates/horizon-agent/src/judge/` の 40 モジュールである。うち 3 個は、同じパッケージの別ターゲット(2 つの結合テスト、2 つのバイナリ)に同じ名前の関数があり、rust-analyzer がそれらを 1 つの記号にすることから生じた見かけの循環だった(3 節)。
- **クレートの循環は 0 個だった。**
- **impl 585 個のうち、対象の型が unit に解決したのは 549 個、トレイト impl 231 個のうちトレイトが unit に解決したのは 45 個だった。** 残りは外部のトレイト・型、型パラメータ、プリミティブである。
- **`local` 記号を含めると、unit の組が 5,973 組足される。** うち 5,944 組は自己辺、異なる unit の組は 29 組だった。
- **`SessionId` の型 unit に入る辺の出どころのファイルは 105 個で、`unit_fan_in_churn` の 107 と 2 つ違った。** 違う 2 つは `crates/horizon-acp/src/lib.rs` と `crates/horizon-wire/src/lib.rs` で、どちらも参照がファイルの直下の `pub use` にしかなく、どの unit にも入らない。
- **トレイトのメソッドの呼び出しの解決は、依頼文の前提と一部違った。** 具体的な型の値の上の呼び出し(`square.area()`)は impl のメソッドに、ジェネリックの境界を通した呼び出し(`s.area()`、`S: Shape`)はトレイトのメソッドに解決した(2 節)。
- **すべてのビューは、どちらのキャッシュの上でも 0.01〜0.4 s で、ビューのままにした。** 例外はセッションキャッシュの上の `cochange_without_reference` の 1.1〜1.7 s である。ライブでは、履歴を読まないビューが 0.7〜1.6 s、co-change のビューが 12.6〜17.3 s だった。

## 1. 組んだもの

| ファイル | 変更 |
| --- | --- |
| `views/graph.sql`(新規、省いてよい、`views/scip.sql` の後) | `unit_occurrences`、`unit_tests`、`unit_references`、`unit_edges`、`unit_contains`、`unit_ancestors`、`impl_targets`、`impl_for`、`implements`、`file_crate`、`modules`、`unit_module`、`module_edges`、`crate_edges`、`module_cycle_edges`、`module_cycles`、`crate_cycles`、`unit_commits`、`unit_cochange`、`edge_cochange`、`cochange_without_reference` |
| `lang/rust.sql` | `rust_impl_parts`(impl のトレイトの部分と型の部分の範囲) |
| `views/units.sql` | `impl_parts`(言語をまたぐ和。今は Rust だけ) |
| `queries/`(新規 5 つ) | `unit_dependencies`、`module_graph`、`module_cycles`、`unit_cochange`、`cochange_without_reference` |
| `check.py` | グラフの fixture、`views/graph.sql` を `views/scip.sql` と一緒に読むこと、更新の検査・CLI・測定の項目 |
| `README.md` | Graph の節、読む順、配置、パラメータ、検査 |

`cache.sql` と `refresh.sql` は変えていない(4 節)。

### 出現を unit に置く規則

出現が unit の中にあるとは、出現の行(開始と終わりの両方)が unit の行の範囲 `[start_line, end_line]` に収まることとした。収まる unit のうち、`unit_id` が最大のもの、つまり最も内側の unit に置く。unit は構文木のとおり入れ子になり、`unit_id` は外側の unit を中身より先に番号づけるので、入れ子の鎖の中では最大の id が最も内側である。終わりの行も見るのは、rust-analyzer がファイルモジュールの定義の出現をファイル全体の範囲(1 行目 1 列から最終行の次まで)で書くためである。開始だけを見ると、ファイルの 1 行目から始まる unit にモジュールの定義が入ってしまう。

unit は行しか持たないので、入れ子でない 2 つの unit が 1 行を共有すると(`fn a() {} fn b() {}`)、その行の出現は両方に収まり、後ろの unit に置かれる。依頼文はこの場合の決め方を書くか、ノードからバイトの列を足すかを求めていた。この repo で、入れ子でない unit の組で行が重なるものを数えると 0 組だったので、行だけの規則にして、決め方をビューの注釈と README に書いた。unit の最初か最後の行にあって unit の外にある出現(同じ行で `fn` より前のコード)も、その unit に置かれる。`generated_or_macro` と `test` のタグの判定も、これまでの規約どおり行で行う。

### unit の参照

`unit_references` は、`local` でない記号ごとに、参照の出現を持つ unit(元)と定義の出現を持つ unit(先)の組を 1 行にする。`occurrences` は元の unit の中の参照の出現の数、`macro_occurrences` はそのうちマクロ呼び出しの中にあるものの数、`via_macro` はすべてがマクロの中にあるときに真である。依頼文の `via_macro` は「元の出現がマクロのノードの中にある辺」だったが、記号ごとにまとめた行では中と外が混ざりうるので、「すべてが中」とし、数も列に残した。`src_is_test`・`dst_is_test` は、unit の行を `test` のタグのノードが含むかどうかである(`unit_tests`)。`unit_edges` は記号をまとめたもので、`symbols` は記号の数、`is_self` は元と先が同じ unit であることを示す。

定義の unit が複数ある記号は 10 個、それらによる参照の行は 122 行、辺は 118 本だった。依頼文のとおり、すべての定義の unit に辺を張り、`ambiguous` の列で区別した。10 個は次のとおりで、8 個は同じパッケージの別ターゲットに同じ名前の項目がある場合である(rust-analyzer の記号はパッケージ名と版で名づけられ、ターゲットを含まない)。

- `horizon-terminald` の結合テスト `tests/e2e.rs` と `tests/latency_probe.rs` の `spawn_terminald`・`connect_hub`・`current_frame`・`terminal_spec`
- `horizon-terminal-core` の `tests/limits.rs` と `tests/frame_watch.rs` の `Conn`
- `horizon-sandbox` の 2 つのバイナリの `fail`、`main`、`horizon-agent` の `main`
- 同じファイルの中に 2 つある `horizon-agent` の `model_limits/CACHE` と `horizon-agentd` の `REBUILD_DELAY_MS`

### 構造の辺

`unit_contains` は `parent_unit_id` そのもの、`unit_ancestors` はその推移閉包(深さ 0 は自分)である。

`impl_for` と `implements` は `impl_targets` から作る。依頼文の案は、impl の見出しの行の出現のうち型の unit を指すものを順に取り、トレイト impl では先のものをトレイト、後のものを型とするものだった。これを先に試すと、トレイトの側で誤りが多かった(5 節)。`impl From<Foo> for Bar` では、`From` は外部の記号で unit が無いので、最初に型の unit を指す出現は `Foo` になり、`Foo` を実装するトレイトとしてしまう。そこで、`lang/rust.sql` に `rust_impl_parts` を足した。impl の子のうち `for` の直前の子をトレイトの部分、直後の子(`for` が無ければ `impl` の後の、`type_parameters` でない最初の子)を型の部分とし(`rust_units_over` が impl の名前に使う子と同じ)、各部分を最初の `type_arguments` の子孫の手前で切った範囲を「頭」とする。頭の中の最後の参照の出現が、名指しされたトレイトや型である(`From<Foo>` の `From`、`a::b::Baz<T>` の `Baz`、`&'a Baz` の `Baz`、`Box<dyn X>` の `Box`)。その記号を、定義の出現が `type` の unit にあるものに解決する。言語の知識は `lang/` にだけ置くという規約に従い、ノードの型の名前を使う部分は `lang/rust.sql` に置いた。

### モジュールとクレート

ファイルは暗黙のモジュールで、名前はパスである。モジュールの unit は `<path>::<qualified_name>`(名前の間は `::`)とした(`src/shapes.rs::tests`)。依頼文は `module_id` をファイルの `file_id` から導くよう書いていたが、`module_edges(src_module, dst_module, …)` の列名に合わせ、読める名前を鍵にした。`unit_module` は、unit 自身とそれを囲む unit のうち最も近いモジュールの unit、無ければファイルである。依頼文の「最も近い囲むモジュール」に、モジュールの unit 自身を含めた。`mod tests { use super::*; … }` の中で、どの子の unit にも入らない出現は、モジュールの unit 自身に置かれるので、それを `tests` モジュールに属させるためである。

`mod x;` の宣言もモジュールの unit になる(この repo のモジュールの unit 705 個のうち、1 行のものが 481 個)。中身は x のファイルにあるので、これらは空のモジュールとして `modules` に現れる。宣言の `x` は、ファイル全体を範囲とする定義を持つモジュールの記号への参照で、その定義はどの unit にも入らないので、辺は生じない。この repo で、1 行のモジュールの unit を端に持つモジュールの辺は 0 本だった。

`file_crate` は依頼文のパスの規則のとおりである(`crates/<name>/…`、`duckdb-ext/<name>/…` はその名前、`preview-plugin/…` は `preview-plugin`、それ以外は `horizon`)。`module_edges` と `crate_edges` は、自己辺を除いた `unit_edges` を、異なるモジュール(クレート)の間で数える。どちらにも、テストの unit を端に持つ辺、`via_macro` の辺、`ambiguous` の辺の数の列を足した。

循環は、`module_edges` の推移閉包(再帰の共通表式、`UNION` で新しい組だけを足す)から、互いに到達する組をまとめた強連結成分である。名前は最小の要素にした。`module_cycle_edges` が成分の内側の辺、`module_cycles` が成分ごとの行である。クレートにも同じものを `crate_cycles` として置いた。DuckDB 2.0.0 の `USING KEY` は使っていない。この repo で、閉包を含む `module_cycles` はキャッシュの上で 0.22〜0.24 s だった。

### 履歴の辺

`unit_commits(sha, unit_id)` は、`unit_changes` を `(path, kind, qualified_name)` で今の unit に結んだもので、`file_cochange` と同じく、`max_commit_files`(既定 50)より多くのファイルに触れたコミットと、`include_merges` が偽のときのマージを除く。`unit_cochange` は、`min_shared`(既定 3)以上のコミットを共有する組で、`coupling_pct` は共有数を大きいほうの数で割った値(`file_cochange` の `basis = 'max'`)である。依頼文に無い列として、同じファイルかどうか(`same_file`)と、一方が他方を囲むかどうか(`nested`)を足した。impl とそのメソッドは、メソッドの変更が必ず impl の変更でもあるので、組の上位を占める。除かずに印を付けた。

`edge_cochange` は、自己辺でない unit の辺すべてに、両端が共有するコミットの数(無ければ 0)と各端のコミットの数を付けたものである。依頼文は `unit_edges` に `unit_cochange` を左結合する形を書いていたが、`unit_cochange` は `min_shared` で絞られているので、それでは閾値より下の辺の共有数が NULL になる。閾値を通さずに数えることにした。`cochange_without_reference` は、`unit_cochange` の組のうち、別のファイルにあり、2 つの unit の間に向きを問わず辺が無く、`coupling_pct` が `min_coupling_pct`(既定 30)以上のものである。比べるのは 2 つの unit そのものだけで、中の unit の辺は数えない。

### クエリ

- `unit_dependencies`:`path`・`unit_kind`・`qualified_name` で与えた unit から、自己辺を除いた `unit_edges` を `depth`(既定 1)段までたどる。`direction` は `out`(参照する先)、`in`(参照する元)、`both`。隣の unit は各向きで最小の距離に 1 行で、1 段手前の unit からの辺の記号(表示名)、辺の数、出現の数、churn(`unit_churn` と同じ数え方)、fan-in のファイル数を付ける。fan-in は、自分のファイル以外で自分に辺を張る unit のファイルの数で、`unit_fan_in_churn` と違い、unit の外の参照と、中の unit の記号は数えない。unit を与えないと誤りにする。
- `module_graph`:`module_edges` と逆向きの辺の数。`crate` を与えると、両端がそのクレートの辺だけ。
- `module_cycles`:循環の内側の辺。成分の中で辺の少ないものから並べ、記号の表示名を 5 つまで付ける。
- `unit_cochange`、`cochange_without_reference`:上のビューの行に、パス・種類・名前を付けたもの。`unit_cochange` には辺があるかどうか(`referenced`)を足した。

見出しの根拠の行は、Mo et al.(TSE 2021)の Modularity Violation・Package Cycle と、調査の co-change の注意(閾値への強い依存、MDPI Software 2024)を書いた。調査は、Rust でモジュールの循環が有害だとする検証は見つからなかったと書いており、`module_cycles` の見出しにもそう書いた。

## 2. 受け入れた制約

依頼文が挙げた 3 つの制約を、こう扱った。

- **`local` 記号は除く。** 文書の中で閉じる記号(束縛、クロージャの引数、型パラメータ)である。この repo の出現 301,687 個のうち 82,970 個が `local` で、参照の出現は 53,303 個、すべてどこかの unit に入っていた。含めた場合に足される unit の組は 5,973 組で、5,944 組は自己辺、29 組が異なる unit の組だった(そのうち `local` でない辺と重ならないものは自己辺を含めて 5,933 組、異なる unit の組は 29 組すべて)。
- **トレイトのメソッドの呼び出しの解決。** 依頼文は、rust-analyzer がトレイトのメソッドの呼び出しを、impl のメソッドでなくトレイトのメソッドの定義に解決すると書いていた。fixture で確かめると、`fn from(square: Square) -> Self { Side(square.area()) }` の `area` は `impl Shape for Square` のメソッドに解決し、`fn total<S: Shape>(shapes: &[S]) -> u32 { shapes.iter().map(|s| s.area()).sum() }` の `area` はトレイト `Shape` のメソッドに解決した。トレイトに解決するのは、受け手の型がジェネリックかトレイトオブジェクトの場合である。この repo で、メソッドの unit に入る参照の行 7,549 行のうち、親が impl のもの(impl のメソッド)が 7,517 行、親が `type`(トレイト)のものが 32 行だった。ビューの注釈と README は、この観察に合わせて書いた。impl のメソッドからそれが実装するトレイトのメソッドへの辺は、SCIP の relationships が無いので、どちらの場合も無い。
- **マクロの呼び出しの中。** rust-analyzer が展開を通して元の位置に戻した出現だけが入る。fixture の `assert_eq!(Square::new(2).area(), 4)` の 3 つの参照は、すべて `via_macro` の辺になった。この repo で、unit の中にありマクロ呼び出しの中にある参照の出現は 50,754 個で、辺のうち `via_macro` のものは自己辺を除いて 5,570 本だった。`macro_rules!` の本体の中の参照は索引に出ない。

## 3. この repo での数

throwaway の clone(`target/code-db/clone`、0ffe4645、作業ツリーと同じ木)で、作業ツリーで作った索引(`rust-analyzer scip .`、167 s、30,385,713 バイトで SCIP の記録と同じ大きさ)をコピーして測った。作業ツリーと clone の違いは `duckdb-ext/sql/` の中だけで、解析される `.rs` は同じである。

| 項目 | 値 |
| --- | --- |
| 出現 / `local` / 定義 | 301,687 / 82,970 / 42,376 |
| `local` でない参照 / うち unit の外 | 206,008 / 15,622 |
| unit の中の参照で、記号の定義がどの unit にも無いもの(外部のクレートなど) | 117,189 |
| `unit_references` の行 / 出現 | 53,812 / 73,271 |
| `unit_edges` / 自己辺 / 自己辺でない | 34,195 / 58 / 34,137 |
| 自己辺でない辺のうち、テストの unit を端に持つもの / 持たないもの | 20,189 / 13,948 |
| 自己辺でない辺のうち `via_macro` / `ambiguous` | 5,570 / 118 |
| モジュール(ファイル 533 + モジュールの unit 705) | 1,238 |
| `module_edges` / 同じクレートの中 / テストの辺だけ / `ambiguous` の辺だけ | 3,517 / 2,458 / 1,542 / 6 |
| `crate_edges` / その unit の辺 | 35 / 5,889 |
| モジュールの循環 / そのモジュール / 内側の辺 | 31 / 166 / 354 |
| クレートの循環 | 0 |
| impl / 型が解決 / トレイト impl / トレイトが解決 | 585 / 549 / 231 / 45 |
| `unit_commits` の行 / コミット / unit | 13,383 / 552 / 7,597 |
| `unit_cochange`(`min_shared` 3)/ 入れ子 / 同じファイル / 別のファイル | 7,528 / 348 / 2,144 / 5,384 |
| `edge_cochange` / 共有 1 以上 / 共有 3 以上 | 34,137 / 14,375 / 636 |
| `cochange_without_reference`(既定)| 2,917 |

型の部分が解決しなかった 36 個のうち 34 個は頭に出現が無いもの(プリミティブや型パラメータ)、2 個は外部の型(`serde_json`)と、別の unit に定義の無い `horizon-agent` の型だった。トレイトの部分の解決しなかった 186 個は、`core` 104、`gpui-pre` 34、出現なし 20、`serde_core` 7、`gpui-component` 5、`schemars` 4 などだった。

非マージのコミット 1,265 個のうち、50 ファイル以下は 1,249 個、`unit_changes` の行を持つのは 889 個で、そのうち今の unit に結びつくのは 552 個だった。

### モジュールの循環

`module_cycles` の 31 行(循環の名前、クレート、モジュールの数、内側の辺、その unit の辺、出現):

| 循環(最小の要素) | クレート | モジュール | 辺 | unit の辺 | 出現 |
| --- | --- | --- | --- | --- | --- |
| `crates/horizon-agent/src/judge/chat.rs` | horizon-agent | 40 | 106 | 338 | 633 |
| `src/agent/view/pane.rs` | horizon | 13 | 41 | 258 | 568 |
| `crates/horizon-agent/src/providers/mock.rs` | horizon-agent | 12 | 38 | 122 | 246 |
| `crates/horizon-agentd/src/board_flow/dispatch.rs` | horizon-agentd | 11 | 21 | 50 | 114 |
| `crates/horizon-agent/src/providers/rig/clearing.rs` | horizon-agent | 9 | 25 | 104 | 236 |
| `crates/horizon-agent/src/transcript/diff.rs` | horizon-agent | 8 | 17 | 35 | 137 |
| `crates/horizon-workspace/src/layout.rs` | horizon-workspace | 7 | 18 | 190 | 332 |
| `crates/horizon-agent/src/tools/input/bounded.rs` | horizon-agent | 5 | 7 | 34 | 52 |
| `src/terminal/keyboard.rs` | horizon | 5 | 10 | 43 | 78 |
| `crates/horizon-agent/src/persistence/projection/duckdb/append.rs` | horizon-agent | 4 | 7 | 52 | 65 |
| `crates/horizon-board/src/store/logd.rs` | horizon-board | 4 | 7 | 14 | 21 |
| `crates/horizon-logd/src/writer.rs` | horizon-logd | 4 | 7 | 22 | 30 |
| `crates/horizon-agent/src/config.rs` | horizon-agent | 3 | 4 | 11 | 32 |
| `crates/horizon-agent/src/contract.rs` | horizon-agent | 3 | 4 | 20 | 29 |
| `crates/horizon-agentd/src/session/approval.rs` | horizon-agentd | 3 | 4 | 11 | 12 |
| `crates/horizon-agentd/src/session/attachment.rs` | horizon-agentd | 3 | 4 | 24 | 31 |
| `src/board/events.rs` | horizon | 3 | 4 | 8 | 8 |
| `src/theme/palette.rs` | horizon | 3 | 4 | 11 | 22 |
| `crates/horizon-agent/src/contract/provider_event.rs` | horizon-agent | 2 | 2 | 3 | 20 |
| `crates/horizon-agent/src/tools/processing.rs` | horizon-agent | 2 | 2 | 2 | 6 |
| `crates/horizon-agentd/src/hub.rs` | horizon-agentd | 2 | 2 | 15 | 19 |
| `crates/horizon-board/src/event.rs` | horizon-board | 2 | 2 | 4 | 15 |
| `crates/horizon-cli/src/cli.rs` | horizon-cli | 2 | 2 | 11 | 42 |
| `crates/horizon-sandbox-runtime/src/linux/mutation.rs` | horizon-sandbox-runtime | 2 | 2 | 7 | 13 |
| `crates/horizon-sandbox/src/bin/horizon-sandbox-helper.rs` | horizon-sandbox | 2 | 2 | 3 | 6 |
| `crates/horizon-sandbox/src/lib.rs` | horizon-sandbox | 2 | 2 | 6 | 10 |
| `crates/horizon-terminal-core/tests/frame_watch.rs` | horizon-terminal-core | 2 | 2 | 8 | 8 |
| `crates/horizon-terminald/tests/e2e.rs` | horizon-terminald | 2 | 2 | 46 | 53 |
| `crates/horizon-workspace/src/persistence.rs` | horizon-workspace | 2 | 2 | 21 | 69 |
| `src/agent/view/mod.rs` | horizon | 2 | 2 | 23 | 30 |
| `src/runtime/attachment.rs` | horizon | 2 | 2 | 33 | 68 |

このうち `horizon-sandbox-helper.rs`(2 つのバイナリ)、`tests/frame_watch.rs`(`frame_watch.rs` と `limits.rs`)、`tests/e2e.rs`(`e2e.rs` と `latency_probe.rs`)の 3 つは、内側の辺がすべて `ambiguous` だった。別のクレートの根どうしは互いを参照できないので、実際の依存ではない。`ambiguous` の辺を除くと 28 個になる。`module_cycles.sql` の行の `ambiguous_unit_edges` で見分けられる。

### クレートの辺と循環

`crate_edges` は 35 本で、`crate_cycles` は 0 行だった。依頼文は、クレートの循環を空と見なし、空でなければ集計の誤りとするよう求めていた。Cargo は dev-dependency の循環(テストのコードが、自分に依存するクレートを使う)を許すので、`crate_cycles` の注釈にはその場合も書いた。この repo では生じていない。

### `SessionId` の照合

`crates/horizon-wire/src/session.rs` の型 unit `SessionId` に入る辺の、自分のファイル以外の出どころのファイルは 105 個だった。`unit_fan_in_churn` は 107 である。差の 2 ファイルは `crates/horizon-acp/src/lib.rs` と `crates/horizon-wire/src/lib.rs` で、どちらもその記号への参照が 1 つずつ、ファイルの直下の `pub use` にあるだけだった。この層の辺は unit の中の参照からだけ張るので、この 2 つは辺を持たない。出現の単位で、unit の中の参照に限って fan-in の定義で数え直すと 105、限らないと 107 になり、差はこの 2 つで説明がつく。依頼文は 107 に等しいことを求めていたが、定義を変えずに報告する。

### `TerminalSession.spawn` の参照先

`unit_dependencies`(`src/terminal/session.rs`、`method`、`TerminalSession.spawn`、`direction := 'out'`)は 21 行だった(既定の `both` では、参照元 1 行を足した 22 行)。出現の多い順に、`TerminalSession`(同じファイルの型、19 出現、記号はフィールド 15 個)、`TerminalCommand`(`horizon-terminal-core`、7)、`SessionId`(`horizon-workspace` の `session_id.rs` の型、5)、`Incoming`(4)、`KeyEventKind`(2)、`TerminalSessionHandle.sender`(2)、`Scrollback`(2)、`TerminalSession.apply_incoming`(2)、以下 1 出現ずつ `TerminalNotification`、`event_stream`、`RuntimeLink`、`RuntimeLink<C>.new`、`RuntimeLink<C>.mark_unreachable`、`TerminalSessionHandle`、`TerminalSessionHandle.frames`、`TerminalSessionHandle.events`、`Scrollback.abandon`、`dump_path_for_session`、`RowGenerations`、`TrafficTraceStats`、`TrafficTraceStats.new` である。標準ライブラリや外部クレートの呼び出しは、定義の unit が無いので現れない。

### co-change

`unit_cochange` の上位は、同じファイルの入れ子の組(`TerminalSession` の impl と `TerminalSession.spawn`、30 コミット)だった。`cochange_without_reference` の上位は、別のファイルに分かれた `WorkspaceShell` の impl どうし(`src/workspace/commands.rs` と `session_lifecycle.rs`、17 コミット、41.5%)や、`crates/horizon-workspace/src/commands.rs` の `command_enabled` と `src/workspace/commands.rs` の `WorkspaceShell` の impl(15、41.7%)だった。2,917 組のうち 150 組は、2 つの unit の中の unit(自分を含む)どうしの間には辺があった。閾値による変化は、`min_shared` 3 で `min_coupling_pct` 30/50/80 が 2,917/1,622/470 組、`min_shared` 5 で 30/50 が 358/181 組だった。辺の上の co-change(`edge_cochange`)の、別のファイルの間の上位は、`WorkspaceShell.execute` → `CommandId`(14 コミット)、`WorkspaceShell` の impl どうし(12)、`Store.project_event` → `Event`(12)などだった。

## 4. キャッシュ

どのビューも、`cache.sql` と `refresh.sql` に足さず、ビューのままにした。下の時間(`check.py measure`、各回は新しいプロセス、3 回の最小〜最大)で、キャッシュの上ではほとんどが 0.1〜0.4 s だった。表にすると問い合わせの時間はさらに縮むが、`refresh.sql` に入れると、ファイル・索引・履歴のどれが変わっても作り直しが要り、作り直しの時間がそのまま更新の時間に足される。依頼文が測るよう求めた `unit_references`(出現と unit の結合)は、キャッシュの上で 0.14〜0.17 s だった。出現の位置づけは `unit_occurrences` で 1 回だけ行い、定義と参照の 2 つの結合はその結果の上でしているので、30 万行と 9 千行の結合は 1 回である。スキーマの版は 3 のままである。

| 文 | ライブ | ピーク RSS | `cache.sql` の上 |
| --- | --- | --- | --- |
| `count(*) FROM unit_occurrences` | 0.70〜0.72 s | 1,704〜1,784 MiB | 0.09〜0.11 s |
| `unit_references` | 1.12〜1.15 s | 2,792〜2,818 MiB | 0.14〜0.17 s |
| `unit_edges` | 1.13〜1.14 s | 2,674〜2,911 MiB | 0.13〜0.15 s |
| `impl_targets` | 1.00〜1.03 s | 2,952〜3,019 MiB | 0.15〜0.18 s |
| `unit_module` | 0.89〜0.90 s | 2,747〜2,900 MiB | 0.01〜0.02 s |
| `module_edges` | 1.41〜1.48 s | 4,508〜4,581 MiB | 0.15〜0.21 s |
| `crate_edges` | 1.18〜1.21 s | 3,195〜3,297 MiB | 0.16 s |
| `module_cycle_edges` | 1.43〜1.49 s | 4,483〜4,639 MiB | 0.18〜0.22 s |
| `module_cycles` | 1.53〜1.58 s | 4,920〜5,108 MiB | 0.22〜0.24 s |
| `crate_cycles` | 1.16〜1.21 s | 3,220〜3,301 MiB | 0.15〜0.17 s |
| `unit_cochange` | 12.57〜12.91 s | 9,582〜9,615 MiB | 0.22〜0.24 s |
| `edge_cochange` | 14.05〜14.10 s | 10,339〜10,437 MiB | 0.19〜0.21 s |
| `cochange_without_reference` | 13.89〜14.49 s | 11,154〜11,270 MiB | 1.18〜1.74 s |
| クエリ `unit_dependencies` | 14.76〜14.95 s | 12,265〜12,508 MiB | 0.24〜0.26 s |
| クエリ `module_graph` | 1.42〜1.47 s | 4,488〜4,583 MiB | 0.17〜0.18 s |
| クエリ `module_cycles` | 3.78〜3.87 s | 10,578〜10,611 MiB | 0.25〜0.37 s |
| クエリ `unit_cochange` | 12.79〜20.11 s | 11,542〜11,787 MiB | 0.23〜0.29 s |
| クエリ `cochange_without_reference` | 16.98〜17.25 s | 11,452〜11,576 MiB | 1.27〜1.44 s |
| ビューのファイルを読んだだけ | 0.00 s | 1,174〜1,259 MiB | |

`cache.sql` の上のピーク RSS は作成を含むので 8.7〜9.1 GB で、表から省いた。ライブの `unit_dependencies` が履歴のビューと同じくらいかかるのは、churn の列のために `unit_changes` を読むためである。ライブの co-change の時間のほとんども `unit_changes`(blob の解析)である。`check.py explain` では、ライブのグラフのクエリは `READ_AST` が 1〜2、`READ_SCIP` が 1〜2 だった。`units`・`tags`・`impl_parts` はそれぞれ別のビューで、ビューの境界を越えて 1 回の走査にまとめることはしていない。

`cochange_without_reference` は、永続キャッシュの上では 0.24〜0.25 s だった(下表)。セッションキャッシュの上で遅い理由は調べていない。

永続キャッシュ(`check.py measure-refresh target/code-db/clone --runs 3`、更新を含む時間):

| 文 | 永続キャッシュの上 | ピーク RSS |
| --- | --- | --- |
| `unit_occurrences` / `unit_references` / `unit_edges` | 0.05 / 0.08〜0.09 / 0.08 s | 511〜605 MiB |
| `impl_targets` / `unit_module` | 0.07〜0.08 / 0.01〜0.02 s | 388〜692 MiB |
| `module_edges` / `crate_edges` / `module_cycle_edges` | 0.11 / 0.09〜0.10 / 0.14〜0.15 s | 544〜643 MiB |
| `module_cycles` / `crate_cycles` | 0.13〜0.15 / 0.09〜0.10 s | 567〜638 MiB |
| `unit_cochange` / `edge_cochange` / `cochange_without_reference` | 0.21〜0.26 / 0.19 / 0.24〜0.25 s | 762〜970 MiB |
| クエリ `unit_dependencies` / `module_graph` / `module_cycles` | 0.22〜0.23 / 0.11〜0.12 / 0.24〜0.29 s | 595〜1,094 MiB |
| クエリ `unit_cochange` / `cochange_without_reference` | 0.25〜0.28 / 0.25〜0.27 s | 933〜968 MiB |

`refresh.sql` 自体は変えていないので、更新の時間はグラフのビューを読むかどうかに関わらない(`views/graph.sql` を読むのは、ビューを作るだけで 0.05〜0.11 s)。SCIP の記録との比較:

| 場合 | この記録 | SCIP の記録 |
| --- | --- | --- |
| 空のキャッシュ | 20.32〜20.43 s(ピーク RSS 6,621〜6,951 MiB) | 22.01〜22.38 s |
| 何も変わっていない | 0.23〜0.27 s | 0.22〜0.26 s |
| 1 ファイルの編集 | 0.31〜0.34 s | 0.32〜0.39 s |
| 10 ファイルの編集 | 0.37 s | 0.41 s |
| 3 ファイルを変えた 1 コミット | 0.34〜0.46 s | 0.39〜0.42 s |
| amend(履歴の作り直し) | 2.79〜3.54 s | 3.06〜4.45 s |
| 索引の変更(SCIP の作り直し) | 0.62〜0.68 s | 0.62〜0.84 s |

キャッシュファイルは 629.8 MiB(SCIP の記録 628.3 MiB)だった。clone の木は SCIP の記録の木より新しく(ファイル 533 個、blob 6,594 個)、差はその分である。

## 5. 検査

### fixture

`check.py fixture` にグラフの fixture を足した(`check.py graph-fixture` で単独でも動く)。履歴の fixture の木に 1 コミットを足し、互いの関数を呼ぶ 2 つのモジュール `src/cycle_a.rs`・`src/cycle_b.rs`(`pong` は `crate::area` も呼ぶ)、`impl From<Square> for Side` とそのメソッドで具体的な `Square` の `area` を呼ぶもの、ジェネリックの境界を通して `area` を呼ぶ `total` を `src/convert.rs` に置いた。その木を SCIP の fixture と同じく rust-analyzer で索引にする。確かめていること:

- `unit_references` の全 22 行(具体的な呼び出しが impl のメソッドに、ジェネリックの呼び出しがトレイトのメソッドに届くこと、`assert_eq!` の中の 3 つの参照と結合テストの 1 つが `via_macro`、`use` の行と `mod` の宣言が辺を生まないこと)、自己辺が無いこと、`local` 記号が無いこと。
- `impl_targets` の 7 行(`From<Square> for Side` のトレイトは `From` で未解決、`Square` ではない)、`impl_for` 4 行、`implements` 2 行。
- `unit_module`(テストモジュールの関数とモジュールの unit 自身が `src/shapes.rs::tests`、`mod rect;` の宣言が `src/lib.rs::rect`、行数が `units` と同じ)。
- `module_edges` の全 7 行、2 つのモジュールの循環 1 個、クレートの辺と循環が無いこと(fixture のパスはすべて `horizon`)。
- `min_shared := 2` での `unit_cochange` 9 行(clone の対の 2 つの `area` メソッドとそれらの impl が 2 回一緒に変わったこと、入れ子の印)、`edge_cochange` の共有 1 以上の 15 行、`cochange_without_reference` の 6 行、`min_coupling_pct := 70` で 0 行、`min_shared := 3` で入れ子の 2 組だけ。
- 5 つのクエリの行(`unit_dependencies` の `depth := 2`・`direction := 'out'` で根を除くこと、unit を与えないときの誤り、`crate` での絞り込み)。
- `cache.sql` の上で同じ行になり、5 つのクエリの計画に `READ_AST`・`READ_SCIP`・`READ_TEXT` が無いこと。

更新の fixture は、SCIP の段階で `views/graph.sql` も読むようにし、各段階の後にグラフのビューとクエリがライブと一致することを確かめる。結果は fixture、history fixture、scip fixture、graph fixture、refresh fixture のすべて ok で、何も変わっていないときの更新は 138〜166 ms だった。

### この repo

`check.py repo`、`refresh`、`cli`、`explain` を clone で動かした。`refresh` は 1 回目で落ちた。既存の 13 ビューとグラフの 11 ビューはライブと一致したが、`unit_cochange` と `cochange_without_reference` のクエリの行の並びが違った。行の集合は同じで、並びの鍵にパスと名前しかなく、同じパス・同じ名前で種類の違う unit(`WorkspaceShell` の `type` と `impl`)の順が決まっていなかった。種類と unit の id を並びの鍵に足すと、2 回の更新の両方で、すべてのビューとクエリがライブと一致した(1 回目 0.31 s、2 回目 0.37 s)。`cli` では、CLI の `.read` と Python で、件数(533・9,264・14,805・34,822・1,502・301,687・42,839・34,195・3,517)と 13 のクエリの行数が、ライブと更新後の両方で一致した。

## 6. うまくいかなかったこと・残っていること

- **impl の見出しの出現を順に取る案**は、トレイトの側で合わなかった。impl の unit の中で子の unit より前の行にある、型の unit を指す参照を順に取り、トレイト impl では 1 番目をトレイト、2 番目を型とした場合、トレイトの 197 行のうち今の規則と同じ先になったのは 45 行で、残り 152 行は、外部のトレイト(`Default`、`Drop`、`From<…>` など)の impl で、型そのものや型の引数をトレイトとしていた。型の側は 401 行のうち 400 行が同じだった。
- **`cochange_without_reference` の向きを問わない辺の判定**は、最初 2 つの向きの `OR` で書き、セッションキャッシュの上で 1.1〜1.2 s だった。向きを持たない組(`least`・`greatest`)にして等号の反結合にしたが、セッションキャッシュの上の時間は 1.18〜1.74 s で変わらなかった。この書き方は `unit_cochange.sql` の `referenced` にも使った。
- **クエリの並びの決まらなさ**(5 節)で、更新の検査が一度落ちた。
- **ファイルの直下の参照**(`use`、`pub use`、ファイルの直下の定数)は辺を持たない。`SessionId` の照合の 2 ファイルの差はこれである。
- **`mod x;` の宣言**は空のモジュールとして `modules` に残る。
- **1 行を共有する入れ子でない unit**は、後ろの unit に置かれる(この repo では 0 組)。
- **別のターゲットの同名の記号**は、どちらの定義にも辺を張り、見かけの循環を 3 つ作った。`ambiguous` で見分けるだけで、ターゲットを解決してはいない。
- **ライブの時間。** グラフのビューは `units`・`tags`・`impl_parts` をそれぞれ読み、co-change は `unit_changes` を読むので、ライブでは 1〜20 s かかる。キャッシュの上で使う前提である。
- 索引は今の木だけを表し、グラフに履歴は無い(co-change だけが履歴を持つ)。

## コマンド

```sh
duckdb-ext/build.sh
rust-analyzer scip . --output target/code-db/index.scip
git clone -q . target/code-db/clone
cp target/code-db/index.scip target/code-db/clone/target/code-db/
uv run --locked --script scripts/refactor-audit/run.py setup
duckdb-ext/sql/jscpd.sh target/code-db/clone target/code-db/clone/target/code-db/jscpd src crates preview-plugin/src
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py graph-fixture
uv run --locked --script duckdb-ext/sql/check.py repo target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py cli target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only "count unit_references"   # 各ビューとクエリについて
uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 3
```

本文の数(循環、`SessionId` の照合、`local` の組、impl の解決、見出しの案との比較、co-change の閾値)は、`refresh.sql` の後の接続で `views/graph.sql` を読み、SQL を直接実行して取った(`target/code-db/scratch/`、コミットしていない)。
