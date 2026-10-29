# 2 つのスナップショットの障害の一覧の差としての前後の比較(2026-10-29)

この記録は、コードデータベース(`docs/code-database-design.md`)の前後の比較を、2 つのスナップショットそれぞれの SCIP 索引とキャッシュから作る今の木の軸の差として作り直した増分の記録である。依頼はブランチ `code-quality-survey` の 514a1332 からで、作業はワーカーのブランチで行った。設計文書の更新は依頼側に残している。

前提として決まっていたこと(2026-10-29 のオーナーの決定。依頼文による):

- 履歴そのものについて「はい。それでよいです。」(`docs/research/code-db-scip-rebuild-2026-10-29.md`)。履歴は仕組みから外れ、前後の比較は「2 つの SCIP 索引の障害の一覧の差として作り直す」ことになっていた。
- 比較の目的は旧来の監査の `compare` と同じで、リファクタリングのあとに狙った障害が減ったか、新しい障害が現れたかを見ることである。要るのは障害の一覧の差で、同一性は記述子による。名前の変更は消えたものと現れたものとして出てよい。unit ごとの対応を作る仕組みと系譜は作らない。
- そのうえで「その3つに進めてもらってよいです」。
- 同時に別のワーカーが複雑さの指標を `unit_quality`(`views/quality.sql`)に足している。このワーカーは `views/quality.sql` を編集せず、`unit_quality` の列は読み込み時にビューから読む。

## 方法と環境

- 検証の clone は作業ツリーの `target/code-db/snapshots/` に作った(`git clone --no-checkout` のあと各コミットを取り出した)。e223bc20(2026-09-22)と 46c37290(2026-09-27、その間 104 コミット、412 ファイル、+51,712/−25,499 行)が 9 月のリファクタリングの一掃、2e6a5f32 と 2a49dc89(13 コミット、53 ファイル、+3,701/−599 行)が判断モデルの範囲である。
- 4 つの clone それぞれに `code-db index` を走らせた(ルート 153.6〜166.3 s、`preview-plugin` 57.4〜65.3 s、jscpd 1.8〜2.4 s、1 つあたり 217〜227 s)。古いチェックアウトの Cargo のワークスペースはどれも問題なく読めた。索引を作ったあとは各ワークスペースの `target/debug` を消した。キャッシュはそれぞれの最初の `code-db compare` で作られた。
- rust-analyzer 1.99.1(58cf8f9 2026-10-13)、DuckDB 2.0.0(`duckdb` クレート 1.20000.0 と Python の `duckdb` 2.0.0)、jscpd 5.3.1。機械はオーナーのデスクトップで、他のワーカーと共有している。時間は他の負荷の下での値で、すべて `nice -n 19` で測った。
- 調べるための SQL とシェルは作業ツリーの `target/code-db/` に置き、コミットしていない。行の確かめには clone の元のリポジトリで `git diff`・`git show` を使った。

## 要約

- **`code-db compare AFTER BEFORE [QUERY] [NAME=VALUE ...]` を足した。** BEFORE を `query` と同じく読み(ビューのファイル、`refresh.sql`、BEFORE のキャッシュ)、片側の行を `compare/sides.sql` の `side_*` ビューから一時ファイルに写し、AFTER を同じく読んでそのファイルを読み取り専用で `before` として付け、`compare/views.sql` のビューとクエリを走らせる。
- **ビュー** は `compare_units`、`compare_unit_axes`、`compare_column_rules`、`compare_clones`、`compare_edges`、`compare_cycles`、`compare_layer_violations`。**クエリ**(`compare/queries/`)は `summary`、`units`、`unit_axes`、`clones`、`edges`、`cycles`、`layer_violations`。しきい値は無く、どれも両側の値、差、状態(kept、appeared、disappeared)を出し、変化の大きい順に並べる。テストは全体を通して別に数える。
- **同一性は `(crate, kind, 記述子)`** である。記述子は SCIP のシンボルからスキーム・マネージャ・パッケージ・バージョンを除いたもので、crate の中のモジュールの木での位置を表す。`qualified_name` はモジュールを含まないので、46c37290 では 163 組 552 個の unit が同じ値を共有した(記述子では 22 組 55 個)。パスは同一性に入れない:モジュールを変えずにファイルが移った unit が一掃で 17 個あり、どれも同じものだった。ただし 1 つの側で記述子を共有する unit(パッケージの複数のターゲットの根がすべて `crate/` になる、関数本体の中の項目)は、パスで先に組にする。
- **一掃の要約**(e223bc20 → 46c37290、本番のコード):unit は 3,980 → 4,503、残った 3,538 個のうち本文が変わったものが 718 個、現れた 965、消えた 442。`loc` は残った unit のうち 344 個が減り 205 個が増えた。クローンの組は本番で `clone_pairs` の合計 489 → 376(残った unit の 63 個で減り 15 個で増えた)、exact の報告の組は 55 → 29(消えた 26、現れた 5)。モジュールの辺は 1,558 → 2,152(現れた 781、消えた 187、残ったもののうち 189 本が軽く 89 本が重くなった)。モジュールの循環は 17 → 21 で、18 個のモジュールの循環が 2 個の循環と合わさって 40 個になった。
- **判断モデルの範囲の要約**(2e6a5f32 → 2a49dc89、本番):unit は 5,106 → 5,184、本文の変わった残りが 79、現れた 80、消えた 2。`const`/`static` の行は現れた 26、消えた 2、本文の変わった残り 3 で、判定のための指示文(`BASE_INSTRUCTIONS`、`GIT_INSTRUCTIONS`、`HOST_EXECUTION_INSTRUCTIONS`)としきい値(`DEFAULT_ASK_THRESHOLD`、`DEFAULT_CONTINUATION_THRESHOLD`)が行として出る。残った本番の unit 5,104 個は、本文が変わらず軸も変わらない 4,914、本文は変わらず軸だけ変わった 111、本文が変わって軸も変わった 73、本文だけ変わった 6 に分かれる。重みの変わった辺は本番のモジュールの辺で 13 本(3 本が軽く、10 本が重く)、crate の辺で 3 本。
- **時間。** 両側のキャッシュがあるとき、一掃の `code-db compare` は 1 回 3.4〜5.7 s で、そのうち BEFORE の写しが 1.7〜2.8 s、クエリだけなら 0.01〜1.3 s(`summary` 1.2〜1.3 s、`units` 0.8〜0.9 s、`unit_axes` 0.6 s、`clones` 0.5 s、`edges`・`cycles`・`layer_violations` 0.01〜0.06 s)。両側のキャッシュを消してからの 1 回は 7.0 s だった。
- **検証。** `check.py fixture` は 2 つのスナップショットの fixture を足して通り、`duckdb-ext/check.sh` も通った。一掃の行のうち 6 行を `git diff`・`git show` で確かめ、すべて合っていた。

## 形

### サブコマンド

```sh
code-db compare AFTER BEFORE                      # compare/queries/summary.sql
code-db compare AFTER BEFORE units kind=function  # compare/queries/units.sql
code-db compare AFTER BEFORE ./my-question.sql    # / を含む QUERY は SQL ファイル
```

1. AFTER の `.code-db.toml` を読み、BEFORE の `Session` にも同じ設定を渡す。BEFORE の設定ファイルが違うときは標準エラーに 1 行書く。
2. 両側に SCIP 索引と jscpd の 2 つの報告があることを確かめる。無ければ `compare (BEFORE) needs the SCIP indexes in …: run 'code-db index …'` のように止まる。
3. BEFORE を `query` と同じく読む(リミット、ビューのファイル、`refresh.sql`、BEFORE のキャッシュ。無ければ作る)。`compare/sides.sql` を読み、`AFTER/target/code-db/tmp/compare-before-<PID>.duckdb` を `snapshot` として付け、`compare/export.sql` で `side_*` ビューをすべて表に写して外す。DuckDB のデータベースはここで閉じる。
4. AFTER を同じく読み、`compare/sides.sql` を読み、写しを `ATTACH … AS before (READ_ONLY)` で付け、`compare/views.sql` を読む。そのあとクエリを走らせ(既定の markdown は同じプロセスで、ほかの `FORMAT` は DuckDB の CLI で)、写しを消す。

`Session` に「読み込みのあとに読む SQL」(`post_load`)を足し、`run` を終了状態を返す `run_status` に分け、クエリのファイルを探す部分(`query_file`)と `LIMIT` で包む部分(`query_text`)を `query` から切り出した。`query`、`sql`、`shell` の振る舞いは変わらない。

### ファイル

| ファイル | 中身 |
| --- | --- |
| `duckdb-ext/sql/compare/sides.sql` | 片側の `side_units`(`unit_quality` の全列、`descriptor`、`identity_count`、`text_hash`)、`side_modules`、`side_clones`、`side_module_edges`、`side_crate_edges`、`side_cycles`(`test_members` を足す)、`side_layer_violations` |
| `duckdb-ext/sql/compare/export.sql` | `side_*` を `snapshot.*` に写す。`side_meta` にリポジトリと時刻 |
| `duckdb-ext/sql/compare/views.sql` | `compare_column_rules`、マクロ `compare_obstacle`、`compare_units`、`compare_unit_axes`、`compare_clones`、`compare_edges`、`compare_cycles`、`compare_layer_violations` |
| `duckdb-ext/sql/compare/queries/*.sql` | `summary`、`units`、`unit_axes`、`clones`、`edges`、`cycles`、`layer_violations` |
| `duckdb-ext/sql/check_compare.py` | 比較の fixture(`check.py fixture` が続けて走らせる) |
| `duckdb-ext/code-db/src/main.rs`、`tests/end_to_end.rs` | `compare` サブコマンドとその通しの試験 |
| `duckdb-ext/sql/README.md` | 節「Comparison」 |

`compare/` の SQL は `VIEW_FILES` にも `refresh.sql` にも入れていない。キャッシュの鍵はビューの本文を含むので、`compare` のときだけ読むファイルがビューを足すと `query` と `compare` で鍵が変わり、キャッシュを交互に作り直すことになる。`sides.sql` は `refresh.sql` のあとに読み、キャッシュの写しの上のビューを読む。

### ビュー

- **`compare_units`**:両側の unit を 1 行ずつ。`unit_key`(`crate kind 記述子`、記述子を共有する unit には ` #n`)、`status`、`is_test`(AFTER の値、消えた unit は BEFORE の値)、`text_changed`(残った unit だけ)、`ambiguous`、`paired_by`、両側の `unit_id`・パス・モジュール・開始行・`loc`、そして `before`・`after` に `side_units` の行全体を構造体で持つ。
- **`compare_unit_axes`**:unit と `unit_quality` の列ごとに 1 行。列は読み込みのときに `duckdb_columns()` から読む(同一性の列 `unit_id, path, crate, module, kind, qualified_name, start_line, end_line, is_test` を除く全列)。値は文字列で、数の列は `delta` を持ち、`change`(unchanged、increased、decreased、changed)、`effect`(improved、regressed)、`obstacle_before`・`obstacle_after` を持つ。
- **`compare_column_rules`**:列の読み方。`lower`(loc、3 つのクローンの数、4 つの fan-in・fan-out、parameters、debt_lines)、`null_is_better`(cycle)、`true_is_better`(has_doc)、`unreferenced_is_worse`(reference_status)、`none`(layer、visibility、references)。載っていない列は数なら `lower`、それ以外は `none` で、軸の名前は列の名前になる。
- **`compare_clones`**:報告と 2 つの unit の `unit_key` ごとの組の数と行数。
- **`compare_edges`**:モジュールの辺(`modules.module` の組)と crate の辺。両側の unit 辺(合計、テストの端を持たない本番、テストの端を持つもの)と出現。
- **`compare_cycles`**:モジュールを 1 つでも共有する BEFORE と AFTER の循環の組を kept、共有しないものを appeared・disappeared とし、大きさ、加わった・抜けたモジュール、内側の unit 辺、`split`、`merged` を持つ。
- **`compare_layer_violations`**:層の違反の辺を両側で。

### クエリ

`summary` は障害の一覧と範囲(production、test)ごとに、両側の大きさ(`before_total`、`after_total`)、`kept`、`improved`、`regressed`、`changed`、`appeared`、`disappeared` を 1 行に出す。ほかのクエリは対応するビューの行を、既定では変化のあるものだけ(`include_unchanged=true` ですべて)、本番を先に、変化の大きい順に出す。各クエリの見出しに行と引数を書いた。

## 依頼文が決めていなかった判断

1. **BEFORE のキャッシュはそのまま付けない。** 依頼文は「BEFORE のキャッシュを読み取り専用で `before` として付ける」形を挙げていた。キャッシュには `unit_quality`、クローンのビュー、循環、層が入っていない(jscpd の報告、変数、設定を読むので生のまま残る)。ビューはリポジトリを `repo`・`source_files`・`scip_indexes` などのセッションの変数で読むので、2 つのリポジトリを 1 つのセッションで生のまま並べることもできない。そこで BEFORE を同じ SQL で読み、比較に要る行(`side_*`)だけを写したファイルを読み取り専用で付けた。BEFORE のキャッシュは BEFORE を読む速さのために使われる。
2. **BEFORE のキャッシュが無いとき、または別の SQL で作られていたときは作る。** 拒否はしない。比較の両側は `code-db` が建てられた Horizon のチェックアウトの SQL で 1 回の実行の中で導かれるので、別の SQL で作ったキャッシュは鍵が違って読まれず、両側の導き方が食い違う状態は形の上で起きない。`query` もキャッシュを作るので、振る舞いをそろえた。拒否するのは両側の SCIP 索引と jscpd の報告が無いときと、AFTER と BEFORE が同じディレクトリのときである。片側の入力が欠けると、その側の一覧が丸ごと消えた・現れたに見えるからである。
3. **設定は AFTER の `.code-db.toml` を両側で読む。** 差がコードの差であって判断の差でないようにするためである。古いスナップショットには設定ファイルが無いことが多い(今回の 4 つの clone にはどれも無かった)。BEFORE の設定が違うときは標準エラーに 1 行書く。`NAME=VALUE` も両側に効く。
4. **同一性は `(crate, kind, 記述子)` で、`qualified_name` ではない。** 依頼文は `(crate, kind, qualified_name)` と書いていたが、このリポジトリの `qualified_name` はファイルのモジュールより下の記述子だけで、モジュールを含まない(`tests.x` や `Type.new` が多くのファイルで同じになる)。46c37290 で数えると、`(crate, kind, qualified_name)` では 7,875 個のうち 552 個が 163 組で値を共有し、`(crate, kind, 記述子)` では 55 個が 22 組だった。記述子はオーナーの言う「記述子による同一性」にも合う。
5. **パスは同一性に入れない。ただし記述子を共有する unit はパスで先に組にする。** 一掃で、残った unit のうちパスの変わったものは 17 個で、`transcript.rs` → `transcript/mod.rs` の 3 個と、インラインの `mod tests` → `completion/tests.rs` の 14 個だった。どれもモジュールが同じで、同じ unit である。パスを入れるとこれらが消えた・現れたになる。一方、46c37290 で記述子を共有する 22 組 55 個のうち 11 組 33 個は crate の根(`crate/`:ライブラリ、バイナリ、統合テスト、example の各ターゲット)で、ほかは統合テストやバイナリの根の直下の項目(`main().`、`connect_hub().`、`Conn#`、`tests/` など)(11 組 22 個)で、そのうち 1 組だけが同じファイルの関数本体の中の `CACHE` だった。最初の形(記述子の中で path と行の順に番号を振って番号で組む)では、`crates/horizon-agent/src/lib.rs` の根が `examples/validate-history.rs` の根と組になるなど、ターゲットが 1 つ増えると組がずれた。今の形は、同じ記述子の中でまず同じパスの unit を行の順に組み、残りをパスと行の順に組む(`paired_by` が `path` か `order`)。`(crate, kind, 記述子, パス)` で値を共有するのは `CACHE` の 1 組 2 個だけである。
6. **モジュールの同一性は `modules.module`(パス)のままにした。** 辺、循環、層の違反はモジュールをパスで名指す今のビューの行で、そのまま比べる。ファイルの移動はモジュールの辺では消えた・現れたになる。
7. **本文のハッシュ。** unit の開始行から終了行までの行全体を、それぞれ末尾の空白を除いて改行でつなぎ、md5 を取る。列で切らず行全体を使うので、同じ行に 2 つの unit があればどちらも同じ行を含む。インデントは残すので、インラインのモジュールへの出し入れのような字下げの変化は本文の変化になる。
8. **列の読み方はビューに書き、`.code-db.toml` には置かない。** 「良くなった・悪くなった」は列の向きで決まる。軸の記録はそれぞれの列を障害の大きさとして定義しており、向きはプロジェクトの判断ではなく軸の定義の一部と読んだ。`references` は障害の大きさではない(未参照かどうかは `reference_status` が持つ)ので `none`、`visibility` と `layer` も向きを持たない。`cycle` は所属する循環の名前(最小のメンバー)なので、両側で値があって名前が違うときは `changed` で、良し悪しは付けない。後から `unit_quality` に足される数の列は `lower` として読まれる。
9. **クローンの組の同一性は、報告と 2 つの unit の `unit_key`。** 同じ 2 つの unit の間の組は数えて 1 行にし、断片の位置は比べない。unit に入らない断片(索引に無いファイル)は `(no unit) <パス>` で名指す。本番とテストは、片側でもテストの unit なら test(`duplication` クエリの既定と同じ)。
10. **辺の本番とテストは unit 辺で分ける。** モジュールの辺の `test_unit_edges` は、どちらかの端がテストの unit 辺の数である。`summary` は、本番の unit 辺(`unit_edges - test_unit_edges`)が 1 本以上ある側を「本番の辺がある」とし、テストも同じく数える。そのため 1 本の辺が本番とテストの両方に数えられることがある。行のビューの `is_test` は、どちらの側にも本番の unit 辺が無い辺である。
11. **循環は共有するメンバーで組にする。** 循環は強連結成分なので 1 つの側では互いに素だが、両側の間では 1 対多になりうる。組ごとに 1 行を出し、`split`・`merged` で印を付けた。`summary` の `kept`・`improved`・`regressed` はこの組を数える。
12. **変化の無い kept の行はクエリで既定で隠す。** 一掃のモジュールの辺は 2,000 本以上が kept で、変化の無いものまで出すと読めない。`include_unchanged=true` ですべて出る。ビューはすべての行を持つ。
13. **`/` を含む QUERY は SQL ファイルとして読む。** 比較のビューに自分の問いを投げるためである。
14. **テストの fixture は `check.py` から別ファイル `check_compare.py` に置いた。** `check.py` の `fixture` の最後に呼ぶ 3 行と見出しの 1 行だけを足した。同時に作業しているワーカーとの衝突を減らすためである。

## 検証

### fixture

`check_compare.py` は 1 つのワークスペース(crate `shapes` と、それを使うバイナリと統合テストを持つ `app`)の 2 つのスナップショットを別々の git リポジトリに作り、それぞれを rust-analyzer で索引し、jscpd の形の報告を書き、層を `core`・`ui`(`ui` は `core` に依存してよい)として、`refresh.sql` を通して読む。仕込んだ変化と期待した行:

| 変化 | 期待した行(すべて得られた) |
| --- | --- |
| `big` を 3 つに分ける | `big` は kept、`loc` 9 → 3(improved)、本文が変わった。`part_a`・`part_b` が appeared |
| `old_name` → `new_name` | `old_name` が disappeared、`new_name` が appeared |
| 重複を消す(`dup_b` が `dup_a` を呼ぶ) | exact の組 `dup_a`/`dup_b` が disappeared。両方の `clone_pairs` 1 → 0(improved)。`dup_a` は未参照から参照されるへ(improved) |
| 循環を断つ(`b` が `a` を呼ばない) | 循環 `a`/`b` が disappeared、辺 `b.rs → a.rs` が disappeared、`a`・`b` の unit の `cycle` が improved |
| 層の違反を入れる(`core` が `ui` を呼ぶ) | 違反 `core → ui` が appeared。同時に `core`/`ui` の循環が appeared |
| 定数の値を変える | `LIMIT` は kept、本文が変わり、軸の列は変わらない |
| `main` が `stable` も呼ぶ | crate の辺 `app → shapes` の unit 辺が 2 → 3(regressed)、モジュールの辺 `main.rs → lib.rs` が 1 → 2 |
| 統合テストを両方に置く | `app` の 2 つの根(`main.rs` と `tests/it.rs`)が `ambiguous`、`paired_by` = `path` で正しく組になる |

ビュー 6 つとクエリ 7 つのすべての行を期待値として書き、さらに `before` が読み取り専用であることを確かめた。`code-db` の通しの試験には、関数の名前を変えた 2 つ目のスナップショットでの `compare units`(現れた・消えた)、`compare clones`、`compare layer_violations`(AFTER の設定で両側に同じ違反が kept)、索引の無い BEFORE の拒否を足した。

`duckdb-ext/check.sh` は通った(最後の `code-db` の試験は 24 s)。`uv run --locked --script duckdb-ext/sql/check.py fixture` は「fixture: ok」「compare fixture: ok」で通った。

### 9 月の一掃(e223bc20 → 46c37290)

`code-db compare 46c37290 e223bc20`(`summary`):

| 一覧 | 範囲 | 前 | 後 | kept | improved | regressed | changed | appeared | disappeared |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| units | production | 3,980 | 4,503 | 3,538 | | | 718 | 965 | 442 |
| units | test | 3,003 | 3,372 | 2,823 | | | 580 | 549 | 180 |
| loc | production | 181,993 | 187,827 | 549 | 344 | 205 | 0 | 965 | 442 |
| loc | test | 112,528 | 130,573 | 362 | 104 | 258 | 0 | 549 | 180 |
| clone_pairs | production | 489 | 376 | 78 | 63 | 15 | 0 | 30 | 19 |
| clone_pairs | test | 1,558 | 1,647 | 139 | 48 | 91 | 0 | 49 | 14 |
| fan_in_files | production | 7,900 | 11,030 | 612 | 70 | 542 | 0 | 625 | 156 |
| fan_out_files | production | 6,070 | 7,975 | 427 | 57 | 370 | 0 | 624 | 226 |
| fan_in_crates | production | 1,030 | 1,132 | 18 | 6 | 12 | 0 | 114 | 31 |
| fan_out_crates | production | 1,283 | 1,419 | 30 | 23 | 7 | 0 | 250 | 140 |
| parameters | production | 5,008 | 5,552 | 29 | 15 | 14 | 0 | 613 | 317 |
| cycle(循環の中の unit) | production | 976 | 1,122 | 475 | 120 | 203 | 152 | 213 | 150 |
| has_doc(doc の無い unit) | production | 1,506 | 1,882 | 23 | 7 | 16 | 0 | 521 | 154 |
| reference_status(未参照) | production | 21 | 21 | 1 | 0 | 0 | 1 | 3 | 3 |
| clone pairs (exact) | production | 55 | 29 | 18 | 0 | 0 | | 5 | 26 |
| clone pairs (normalized) | production | 280 | 234 | 117 | 5 | 3 | | 38 | 55 |
| module edges | production | 1,558 | 2,152 | 1,371 | 189 | 89 | | 781 | 187 |
| module edges | test | 1,064 | 1,595 | 1,011 | 82 | 179 | | 584 | 53 |
| crate edges | production | 27 | 27 | 27 | 0 | 11 | | 0 | 0 |
| module cycles | production | 17 | 21 | 17 | 1 | 5 | 2 | 5 | 0 |
| module cycles | test | 2 | 2 | 2 | 0 | 0 | 0 | 0 | 0 |

(`summary` は全 44 行。ここでは本番の主な行とテストの一部を抜いた。layer は層の設定が無いので空、debt_lines は本番で 0 → 0。)

軸ごとの大きな変化(本番、モジュールを除く残った unit、差の大きい順):

- size:`definitions`(`tools/catalog.rs`)817 → 3、`SessionLoopState.run` 356 → 105、`perform` 218 → 15。
- duplication:`dispatch_invoke` の `clone_pairs` 14 → 0、`spawn_terminal` 10 → 1、`resolve_filesystem_denial_retry` 6 → 0。組では normalized の `dispatch_invoke` 内の組(164 行)と `definitions` 内の組(112 行)が消え、`Evidence` 内の組(74 行)が現れた。
- coupling:`OccurrenceId` の `fan_in_files` 12 → 66、`Event` 65 → 90、`ToolCallRequest` 43 → 67。`WorkspaceShell` の `fan_out_files` 51 → 60。モジュールの辺では `session_lifecycle.rs → workspace/mod.rs` の本番の unit 辺が 56 → 20、`tools/approval.rs → contract.rs` が 70 → 39。crate の辺は重みの変わった 11 本がすべて重くなった(`horizon-agentd → horizon-agent` 1,626 → 1,943 など)。
- interface:`dispatch`(`horizon-cli/src/board.rs`)の引数 15 → 4、`drain_pty_output` 11 → 7、`process_pty_chunk` 10 → 6。
- independence:18 個のモジュールの循環(`judge/handle.rs` を最小のメンバーとする)が `providers/rig/completion.rs` の 2 個の循環と合わさって 40 個になった(2 行とも kept、`merged`)。2 個の循環が 5 つ現れた(`session/approval.rs`、`terminal/paint.rs`、`tools/processing.rs`、`agent/view/composer.rs`、`logd/writer/messages.rs`)。
- 現れた・消えた大きな unit:現れたものは `build_definitions` 332、`AgentTranscript.render_item` 313(`transcript/items.rs`)、`paint_terminal` 266(`terminal/paint.rs`)。消えたものは `run_sandboxed` 404(`tools/bash/exec.rs`)、`AgentTranscript.render_item` 321(`transcript.rs`)、`paint_terminal` 281(`terminal/mod.rs`)、`WorkspaceShell.spawn_workspace_restore` 271。同じ名前の組は別のモジュールへの移動で、決めたとおり消えた・現れたに出る。

残った本番の unit 3,538 個のうち、本文の変わらない 2,820 個の 722 個で軸の列が変わった(ほかの場所の呼び出しによる fan-in、循環への出入りなど)。本文の変わった 718 個のうち 663 個で軸の列も変わった。

**`git diff` で確かめた 6 行**(どれも合っていた):

1. `definitions`(`crates/horizon-agent/src/tools/catalog.rs`):前は 15〜831 行で空行でない行が 817、後は 16〜18 行の 3 行(`catalog().to_vec()`)で、本体は新しい `build_definitions`(25〜369 行、332 行)に移っている。比較の行は `loc` 817 → 3、`build_definitions` が appeared、`loc` 332。
2. `dispatch`(`crates/horizon-cli/src/board.rs`):前の `async fn dispatch(` は引数 15 個(`command`、`positionals`、…、`store`、`stdout`)、後は `command`、`options: &Options`、`store`、`stdout` の 4 個。`loc` は 213 → 216。比較の行と同じ。
3. `run_sandboxed`:前は `tools/bash/exec.rs` の 468〜890 行(404 行)、後は `exec.rs` に無く、`exec/sandboxed.rs`(`mod sandboxed;`)の 29〜144 行(114 行)にある。記述子が `tools/bash/exec/run_sandboxed().` から `tools/bash/exec/sandboxed/run_sandboxed().` に変わるので、消えた(404)と現れた(114)の 2 行になる。
4. `dispatch_invoke`(`src/control_plane.rs`):前の 170〜374 行の中に、jscpd の normalized の報告で両側がこの範囲に入る組が 14 個、合計 164 行あった。後のファイルには組が無い。比較は `clone_pairs` 14 → 0、組の行 164 → 0 で同じ。
5. `WorkspaceShell.spawn_workspace_restore`:前は `src/workspace/session_lifecycle.rs` の 845〜1124 行(271 行)、後は `src/workspace/restore.rs` の 170〜238 行(68 行)。`session_lifecycle.rs` は 1,194 行が消えて 30 行が足され、`restore.rs`(391 行)と `session_creation.rs`(412 行)が新しい。比較では消えた・現れたの 2 行で、`session_lifecycle.rs → workspace/mod.rs` の本番の unit 辺が 56 → 20 に減ったこととも合う。
6. `SessionId`(`crates/horizon-wire/src/session.rs`):`git diff e223bc20 46c37290 -- crates/horizon-wire/src/session.rs` は空で、比較の `text_changed` は false、`references` だけが 696 → 861 に増えている(ほかのファイルからの参照)。

### 判断モデルの範囲(2e6a5f32 → 2a49dc89)

`summary` の本番の主な行:units 5,106 → 5,184(kept 5,104、本文の変化 79、現れた 80、消えた 2)、テスト 3,633 → 3,723(kept 3,614、本文の変化 39、現れた 109、消えた 19)。`loc` は本番で 6 個が減り 56 個が増えた。`fan_in_files` は 65 個がすべて増えた。クローンの組は本番の exact が 35 → 35、normalized が 288 → 292(現れた 4、消えた 1)。モジュールの辺は本番で 2,172 → 2,208(現れた 38、消えた 2、軽く 3、重く 10)。循環は 22 → 22 で 2 つが大きくなった(`auxiliary.rs` の循環に `decision/config.rs` が加わり 4 → 5、`session/input.rs` の 2 個の循環に `session/continuation.rs` が加わり 3)。

三度目の再評価が足りないとしたものとの対応:

- **`const`/`static` の行。** 今は unit なので行として出る。本番で現れた定数 26 個のうち、判定の指示文 `BASE_INSTRUCTIONS`(25 行)・`GIT_INSTRUCTIONS`(25 行)・`HOST_EXECUTION_INSTRUCTIONS`(6 行)・`ASK_HUMAN`、継続判定の `INSTRUCTIONS`・`CONTINUATION_PROMPT`・`MODEL_CRITERION`・`USER_CRITERION`・`QUESTION`、しきい値 `DEFAULT_ASK_THRESHOLD`・`DEFAULT_CONTINUATION_THRESHOLD`・`ASK_THRESHOLD_VAR`・`CONTINUATION_THRESHOLD_VAR`・`MAX_CONTINUATIONS_PER_TURN` などがある。`STAGE1_MAX_TOKENS`・`STAGE2_MAX_TOKENS` は `judge/mod.rs` から `judge/chat.rs` に移り、消えた・現れたの 2 組になる。設定の `SMALL_MODEL_KEY`・`MODEL_PAIR_TOP_LEVEL_KEYS`・`TOP_LEVEL_KEYS` は本文の変わった kept。前の Git の指示文は `judge/prompt.rs` の `user_content` の中の文字列だったので、定数の行は無く、`user_content` の `loc` 99 → 75 と本文の変化に出る。
- **本文の変化と変わらないもの。** `text_changed` で分かれる。本番の残った unit は、本文が変わらず軸も変わらない 4,914、本文は変わらず軸だけ変わった 111、本文が変わって軸も変わった 73、本文だけ変わった 6(`fold` モジュール、`parse_stage1_result`、`SessionLoopState.handle_task_wake`、`summarize_session_title`、`RawNamedProviderConfig`、`model_pair_warnings`)。再評価が指摘した「両端で本文が同じなのにコミット数 2 の 55 行」は、本文のハッシュで比べるので変化として出ない。
- **辺の重みの変化。** kept の辺にも両側の unit 辺と差が出る。本番のモジュールの辺で重みの変わった 13 本は、`judge/handle.rs → judge/mod.rs` 17 → 23、`judge/mod.rs → judge/client.rs` 8 → 2、`judge/record.rs → judge/mod.rs` 10 → 14、`judge/mod.rs → horizon-sandbox/src/policy.rs` 5 → 2 など。crate の辺は `horizon-agentd → horizon-config` の本番 22 → 24、`horizon-agent → horizon-sandbox` の本番 68 → 66・テスト 84 → 90 など 4 本。
- **テストの分離。** すべての出力に範囲の列があり、`summary` は本番とテストを別の行に出す。消えた 21 個のうち 19 個はテストで、`judge/mod.rs` の `mod tests` の関数と `JudgeHandle.for_test` などである(移動したテストは消えた・現れたに出る)。
- 再評価のほかの指摘のうち、カウントの裏のコミットの一覧は履歴を外したので無い。親子のモジュールの辺は `compare_edges` の `nested_modules` で印が付き、循環は従来どおりそれを除いて作る。Rust 以外のファイルは今も範囲の外である。

### 時間

両側のキャッシュがあるとき(各クエリについて別の `code-db compare` を 1 回):

| クエリ | 一掃:全体 | 一掃:クエリだけ(3 回の最小〜最大) | 判断モデル:全体 | 判断モデル:クエリだけ |
| --- | ---: | ---: | ---: | ---: |
| summary | 5.6 s | 1.22〜1.29 s | 5.7 s | 1.37〜1.41 s |
| units | 4.8 s | 0.81〜0.89 s | 4.5 s | 0.86〜0.93 s |
| unit_axes | 4.2 s | 0.60〜0.65 s | 4.6 s | 0.65〜0.68 s |
| clones | 3.8 s | 0.50〜0.54 s | 4.3 s | 0.55〜0.59 s |
| edges | 3.4 s | 0.01〜0.02 s | 4.4 s | 0.02 s |
| cycles | 4.2 s | 0.03〜0.06 s | 3.4 s | 0.03〜0.05 s |
| layer_violations | 3.9 s | 0.01 s | 3.8 s | 0.01 s |

全体のうち BEFORE の読み込みと写しが 1.7〜2.8 s。クエリだけの時間は、AFTER を読んだあとの同じセッションで各クエリを `CREATE TEMP TABLE … AS` で 3 回走らせて測った。両側のキャッシュを消した直後の 1 回(両側のキャッシュを作る)は 7.0 s で、BEFORE の写しが 3.4 s だった。

キャッシュを使わない生のビューの上では、fixture のクエリが 4 分半たっても終わらなかった(どこに時間が掛かっているかは追っていない)。`code-db` と `check_compare.py` はどちらも `refresh.sql` を読む。

## 限界

- **モジュールをまたぐ移動と名前の変更は消えた・現れたになる。** 決めたとおりだが、一掃では消えた本番の unit 442 個のうち 289 個に、現れた unit の中に同じ `(crate, kind, qualified_name)` を持つものがあった(テストを含めて 460 組。`contract.rs` → `contract/tool_result.rs` などのサブモジュールへの分割)。読み手は名前で見比べることになる。
- **同じ記述子の unit の組は位置で決まる。** パスが同じものを行の順に、残りをパスと行の順に組むので、ターゲットのあいだを移った関数や、関数本体の中の同じ名前の項目が入れ替わったときは、組が実際と違うことがある(`ambiguous` で印が付く)。一掃では kept の 49 個がこれに当たり、49 個ともパスで組になった(順番で組になったものは無い)。
- **`summary` の数え方。** 循環の `kept`・`improved`・`regressed` は組の数なので、2 つの循環が合わさると 2 回数える(一掃の 40 個の循環)。辺は本番とテストの両方に数えられることがある。列の合計(`before_total`)は数の列では値の和、ほかの列では障害に当たる unit の数で、単位が行ごとに違う。
- **`cycle` の列は循環の名前なので、名前が変わっただけの残りは `changed` になる**(一掃で 152 個)。循環の中にいるかどうかの出入りだけが improved・regressed になる。
- **本文のハッシュは行全体と字下げを含む。** `rustfmt` の整形だけの変化も本文の変化になる。範囲が SCIP の `enclosing_range` なので、前置きの doc の変化も含む。
- **モジュールの同一性はパス。** ファイルを動かすと、そのモジュールの辺と循環のメンバーは消えた・現れたになる。
- **列の向きは SQL に書いた既定である。** `unit_quality` に足された数の列は「小さいほど良い」と読まれる。そうでない列が足されたら `compare_column_rules` に行を足す。
- **生のビューでは遅い。** キャッシュ無しで比較のビューを読むと終わらない。原因は調べていない。
- **BEFORE の読み込みはキャッシュを作ることがあり、BEFORE の `target/code-db/cache/` に書き込む**(`query` と同じく、ほかの入力の状態のキャッシュを消す)。BEFORE のチェックアウト自体には書かない。
- **索引を作った rust-analyzer の版が両側で違うと、その違いも差に出る。** 今回は 4 つとも同じ版で作った。比較はそれを確かめない。
