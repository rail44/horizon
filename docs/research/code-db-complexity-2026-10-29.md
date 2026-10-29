# コードデータベース:SCIP で複雑さと入れ子の深さを表せるか(2026-10-29)

## 依頼と結論

オーナーの発言(2026-10-29):

> 複雑さや入れ子の深さは、保存したSCIPのクエリで表現できそうですね。後続で検討しましょう

その後に「その3つに進めてもらってよいです」。

依頼は、保存した SCIP の索引と unit の範囲の本文で表せる範囲で、複雑さと入れ子の深さの尺度をビューとクエリとして足すことだった。まず SCIP が何を表せるかを確かめ、表せるものだけを作る。依頼文には「local の記号に入れ子に当たるものを SCIP が何も与えないなら、止めて報告する(その場合は記録が成果物で、何も作らない)」という停止条件があった。

結論:**rust-analyzer 1.99.1 の SCIP は、local の記号(変数、引数)にスコープの範囲を与えない。** 定義の `enclosing_range` は束縛のパターン自身(`x`、`mut x`、`&self`、`T: Clone`)で、参照の `enclosing_range` は定義のそれの写しである。ブロック、`if`・`match`・ループの本体、クロージャには範囲も記号も無い。停止条件に当たるので、ビュー、`unit_quality` の列、クエリ、fixture は作っていない。この記録が成果物である。以下は、調べた事実と、スコープを使わずに表せる尺度を測った結果である。

## 対象と方法

- ブランチ `code-quality-survey` の 514a1332 を worker の作業ツリーの `target/code-db/clone` に clone し、そこで `code-db index` を動かした(jscpd 2.7 s、rust-analyzer はルート 224.2 s、他の 4 workspace は 7.4〜155.2 s)。rust-analyzer は 1.99.1、DuckDB は 2.0.0。索引の後に clone の `target/debug` を消した。
- 小さな確認用の crate(`target/code-db/probe`)を別に作り、`rust-analyzer scip` の出力を `read_scip` と `read_scip_symbols` で直接読んだ。中身は 4 つの関数である:入れ子の `if` と `for` と早期の `return`、ガード付きの `match`、クロージャ 3 つと関数の中の `fn`、型引数と `if let`。
- 尺度は、`units` のうち `kind` が function か method で `is_test` でないもの 3,425 個(全 5 索引)について、`unit_occurrences`(出現をそれを含む最も内側の unit に帰属させたもの)から計算した。相関は、同順位に平均順位を与えた Spearman の順位相関で、相手は `units.loc` である。

## SCIP が local に与えるもの

### 定義の `enclosing_range`

ルートの索引の local の定義 29,667 個のうち、29,665 個で `enclosing_range` は 1 行に収まる。26,085 個は名前の範囲と同じで、残る 3,582 個は名前を含む少し広い範囲だった。その中身を種類ごとに数えると、Variable 2,029(`mut state` のような `mut` 付きの束縛)、SelfParameter 1,402(`&self`、`&mut self`)、TypeParameter 78(`T: serde::de::DeserializeOwned` のような境界付きの型引数)、Parameter 73 である。複数行にわたる 2 個は、どちらも `providers/rig/tests.rs` のテスト関数全体を指していた。属性マクロ(`#[tokio::test(...)]`)の展開から来たものと見られるが、確かめていない。

確認用の crate でも同じだった。`if` の中の `let y` の `enclosing_range` は `y` の 1 文字で、それを囲む `if` のブロックではない。

### 参照の `enclosing_range`

local の参照 53,303 個のうち、定義が同じ文書にあるもの 53,276 個のすべてで、参照の `enclosing_range` は定義の `enclosing_range` と同じ値だった。参照の位置を囲む範囲ではない。非 local の参照について 2026-10-29 の tree-sitter との対応の記録が書いたこと(81,765 件のほとんどが参照先の定義の範囲)は、local についても成り立つ。

### 記号の情報

- `kind`:Variable 18,759、Parameter 9,237、SelfParameter 1,481、TypeParameter 190(ルートの索引)。`match` の腕の束縛(`Some(n)`)、`if let`・`while let`・`for` の束縛は Variable である。
- **クロージャの引数も Parameter である。** 確認用の crate では `|e| e * 2` の `e` は Parameter で、`enclosing_symbol` はクロージャではなく、それを含む関数(`closures().`)だった。クロージャは記号も範囲も持たない。
- `enclosing_symbol` は、すべての local で、それを含む名前付きの item を指す。関数の中の `fn` の引数は、その内側の `fn` を指す。
- `syntax_kind` は書かれていない(tree-sitter との対応の記録のとおり)。

### 入れ子として残るもの

関数の中の `fn` などの item は、それ自身の unit になり、範囲を持つ。これは今の `units.parent_unit_id` ですでに表されている。item より細かい入れ子(ブロック、制御構造、クロージャ)は索引に無い。

## 表せるもの、表せないもの

依頼文が例に挙げた尺度ごとに分けた。

| 尺度 | 表せるか | 理由 |
| --- | --- | --- |
| local のスコープの入れ子の最大の深さ | 表せない | スコープの範囲が無い |
| スコープの数 | 表せない | 同上 |
| スコープあたりの行数 | 表せない | 同上 |
| local の数 | 表せる | Variable の定義を数える |
| クロージャの数 | 近いものだけ | Parameter の定義の数から、signature の引数の数(`units.parameters` から self を引いたもの)を引くと、クロージャの引数の数になる。引数の無いクロージャ(`\|\| …`)と `_` だけのもの(`\|_\| …`)は数えられず、引数が 2 つのクロージャは 2 と数える。signature の引数が `_` だと差が負になり(3,425 個のうち 15 個)、0 に丸めた |
| 参照する記号の種類の数(vocabulary)と参照の出現の数(length) | 表せる | unit に帰属する参照の出現 |
| unit の中の fan-out | 表せる | 参照する非 local の記号の種類の数(unit 単位の `unit_fan_out` は既にある) |
| 同時に生きている local の最大数 | 表せる | 定義の行から最後の参照の行までを生存とみなし、行ごとに数えた最大値。スコープではなく、最後に使われた位置で切る |
| local の最長の生存行数 | 表せる | 同上の区間の長さの最大値 |
| 分岐の数(循環的複雑度) | 表せない | `if`・`match` の腕・`&&`・`\|\|`・`?` は記号を持たない |
| cognitive complexity の増分(`if`・`match`・ループ・`&&` と入れ子による加算) | 表せない | 同上。入れ子による加算はスコープも要る |
| 早期の脱出(`return`、`break`、`continue`、`?`) | 表せない | 同上 |

括弧を数えて分岐や入れ子を近似することは、依頼文のとおりしていない。

### 本文の行の規則で表せる近いもの(SCIP ではない)

依頼文は、unit の範囲の本文に対する軽い規則(1 つの正規表現)を許している。入れ子の深さに最も近いのは字下げである:unit の名前の行の字下げを基準に、範囲の空でない行の先頭の空白の幅(タブは 4)を 4 で割った最大値(`indent_depth`)と総和(`indent_total`)。行ごとの正規表現 1 つで、行をまたいで括弧を数えることはしない。これは SCIP の情報ではないので、停止条件の外で比べるために測った。作ってはいない。

## 10 個の関数での確認

形の違う関数を、本文の行の数え(`=>` の行、`if` の行、ループの行、脱出の行)で選び、ソースを読んで尺度と照らした。`indent` は `indent_depth`、`live` は同時に生きている local の最大数、`cl` はクロージャの引数の数である。

| 形 | 関数 | loc | locals | cl | vocabulary | length | live | indent |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 長い `match`、束縛なし | `src/terminal/glyphs.rs` `box_draw_segments` | 133 | 1 | 0 | 17 | 568 | 2 | 3 |
| 長い `match`、束縛なし | `src/keymap.rs` `command_for` | 47 | 0 | 0 | 42 | 113 | 1 | 2 |
| `match` と `if` と束縛 | `horizon-terminal-core` `kitty_keyboard.rs` `legacy_bytes` | 175 | 17 | 0 | 76 | 303 | 7 | 4 |
| 深い `if` と `for` の連なり | `horizon-sandbox-runtime` `linux/open.rs` `match_initial_capability` | 56 | 10 | 0 | 37 | 83 | 8 | 6 |
| ループと早期の `return` | `scip-index-core` `IndexReader<R>.next_part` | 41 | 7 | 0 | 37 | 87 | 4 | 6 |
| `while let` と `match` の入れ子 | `horizon-agent` `tools/bash/shell.rs` `executable_index` | 55 | 7 | 1 | 21 | 94 | 3 | 8 |
| クロージャの多い builder | `src/runtime/agent.rs` `client` | 100 | 8 | 18 | 68 | 147 | 10 | 6 |
| クロージャの多い描画 | `src/workspace/render.rs` `WorkspaceShell.render_node` | 326 | 35 | 37 | 233 | 470 | 16 | 13 |
| 文字列だけの長い関数 | `src/board/previews.rs` `long_report` | 194 | 0 | 0 | 5 | 5 | 0 | 3 |
| 状態機械(ループの中の `if`) | `duckdb-ext/code-db/src/script.rs` `split_statements` | 73 | 8 | 0 | 41 | 175 | 8 | 6 |

読んで分かったこと:

- **束縛の無い分岐は見えない。** `box_draw_segments` は `=>` の行が 112 ある `match` だが、local は 1 つ(`codepoint`)で、腕の数はどの SCIP の尺度にも出ない。`command_for` も 37 の腕で local は 0 である。length だけが腕の量に応じて大きい(`box_draw_segments` は列挙子 `Light`・`Heavy` などの参照が 568)。length が数えているのは参照の量で、分岐ではない。
- **束縛のある制御構造は local として見える。** `match_initial_capability` の 10 個の local には、2 つの `for` の `capability`、内側の `for` の `excluded`、`let covers`、`let score`、`if let Some(capability)` が入る。ただし入れ子の深さ(`if` > `for` > `if` > `for` > `if` の 5 段)は local からは出ず、字下げ(6)にだけ出る。`if requested != AccessMode::Read` のような束縛の無い `if` は local に何も残さない。
- **早期の脱出は見えない。** `next_part` の `return` 3 つ、`continue`、`?` 9 つは、どの SCIP の尺度にも出ない。local の 7 個(`key`、`number`、`wire_type`、`len` 2 つ、`part`、`read`)は数えたとおりである。`.map_err(|_| …)` のクロージャは引数が `_` なので数えられない。
- **`executable_index`** の local 7 個には、`match` のガードの束縛 `value` 4 つが入る。クロージャ 2 つのうち、`|word|` は数え、`|_|` は数えない(`cl` = 1)。
- **クロージャの引数の数はクロージャの数ではない。** `client` のクロージャは 8 つで、それぞれ引数が 2〜3 個あるので `cl` は 18 になる。`client` に制御構造は無いが、builder の連鎖と複数行の引数に rustfmt が付ける字下げで `indent_depth` は 6 になる。字下げは制御構造の入れ子と、式の折り返しの両方を数える。
- **`long_report`** は `format!` と `concat!` に文字列を並べた 194 行で、vocabulary 5、length 5 である。SCIP の尺度は、loc が大きくても読むべき状態が少ない関数を小さく出す。字下げは 3 である。
- **`split_statements`** の `live`(8)は、ループの外で宣言されループの中で更新される状態変数(`chars`、`statements`、`current`、`has_code`、`state`、`i`)とループの中の `c`、`next` の数に当たる。`max_span` は 58 行で、関数のほぼ全体にわたって生きている状態があることを示す。
- 関数の中の `fn` は別の unit なので、その本体は外側の関数の尺度に入らない。クロージャの本体は入る。

## `loc` との相関

非テストの function と method 3,425 個(`loc` の中央値 12、90 パーセンタイル 42、99 パーセンタイル 135)での Spearman の ρ。1 − ρ² は、`loc` の順位で説明されない順位の分散の割合で、その尺度が `loc` に足すものの上限の目安である(足したものが何を予測するかは測っていない)。

| 尺度 | 何を数えるか | ρ(`loc`) | 1 − ρ² |
| --- | --- | ---: | ---: |
| locals | Variable の定義の数 | 0.755 | 0.43 |
| closure_params | クロージャの引数の数(上の近似) | 0.370 | 0.86 |
| vocabulary | 参照する記号の種類の数 | 0.839 | 0.30 |
| length | 参照の出現の数 | 0.856 | 0.27 |
| external_vocabulary | 参照する非 local の記号の種類の数 | 0.821 | 0.33 |
| local_vocabulary | 参照する local の種類の数 | 0.746 | 0.44 |
| max_live | 同時に生きている local(引数と self を含む)の最大数 | 0.697 | 0.51 |
| max_span | Variable の最長の生存行数 | 0.771 | 0.41 |
| indent_depth(本文、SCIP ではない) | 字下げの最大の深さ | 0.811 | 0.34 |
| indent_total(本文、SCIP ではない) | 字下げの総和 | 0.911 | 0.17 |

分布:locals は中央値 1、90 パーセンタイル 7、最大 50 で、1,630 個(48%)が 0。closure_params は 2,473 個(72%)が 0、最大 83。max_live は中央値 3、最大 27。indent_depth は中央値 2、90 パーセンタイル 5、最大 13 で、992 個が 1 以下(本体が平らな関数)。

軸の記録(3.3)は、複雑さはサイズと強く相関すると書いている。ここでも、SCIP から出る尺度のうち vocabulary と length は `loc` とほぼ同じ順位を与える(ρ 0.84〜0.86)。`loc` から最も離れるのは closure_params と max_live である。closure_params は描画と builder のコードに偏り(上の `client`、`render_node`)、分岐の量ではない。max_live は「読むときに保持すべき状態の量」(軸の記録 3.3 の (a))に最も近いが、スコープではなく最後の参照で生存を切るので、ブロックを抜けて死んだ変数と、最後まで使われる変数を区別しない。

証拠の強さ:軸の記録のとおり、複雑さと入れ子の行は弱〜中で、cognitive complexity は理解時間を予測するが正しさをほとんど予測しない(2026-10-24 の調査)。上の尺度はどれも、その研究で測られた量(分岐、入れ子の増分)ではない。local の数、生存区間、vocabulary と保守性や欠陥との関係は、この記録では調べていない。

## 作ったもの

何も作っていない。停止条件(local の記号に入れ子に当たるものが無い)に当たったためである。調べるのに使った SQL は clone の側に置いた一時的なもので、ブランチには入れていない。

## 判断が残る点

- 停止条件は「入れ子」についてのものだった。入れ子でない尺度(locals、vocabulary、length、max_live、max_span、closure_params)は SCIP で表せ、上のとおり測れる。それらを作るかどうかは、依頼文の範囲では決まっていない。
- 字下げ(`indent_depth`)は本文の 1 行ごとの規則で入れ子の深さに近いものを出すが、SCIP の情報ではなく、rustfmt の式の折り返しも数える。依頼文の「本文の軽い規則は許す」に当たるかどうかは決まっていない。
- 分岐の数、cognitive complexity、早期の脱出は、構文を読まない限り表せない。構文のパーサは戻さない、という前提(tree-sitter と sitting_duck は削除済み)は変わっていない。
