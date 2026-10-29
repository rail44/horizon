# コードデータベース:unit ごとの構文木と、それに対する分岐と入れ子の数え方(2026-10-29)

この記録は、SCIP の各 unit に、その範囲の本文を tree-sitter で解析した完全な構文木を持たせ、分岐の数と入れ子の深さをその木に対するクエリとして書いた増分の記録である。依頼はブランチ `code-quality-survey` の b3887ee3 からで、作業はワーカーのブランチで行った。設計文書(`docs/code-database-design.md`)の更新は依頼側に残している。

## 依頼と前提

2026-10-29 の複雑さの記録(`code-db-complexity-2026-10-29.md`)は、SCIP の索引には制御構造が現れず、分岐の数と入れ子の深さは SCIP からは表せないと結論した。その後のオーナーの決定は次のとおりである(依頼文による)。

- 分岐は unit の本文の構文木から数える。「本文の中のキーワードを数えることはできます」と言われたので、その内容をtree-sitterでASTとして扱えばよいのではないか」。
- 木は目的で絞らず、完全なまま格納する。「出す/出さないはそれによって左右されるものではない…普通に完全なツリーを全て格納すればいいのではないの？」。何を数えるかはクエリで決める。
- 道具は「Aでお願いします」。sitting_duck のスカラー関数 `parse_ast_list(text, 'rust')` を DuckDB の中で unit の本文に当てる。新しい Rust は書かず、`code-db` に木を作る処理は足さない。
- unit の本文を単独で解析してよい。依頼側が af0d26a7 の木で測った結果:ファイルのモジュールでない 8,959 個の unit のうち、単独で解析して ERROR・MISSING のノードが出るものは 0 個。`if_expression`、`match_arm`、`for/while/loop_expression`、`try_expression`、`closure_expression` の数は、ファイル全体を解析して unit の行に絞ったものと 8,959 個すべてで一致した(各 13,813)。
- 選んだトークンを別の SCIP 索引に書く先の試み(ブランチ `worktree-agent-ad8517c760dd346f6`)は破棄され、そこからは何も取っていない。

## 対象と方法

- ブランチの先端 b3887ee3 を、ワーカーの作業ツリーの `target/code-db/clone` に clone し、そこで `code-db index` を動かした(jscpd 2.7 s、rust-analyzer はルート 181.0 s、`preview-plugin` 80.7 s、他の 3 つは 9.6〜14.7 s)。索引のあとに各ワークスペースの `target/debug` を消した。
- rust-analyzer 1.99.1、DuckDB 2.0.0、sitting_duck v1.16.0(コミュニティ拡張、`INSTALL sitting_duck FROM community`)。比較に rust-code-analysis 0.0.25(リファクタリング監査の固定版、以下 RCA)を使った。
- 機械はオーナーのデスクトップで、他のワーカーと共有している。時間はすべて `nice -n 19` での値で、他の負荷の下で測った。
- この木の unit は 9,335 個(ファイルのモジュール 532、それ以外 8,803)である。依頼側の 8,959 個は af0d26a7 の木で、木が違う。
- 測定用の SQL とスクリプトは作業ツリーの `target/scratch/` に置き、コミットしていない。

## 作ったもの

- `00_load.sql`:`INSTALL sitting_duck FROM community; LOAD sitting_duck;` を戻した。削除前(974c3605 の親)も版を固定していなかった。コミュニティ拡張は DuckDB の版ごとにリポジトリの今の版が配られ、`INSTALL` に版を指定する形は使っていない。代わりに、読み込んだ版を `refresh.sql` のキャッシュの鍵に入れた(文法が変われば木が変わるため)。確認した版は v1.16.0 である。
- `views/units.sql` に `unit_ast`、`views/quality.sql` に `unit_own_ast`・`unit_complexity` を足し、`unit_quality` に `decisions` と `max_nesting` の 2 列を足した。
- `queries/complexity.sql`(問い、規則、証拠の強さ、`loc` との相関を見出しに書いた)。
- `refresh.sql`:`unit_ast` を `units` の後、`unit_complexity` を `unit_debt_markers` の後の層として写す。
- `check.py`:`complexity` をクエリの一覧に、`unit_ast` と `unit_complexity` をキャッシュの層の一覧に足し、制御構造の形が分かっている crate `flow` の fixture を足した。`README.md` を合わせた。
- ビューのファイルの一覧(`code-db` の `VIEW_FILES`)は変えていない。新しいビューは既存の 2 ファイルに入れたので、Rust の変更は無い。

## 保存した木の形

`unit_ast` は unit ごとの木のノード 1 つにつき 1 行である。本文は unit の範囲の行 `[start_line, end_line]` を丸ごと取り出して改行でつないだもの(依頼側の探索の SQL と同じ)で、ファイルの言語(`files.language`)で `parse_ast_list` に渡す。

列は `parse_ast_list` が返す struct のフィールドをそのまま開いたものである。

| 列 | 中身 |
| --- | --- |
| `unit_id` | unit |
| `node_id` | unit の木の中の前順の位置(0 始まり、根の `source_file` が 0)。子孫は `node_id + 1` から `node_id + descendant_count` |
| `type` | tree-sitter のノードの型。名前つきのノード(`if_expression`)も匿名のトークン(`if`、`&&`、`?`)も入る |
| `semantic_type`、`flags`、`name`、`qualified_name`、`signature_type`、`parameters`、`modifiers`、`annotations`、`receiver` | sitting_duck の読み(`flags` のビットは `is_syntax_only(flags)`、`is_scope(flags)` などで読む) |
| `start_line`、`end_line` | unit の本文の中の行(1 始まり、終わりを含む) |
| `file_start_line`、`file_end_line` | 同じ行のファイルの中の行 |
| `parent_id`、`depth`、`sibling_index`、`children_count`、`descendant_count` | 木の中の位置 |
| `scope_current`、`scope_function`、`scope_class`、`scope_module`、`scope_stack` | sitting_duck の `scope` struct(囲むスコープのノード id とその積み重ね) |

除いた列は 3 つである。`peek`(本文の切り詰めた先頭で、表示用)、`file_path`(`parse_ast_list` では常に `<inline>`)、`language`(ファイルの言語そのもの)。`peek` は数えるのに要らない:演算子は `binary_expression` の子のトークン(`&&`、`||`)として型に現れる。

`parse_ast_list` は列(column)も byte の位置も返さない(2026-10-24 の sitting_duck の評価のとおり。`source := 'full'` を取るのはリテラルしか受けない `parse_ast` だけ)。そのため、ノードの位置は行でしか分からない。tree-sitter のフィールド名と `is_named` も出ない。

sitting_duck の出力で欠けたトークン(tree-sitter の MISSING)を見分ける方法は無い:MISSING は幅 0 のトークンとして元の型名で出るはずで、列が無いので幅が見えない。ERROR のノードは `type = 'ERROR'` で出る(閉じない `{` を与えて確かめた)。

**大きさ**(9,335 個の unit):

| | ノード | 解析の CPU 時間(1 スレッド) | 本文の文字数 |
| --- | ---: | ---: | ---: |
| ファイルのモジュール以外 8,803 個 | 1,863,560 | 68.6 s | 7,945,808 |
| ファイルのモジュール 532 個 | 1,575,301 | 52.5 s | 6,982,894 |
| 合計 | 3,438,861 | 121.2 s | 14,928,702 |

`type = 'ERROR'` のノードは 9,335 個すべての木で 0 個だった(ファイルのモジュールを含む)。

木のうち unit 自身のもの(`unit_own_ast`、下の帰属の規則)は 1,587,844 ノードで、保存した木は自身のノードの 2.17 倍になる。差は入れ子の unit の重複で、ファイルのモジュールの木が全体を、インラインのモジュール(`mod tests { … }`)の木がその中の関数を、もう一度持つ。

キャッシュのファイルは 90.5 MiB になり、そのうち `unit_ast` は 66.0 MiB である(別ファイルに `unit_ast` だけを写して測った)。`qualified_name` と `scope_stack` の 2 つのリストの列を除くと 62.2 MiB で、差は 3.8 MiB だった。

### マクロの中

マクロの呼び出しの引数は `token_tree` で、中はトークンの並びである。`assert!(if x > 0 { … })` の `if` は `if_expression` ではなく `token_tree` の子の `if` トークンになる。`macro_rules!` の本体も同じである。したがって、マクロの中の分岐は下の数え方のどれにも入らない。この木で、unit 自身の `token_tree` の直下にある制御のトークンは `if` 299(197 unit)、`&&` 117、`return` 32、`match` 31、`?` 28、`||` 21、`for` 6、`loop` 2 だった。多いのは `assert!`(285)、`tokio::select!`(68)、`matches!`(60)、`vec!`(21)、`assert_eq!`(21)、`format!`(14)である。

## 数え方の規則

規則はクエリ(`unit_complexity` のビュー)にあり、木には何も足していない。

### 帰属(innermost)

unit の木には、その中に入れ子になった unit(関数の中の `fn`、インラインのモジュールの item、関数の中の型や定数)のノードも入っている。`unit_own_ast` は、ファイルの行の範囲が、その unit を親とする unit(`parent_unit_id`)の範囲 `[start_line, end_line]` に収まるノードを除く。入れ子の unit を囲むノード(外側のブロックや根)は範囲がはみ出すので残る。`unit_occurrences` が出現を最も内側の unit に帰属させるのと同じ考え方で、行だけで判定する(列が無いため)。

行だけで判定するので、入れ子の unit と同じ行に親のコードがあると、そのコードは入れ子の側に入る。この木で、入れ子の unit 8,803 個のうち、始まりの行の unit より前に何かがあるものは 0 個、終わりの行の unit より後に `,`・`;` 以外があるものは 6 個で、6 個とも定数の後ろの行末コメント(`src/theme/scheme.rs`)だった。

クロージャは unit ではないので、クロージャの本体は書いた側の unit に数える。

### 数えるもの

| 列 | 数えるノード |
| --- | --- |
| `if_count` | `if_expression`(`if let` と、それぞれの `else if` を含む) |
| `else_if_count` | 親が `else_clause` の `if_expression` |
| `match_count` | `match_expression` |
| `arm_count` | `match_arm` |
| `guard_count` | `match_pattern` の子に `if` トークンがある腕 |
| `for_count`、`while_count`、`loop_count` | `for_expression`、`while_expression`(`while let` を含む)、`loop_expression` |
| `try_count` | `try_expression`(`?`) |
| `and_count`、`or_count` | 親が `binary_expression` か `let_chain` の `&&`・`||` トークン(`if let … && …` は `let_chain` の直下に `&&` が来る) |
| `let_else_count` | 子に `else` トークンがある `let_declaration` |
| `return_count`、`break_count`、`continue_count` | `return_expression`、`break_expression`、`continue_expression` |
| `closure_count` | `closure_expression` |

### 判断の数(`decisions`)

McCabe の判断点の数で、関数の循環的複雑度は `decisions + 1` になる。

`decisions` = if(else if を含む)+ while + for +(各 match の腕の数 − 1)+ ガード + `&&` + `||` + `?` + let-else

規則の出どころと、RCA との違い:

- 2026-10-24 の調査の「固定版と最新版の差」の節に書いた rust-code-analysis の issue #1249 は、McCabe からの逸脱として 3 つを挙げている。`match` の最後の腕まで数える(4 腕で 5、期待値 4)、裸の `loop` を判断として数える(期待値は数えない)、`let … else` を数えない(期待値は数える)。ここではこの 3 つを期待値のほうで数えた。#1249 の 4 つ目(`&&`/`||` の混在)は cognitive complexity の話で、循環的複雑度には関わらない。
- `?` とガードは #1249 が触れていない。RCA 0.0.25 を小さな関数で動かして確かめると、RCA は `?` を 1 つ(`let x = a?;` の関数が 2)、ガードを 1 つ(4 腕とガード 1 つの `match` が 6)と数える。`?` は失敗したときに早く戻る分岐、ガードは偽のとき次の腕へ進む分岐なので、ここでも 1 つずつ数えた。`&&`・`||` も RCA と同じく 1 つずつである。
- クロージャ:RCA はクロージャを別の空間として、関数の値にクロージャごとの入口の 1 を足す。ここではクロージャは unit ではなく、その分岐だけを書いた側の unit に足し、入口の 1 は足さない。

### 入れ子の深さ(`max_nesting`)

1 つのノードで同時に開いている制御構造の数の最大で、制御構造が無い unit は 0 である。制御構造は `if`、`match`、`for`、`while`、`loop`。`else if` は鎖の続きで、段を開かない(親が `else_clause` の `if_expression` は数えない)。クロージャと普通のブロックは段を開かない。祖先は `node_id` の区間(`a.node_id <= c.node_id <= a.node_id + a.descendant_count`)で数え、`parent_id` を再帰でたどらない。入れ子の unit の中のノードは除かれているので、関数の中の `fn` の段は外側に数えず、その `fn` 自身の木で 0 から数える。

## fixture

`check.py fixture` の最後に、crate `flow`(edition 2024)を持つ 2 つ目のリポジトリを作り、rust-analyzer で索引して、生と `refresh.sql` の写しの両方で次を確かめる。

| 関数 | 形 | 期待値(読んで数えたもの) |
| --- | --- | --- |
| `nested_loop` | `for` の中の `if` の中の `if … else if`、`continue` | if 3(else if 1)、for 1、continue 1、decisions 4、入れ子 3 |
| `classify` | ガード付きの `match` の中の `match`、ガードに `&&` と `\|\|` | match 2、腕 6、ガード 2、`&&` 1、`\|\|` 1、decisions 8、入れ子 2 |
| `chain` | `?` 2 つ | `?` 2、decisions 2、入れ子 0 |
| `closures` | 分岐を持つクロージャ 3 つ(`if` と `match`) | クロージャ 3、if 1、match 1、腕 2、decisions 2、入れ子 1 |
| `outer` | 中に `fn inner`、`if` と `return`、let-else | if 1、let-else 1、return 2、decisions 2、入れ子 1(`inner` の分岐を含まない) |
| `inner` | `if` の中の裸の `loop` と `break` | if 1、loop 1、break 1、decisions 1、入れ子 2 |
| `with_macro` | `assert!(if …)` と `matches!(…, … if …)` | すべて 0 |
| `while_let` | `while let` の中の `if let … && …` | while 1、if 1、`&&` 1、decisions 3、入れ子 2 |
| `picks.pick` | インラインのモジュールの中の関数 | if 1、decisions 1。モジュール `picks` 自身は 0 |

ほかに、各木の根が `source_file`(深さ 0、ファイルの行が unit の範囲と一致)であること、ERROR が無いこと、ファイルのモジュールの木が全ノード(740)を持ちながら自身のノードは根の 1 つだけであること、`outer` の木 102 ノードのうち自身のものが 60 であること、`with_macro` の `if` 2 つが `token_tree` の子のトークンで `pick` の `if` が `if_expression` であることを確かめる。既存の fixture の `unit_quality` の行には 2 列(すべて 0)が増え、`complexity` のクエリの行が増えた。`check.py fixture` は通った(3 分 58 秒、ほとんどは rust-analyzer の索引)。

## 10 個の関数での確認

複雑さの記録の 10 個の関数について、`unit_complexity` の値をソースと照らした。`cyc` は `decisions + 1`、`RCA` は RCA 0.0.25 の循環的複雑度(関数の空間の合計)である。

| 関数 | loc | if(else if) | match / 腕 / ガード | for / while / loop | `?` | `&&` / `\|\|` | let-else | クロージャ | decisions | 入れ子 | cyc | RCA |
| --- | ---: | --- | --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `box_draw_segments` | 133 | 1 (0) | 1 / 112 / 0 | 0 / 0 / 0 | 0 | 0 / 0 | 0 | 0 | 112 | 1 | 113 | 114 |
| `command_for` | 47 | 0 | 1 / 37 / 0 | 0 / 0 / 0 | 0 | 0 / 0 | 0 | 0 | 36 | 1 | 37 | 38 |
| `legacy_bytes` | 175 | 14 (2) | 5 / 38 / 0 | 0 / 0 / 0 | 0 | 3 / 5 | 0 | 0 | 55 | 2 | 56 | 61 |
| `match_initial_capability` | 56 | 10 (1) | 0 | 3 / 0 / 0 | 0 | 1 / 0 | 0 | 0 | 14 | 5 | 15 | 15 |
| `IndexReader<R>.next_part` | 41 | 1 (0) | 2 / 8 / 0 | 0 / 0 / 1 | 11 | 0 / 0 | 1 | 3 | 19 | 3 | 20 | 25 |
| `executable_index` | 55 | 0 | 3 / 14 / 4 | 0 / 3 / 1 | 0 | 0 / 3 | 0 | 2 | 21 | 4 | 22 | 28 |
| `client` | 100 | 0 | 0 | 0 / 0 / 0 | 0 | 0 / 0 | 0 | 8 | 0 | 0 | 1 | 9 |
| `WorkspaceShell.render_node` | 326 | 3 (0) | 3 / 7 / 0 | 1 / 0 / 0 | 0 | 4 / 0 | 0 | 23 | 12 | 4 | 13 | 39 |
| `long_report` | 194 | 0 | 0 | 0 / 0 / 0 | 0 | 0 / 0 | 0 | 0 | 0 | 0 | 1 | 1 |
| `split_statements` | 73 | 3 (0) | 2 / 14 / 8 | 0 / 1 / 0 | 0 | 3 / 0 | 0 | 0 | 27 | 4 | 28 | 30 |

ソースを読んで分かったこと:

- `box_draw_segments` は範囲の外の文字で早く戻る `if` 1 つと、`=>` が 112 行ある `match`(うち 3 つは `return None` の腕、もう 1 つは `if` の中の `return None`)。数えたとおりで、入れ子は `if` と `match` が並ぶだけなので 1。`command_for` は 37 腕の `match` 1 つだけである。
- `match_initial_capability` は `if requested != Read` > `for` > `if starts_with` > `for` > `if starts_with` の 5 段で、入れ子 5 は複雑さの記録の読み(5 段)と一致する。`if` は 10 個(`else if let` 1 つを含む)、`for` 3、`&&` 1、`continue` 2、`return` 1。
- `next_part` は `loop` の中の `match` の中の `match`(入れ子 3)。`?` 11 個、let-else 1、`return` 3、`continue` 1、`.map_err(|_| …)` などのクロージャ 3 つを、すべて行ごとに確かめた。
- `executable_index` は `loop` > `match` > `while let` > `match` の 4 段。ガードは `value if …` の 4 つ、`||` は複数行のガードの 3 つ、`break` 5、`return` 2。
- `legacy_bytes` は外側の `match`(10 腕)の中に、`match` 4 つ(11、5、4、8 腕)と `if` 14 個(`else if` 2)。`&&` 3 つは 1 つの複数行の条件、`||` 5 つは 3 つの条件にある。入れ子は `match` の中の `if` か `match` で 2。
- `split_statements` の中の `enum State` は入れ子の unit(型)で、その行は数えない。`while` > `match state` > `match c` > `if has_code` の 4 段、ガード 8 つ、`matches!` の中のパターンは数えない。
- `client` は制御構造が無く、クロージャ 8 つだけである。`render_node` はクロージャ 23 個の中に分岐が少しある(`match` 3、`for` 1、`if` 3、`&&` 4)。`long_report` は文字列だけで 0 である。
- **RCA との差は、どの関数でも規則の違いで説明がつく。** 差 = 各 `match` の最後の腕 + 裸の `loop` + クロージャの数 − let-else。`box_draw_segments` +1(腕)、`command_for` +1(腕)、`legacy_bytes` +5(腕)、`next_part` +5(腕 2、`loop` 1、クロージャ 3、let-else −1)、`executable_index` +6(腕 3、`loop` 1、クロージャ 2)、`client` +8(クロージャ)、`render_node` +26(腕 3、クロージャ 23)、`split_statements` +2(腕)、`match_initial_capability` と `long_report` は 0。

## `loc` との相関

非テストの function と method 3,425 個(複雑さの記録と同じ集合で、数も同じ)での Spearman の ρ。同順位には平均順位を与えた。

| 尺度 | ρ(`loc`) | 1 − ρ² |
| --- | ---: | ---: |
| decisions | 0.690 | 0.52 |
| max_nesting | 0.642 | 0.59 |
| if_count | 0.570 | 0.68 |
| arm_count | 0.402 | 0.84 |
| try_count | 0.238 | 0.94 |
| `&&` + `\|\|` | 0.245 | 0.94 |
| closure_count | 0.396 | 0.84 |
| return + break + continue | 0.523 | 0.73 |

decisions と max_nesting の間は 0.866 だった。

複雑さの記録の SCIP だけの尺度と比べると、decisions(0.69)と max_nesting(0.64)は、vocabulary(0.84)、length(0.86)、字下げの深さ(0.81)、locals(0.76)より `loc` から離れ、同時に生きている local の最大数(0.70)と同じくらいである。字下げの深さ 0.81 に対して構文の入れ子は 0.64 で、字下げが rustfmt の式の折り返しも数えていた分が消えたと見られる(`client` の字下げは 6、構文の入れ子は 0)。ただし、どちらが何を予測するかは測っていない。

分布:decisions は中央値 1、90 パーセンタイル 6、99 パーセンタイル 22、最大 112(`box_draw_segments`)で、1,591 個(46%)が 0。max_nesting は中央値 0、90 パーセンタイル 2、99 パーセンタイル 4、最大 6 で、1,840 個(54%)が 0。最も深い 6 は `ConversationHistory.messages`(29 行、decisions 6)で、`loc` が小さく入れ子が深い関数が上に来る。

decisions の多い順の先頭は、`box_draw_segments`(112)、`horizon-cli` の `board.rs` `dispatch`(60)、`legacy_bytes`(55)、`horizon-cli` の `cli.rs` `parse`(54)、`box_draw_geometry`(52)、`WorkspaceShell.execute`(46)、`apply_agent_event_to_frame`(37、入れ子 5)、`control_plane/invoke.rs` `parse`(37)で、ほとんどが長い `match` の振り分けである。

unit の種類ごとの decisions:function 1,710 個に 6,504、method 854 個に 2,836、module 3 個に 15、constant・type・macro は 0。module の 15 は、rust-analyzer が見ない cfg の無効なコード(`horizon-sandbox` の `linux/mod.rs` の `spawn_with_grants`、`horizon-sandbox-helper.rs` の 2 つ目の `main`、`src/board/execute.rs` の `run_store_job`)で、unit が無いのでファイルのモジュールに帰属した。

証拠の強さは軸の記録(3.3)のとおり弱〜中である:cognitive complexity は理解時間を予測する(r = 0.54)が正しさはほとんど予測せず、SIG の unit complexity は欠陥の解決と 0.50〜0.54、Rust の研究は無い(2026-10-24 の調査)。ここで作った decisions は McCabe 系で、cognitive complexity の入れ子による加算はしていない。

## 時間

- **キャッシュを作る**(`refresh.sql`、すべての層):43.0〜45.1 s、ピークの RSS 2.6 GB(`check.py step … --mode build`)。`unit_ast` を足す前は 1.0〜2.3 s だった。ほぼすべてが解析で、写した `unit_ast` からの `unit_complexity` は 0.14 s。
- **解析の並列度**:同じ解析を一時テーブル(9,335 行は 1 つの row group)から行うと 1 スレッドで 121 s、ビューの中では user 214 s に対して wall 45 s だった。24 スレッドのうち実際に並ぶのは数スレッドで、物化した CTE のチャンク(2,048 行)単位で並ぶためと見られるが、確かめていない。
- **キャッシュからのクエリ**:`complexity` 0.014 s、`unit_quality` 0.23 s。`code-db` の 1 回の起動の読み込みは約 3 s(以前と同じ)。
- **生**(キャッシュ無し):`complexity` 49 s、ピークの RSS 2.9 GB。
- 依頼文の停止条件(行ごとの `parse_ast_list` がこの規模で時間かメモリの面で使えない)には当たらなかった。ただし、入力が 1 か所でも変わるとキャッシュは丸ごと作り直しになり、そのたびに約 45 s かかる。

## 依頼文が決めていなかった判断

- **ファイルのモジュールも解析した。** ファイルのモジュールの本文はファイル全体で、それを解析して木にした。理由:どの unit にも同じ規則(範囲の本文の木)を当てられる。インラインのモジュールの木もその中身を持つので、重複は新しい性質ではない。unit の無いコード(rust-analyzer が見ない cfg の無効な item)もファイルのモジュールの自身のノードとして数えられる(上の 15)。代償は、ノード 1,575,301(全体の 46%)、解析の CPU 時間 52.5 s(43%)。解析しない場合、その分の時間と大きさが減り、cfg の無効なコードの分岐はどこにも数えられない。
- **列は `peek`・`file_path`・`language` 以外をすべて残した。** `scope_stack` と `qualified_name` は木から導けるが、関数が返す事実なので残した(3.8 MiB)。
- **sitting_duck の版は固定せず、キャッシュの鍵に入れた。**
- **帰属は直接の子の unit の行の範囲で除く**(孫は子の範囲に入る)。列が無いので行で判定した。
- **`?`、ガード、`&&`・`||`、let-else を判断として数え、裸の `loop` と各 `match` の最後の腕は数えない。** クロージャの入口は足さない。
- **入れ子の段は if・match・for・while・loop だけで、`else if` は段を開かず、クロージャも段を開かない。** SonarSource の cognitive complexity はクロージャ(ラムダ)で入れ子を深くするが、ここは制御構造の入れ子だけにした。
- **`unit_quality` に足したのは `decisions` と `max_nesting` の 2 列だけ**で、個々の数は `unit_complexity` と `complexity` のクエリにある。循環的複雑度(`decisions + 1`)はクエリで関数と method にだけ出し、`unit_quality` はすべての unit について判断点の数を持つ(型や定数は 0)。
- **新しいビューは既存のファイル(`views/units.sql`、`views/quality.sql`)に入れた。** 新しいファイルにすると `code-db` の `VIEW_FILES` と `check.py` の一覧を変える必要があった。
- **fixture は別のリポジトリにした。** 既存の fixture に関数を足すと、ほぼすべての期待値(ファイル、unit、グラフ、各クエリ)が動くため。

## 残る点

- マクロの引数の中の分岐(上の `if` 299 など)は数えていない。`token_tree` の中のキーワードのトークンを数える規則は書けるが、`matches!` のパターンのガードと本当の分岐を区別できない。
- cognitive complexity(入れ子による加算、`&&`/`||` の連なり)は作っていない。木と `node_id` の区間で書ける形ではある。
- 解析の並列度と、入力の変化ごとの全面的な作り直し(約 45 s)。unit の本文の md5 ごとに木を使い回せば変わった unit だけを解析できるが、キャッシュの形を変えることになる。
- MISSING のノードは sitting_duck の出力では見分けられない。依頼側の 0 件の測定は別の方法による。
