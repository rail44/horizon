# コードデータベースの 3 回目の dogfood:後ろ向きの表のまとめと、前後比較の評価(2026-10-28)

コードデータベースの SQL 層の 3 回目の dogfood の後半である。2 部に分ける。第 1 部は、これまでの記録に散らばる後ろ向きの P@k をひとつの表にまとめ、今の SQL(b3b94fc1)で最後の列を測り直したもの。第 2 部は、14 個目の増分の前後比較(`compare_units`、`compare_graph`、`compare_cycles`)を、9 月のスイープの標本と、1 本の実際のブランチ(決定モデルの導入)で読んだものである。SQL、Rust、`check.py` は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、オーナーは 2026-10-28 に、提案ごとの相談のあとで次のように決めた。後ろ向きの確認は最小の形で行う(記録から組み立てた表に、今の SQL で測り直した最後の列と `loc` の列を付ける)。比較は (a) スイープの行を状態ごとに無作為に取り出して `git diff` と照らし、(b) 1 本の実際のブランチで評価する。(b) のブランチは、オーナーが好みを示さなかったので、依頼側が決定モデルの導入を選んだ(オーナーの発言は「3のbも入れましょう。6の一致率は不要です。」)。合成した信号のモデルと追加のスイープは、別の相談に回す。

前提は、[設計の文書](../code-database-design.md)(7〜14 個目の増分と Open points)、[後ろ向き 1](code-db-dogfood-retrospective-2026-10-26.md)、[後ろ向き 2](code-db-dogfood-retrospective-2-2026-10-26.md)、[数え方の記録](code-db-counting-rules-2026-10-27.md) 3 節、[帰属の記録](code-db-attribution-2026-10-27.md) 3 節、[クローンの側の記録](code-db-clone-sides-2026-10-27.md) 3 節、[継承の記録](code-db-inherited-lineage-2026-10-27.md) 5 節、[前後比較の記録](code-db-compare-2026-10-28.md) 1〜4 節、`duckdb-ext/sql/README.md` の「Before/after comparison」と `retro/` の説明、`duckdb-ext/sql/retro/*.sql`、`queries/compare_*.sql` である。記録の決定は問い直していない。

## 要約

- **最後の列(b3b94fc1)の正解は 778 / 92 / 30 で、記録と一致した。** `production` の 1 コミット以上と 3 コミット以上の、6 つの信号のすべての P@10 / P@50 / P@100 / recall@100 と順位の付く数が、継承の記録(12 個目の増分)の「後」の値と同じだった。13・14 個目の増分の SQL で動いた値は無い(1 節、2 節)。
- **スイープの標本 40 行のうち、状態と相手がどちらも正しかったのは 30 行、誤りは 8 行、判断できないものが 1 行、規則どおりだが読み方に注意の要るものが別に 7 行あった**(`renamed` の 10 行に含まれる。後述)。誤りの内訳は、`moved` の `ambiguous` 10 行のうち 6 行(同じコミットで一緒に移された兄弟の関数への、類似度 0.5〜0.6 の `content_match` の組)、`added` 5 行のうち 1 行と `removed` 5 行のうち 1 行(どちらもリンクが無いための誤り)である。`moved` の `ambiguous` でない 10 行はすべて正しかった(4 節)。
- **`renamed` の 10 行は、すべて文書の規則(体を追う)のとおりだったが、7 行は、前の名前の unit が HEAD にも残っていた**(本体を新しい関数に移し、古い名前は包む関数として残した形)。残った方は `added` の行になる(4.3 節)。
- **`kept` で `loc` が 50 % より大きく変わった行は 123 行だった**(4.6 節)。
- **決定モデルのブランチ(`2e6a5f32..2a49dc89`)では、`kept` 8,669(うち `ambiguous` 1)、`added` 174、`removed` 19、`renamed` 8、`moved` 1(`ambiguous`)、`missing` 0 だった。** モジュールの辺は `added` 66、`removed` 3、`kept` 3,451、循環は 25 個すべて `kept`(1 つはモジュールが 2 → 3 に増えた)だった(5 節)。
- **ブランチでは、コミット 1 以上の `kept` 126 行のうち 55 行が、両端で本文がまったく同じだった。** cad2cfab が `judge/parse.rs`、`client.rs`、`prompt.rs` を消し、ff10f1ef が戻したためで、55 行はすべてコミット数 2 である(5.3 節)。
- **ブランチの中の 2 段階の変更は、両端だけを見る読み方と食い違った。** 前の `run_judge`(2 段階の chat の判定)は `run_decision_judge` に `renamed` とされたが、行の集合の類似度は前の `run_judge` と `run_decision_judge` で 0.12、`added` とされた `run_chat_judge` で 0.71 である。`judge/mod.rs` で `removed` になったテストの 13 個は、同じ名前で `judge/chat.rs` に `added` されている(5.3 節)。
- **ブランチの最後のコミット 2a49dc89 の主な変更(Git の判定の方針の文言)は `const` の文字列にあり、`const` と `static` は unit でないので、`compare_units` に行が無い。** `judge/`、`decision/`、`continuation.rs` には HEAD で `const`/`static` が 37 個ある(5.4 節)。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の b3b94fc1 に合わせた(変更の無い状態で `git reset --hard`)。測定はこのリポジトリの使い捨ての複製 3 つ(作業ツリーの `target/code-db/` の下、`git clone --no-checkout` の後に `checkout`)で行い、作業ツリーのファイルと履歴は測定に使っていない。

- `retro/base`:e3c619a0(後ろ向きの基準の木)。`run.sh index` で索引と jscpd の報告を作った。
- `retro/end`:46c37290(スイープの終わり)。全履歴のキャッシュ。索引は、別の worker の作業ツリーにあった 46c37290 の索引を写した(依頼文の許可による。読むだけで、そのキャッシュには問いを投げていない)。jscpd は `jscpd.sh` で `src crates` に対して作った。
- `dm/head`:2a49dc89(決定モデルのブランチの先端)。全履歴のキャッシュ。同じ複製を先に 2e6a5f32 に `checkout` して索引を作って取り出し、2a49dc89 に戻して索引を作り直した。

e223bc20 の索引も同じ作業ツリーから写した。rust-analyzer が複製の `target/debug` に書いたビルドスクリプトの出力(1 回 2.5 GB)は、索引ができるたびに消した。ディスクの空きは始めに 19 GB、途中の最小が 16 GB だった。機械はオーナーの対話用デスクトップで、他の作業と共有している。DuckDB CLI 2.0.0、rust-analyzer、jscpd 5.3.1 を使い、すべて `nice -n 19` で動かした。測定用の SQL とシェル、問いの出力のファイルは作業ツリーの `target/code-db/scripts/` と `target/code-db/out/` に置き、コミットしていない。

# 第 1 部 後ろ向きの確認の表

## 1. 列と手順

列はこれまでの測定点である。値はそれぞれの記録から写し、最後の列だけを測った。

| 列 | 出どころ | 正解 | 信号 | 帰属 |
| --- | --- | --- | --- | --- |
| 第 1 回 | 後ろ向き 1、3 節 | 名前 | 名前で数えた履歴 | `all` |
| 第 2 回 | 後ろ向き 2、4 節の B | 名前か系譜 | 系譜で数えた履歴 | `all` |
| 8 | 数え方の記録、3 節の「fixed を innermost の正解で」 | 名前か系譜 | 5 つの規則 | innermost |
| 9 | 帰属の記録、3 節の「後」 | 名前か系譜 | 同上と、前置きの doc・空行の規則 | innermost |
| 11(`contain`) | 継承の記録、5 節の「前」(後述) | 名前か系譜 | 同上、`clone_sides` は `contain` | innermost |
| 12 | 継承の記録、5 節の「後」 | 名前か系譜 | 同上と、継承のリンク | innermost |
| 最後 | 今回、b3b94fc1 | 名前か系譜 | 今の既定 | innermost |

- **11 の列。** クローンの側の記録(11 個目の増分)の既定は `overlap` だった。この列は依頼文の指定どおり `contain` の値で、継承の記録 5 節の「前」(11 個目の増分の後の SQL を `contain` で動かしたもの)から取った。クローンの側の記録 3 節は、`contain` のクローンの値(1 コミット以上 0.8 / 0.72 / 0.66、0.085、3 コミット以上 0.3 / 0.2 / 0.19、0.207)と、ほかの信号の 1 コミット以上の P@100(churn 0.72、`fan_in_x_commits` 0.48、co-change 0.43、loc 0.75)だけを書いていて、どれも継承の記録の「前」と同じである。`overlap` の値は、1 コミット以上で 0.3 / 0.38 / 0.38、0.049、3 コミット以上で 0.1 / 0.12 / 0.10、0.109(順位の付く同一性 379)だった。
- **最後の列の手順。** 後ろ向き 2 の 9 節の手順で、今の既定の変数のまま動かした。`retro/changes.sql` と `retro/lineage.sql` を `retro/end` で `retro_since=e223bc20`(全履歴のキャッシュ)、`retro/signals.sql` を `retro/base` で、2 つの系譜のファイルを与え、`retro_scope=production`、`retro_min_commits` を 1 と 3 にした。範囲の unit の変更は 4,433 行、84 コミット、同一性 3,309 個で、前後比較の記録 3.3 節の値と同じだった。
- `fan_in_x_commits` と `fan_in` は `unit_fan_in_churn` の 2 つの並べ方、`clone_one_sided` は `clone_change_consistency`、`cochange_without_reference` は co-change の問いである。`module_cycles` は順位の無い集合で、欄は「正解に入る数 / 集合の大きさ」である。

## 2. 表

各欄は P@10 / P@50 / P@100 / recall@100 である。8 と 9 の列は、記録が recall@100 を小数 2 桁で書いている。「記録なし」は、その記録にその値が無いこと。

### `production`、1 コミット以上

| | 第 1 回 | 第 2 回 | 8 | 9 | 11(`contain`) | 12 | 最後 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 正解 refactored / preserve | 755 / 28 | 760 / 30 | 760 / 30 | 778 / 30 | 778 / 30 | 778 / 30 | 778 / 30 |
| 基準率(2,583 個のうち) | 0.292 | 0.294 | 0.294 | 0.301 | 0.301 | 0.301 | 0.301 |
| `unit_churn` | 0.9 / 0.82 / 0.74 / 0.098 | 0.9 / 0.76 / 0.71 / 0.093 | 0.90 / 0.76 / 0.71 / 0.09 | 0.90 / 0.76 / 0.72 / 0.09 | 0.9 / 0.76 / 0.72 / 0.093 | 0.9 / 0.76 / 0.71 / 0.091 | 0.9 / 0.76 / 0.71 / 0.091 |
| `fan_in_x_commits` | 0.7 / 0.64 / 0.58 / 0.077 | 0.8 / 0.52 / 0.43 / 0.057 | 0.80 / 0.52 / 0.43 / 0.06 | 0.90 / 0.50 / 0.48 / 0.06 | 0.9 / 0.5 / 0.48 / 0.062 | 0.9 / 0.48 / 0.47 / 0.06 | 0.9 / 0.48 / 0.47 / 0.06 |
| `fan_in` | 0.2 / 0.32 / 0.28 / 0.037 | 0.2 / 0.32 / 0.29 / 0.038 | 0.20 / 0.32 / 0.29 / 0.04 | 0.40 / 0.36 / 0.32 / 0.04 | 0.4 / 0.36 / 0.32 / 0.041 | 0.4 / 0.36 / 0.32 / 0.041 | 0.4 / 0.36 / 0.32 / 0.041 |
| `clone_one_sided` | 0.9 / 0.74 / 0.65 / 0.086 | 0.8 / 0.76 / 0.68 / 0.089 | 0.80 / 0.74 / 0.65 / 0.09 | 0.80 / 0.72 / 0.66 / 0.09 | 0.8 / 0.72 / 0.66 / 0.085 | 0.8 / 0.76 / 0.66 / 0.085 | 0.8 / 0.76 / 0.66 / 0.085 |
| `cochange_without_reference` | 0.9 / 0.30 / 0.49 / 0.065 | 0.4 / 0.36 / 0.41 / 0.054 | 0.60 / 0.46 / 0.49 / 0.06 | 0.60 / 0.44 / 0.43 / 0.06 | 0.6 / 0.44 / 0.43 / 0.055 | 0.7 / 0.44 / 0.4 / 0.051 | 0.7 / 0.44 / 0.40 / 0.051 |
| `loc` | 0.8 / 0.78 / 0.75 / 0.099 | 0.8 / 0.78 / 0.75 / 0.099 | 0.80 / 0.78 / 0.75 / 0.10 | 0.80 / 0.78 / 0.75 / 0.10 | 0.8 / 0.78 / 0.75 / 0.096 | 0.8 / 0.78 / 0.75 / 0.096 | 0.8 / 0.78 / 0.75 / 0.096 |
| `module_cycles`(集合) | 466 / 1,125 | 471 / 1,125 | 232 / 580 | 240 / 580 | 記録なし | 記録なし | 240 / 580 |

### `production`、3 コミット以上

| | 第 1 回 | 第 2 回 | 8 | 9 | 11(`contain`) | 12 | 最後 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 正解 refactored | 67 | 89 | 89 | 92 | 92 | 92 | 92 |
| 基準率 | 0.026 | 0.034 | 0.034 | 0.036 | 0.036 | 0.036 | 0.036 |
| `unit_churn` | 0.3 / 0.32 / 0.27 / 0.403 | 0.5 / 0.34 / 0.30 / 0.337 | 0.50 / 0.34 / 0.30 / 0.34 | 0.50 / 0.32 / 0.31 / 0.34 | 0.5 / 0.32 / 0.31 / 0.337 | 0.5 / 0.32 / 0.32 / 0.348 | 0.5 / 0.32 / 0.32 / 0.348 |
| `fan_in_x_commits` | 0.4 / 0.22 / 0.18 / 0.269 | 0.6 / 0.24 / 0.16 / 0.180 | 0.60 / 0.24 / 0.16 / 0.18 | 0.60 / 0.24 / 0.15 / 0.16 | 0.6 / 0.24 / 0.15 / 0.163 | 0.6 / 0.24 / 0.15 / 0.163 | 0.6 / 0.24 / 0.15 / 0.163 |
| `fan_in` | 0.1 / 0.06 / 0.04 / 0.060 | 0.2 / 0.08 / 0.05 / 0.056 | 0.20 / 0.08 / 0.05 / 0.06 | 0.20 / 0.08 / 0.06 / 0.07 | 記録なし | 記録なし | 0.2 / 0.08 / 0.06 / 0.065 |
| `clone_one_sided` | 0.5 / 0.22 / 0.17 / 0.254 | 0.4 / 0.18 / 0.18 / 0.202 | 0.30 / 0.18 / 0.19 / 0.21 | 0.30 / 0.20 / 0.19 / 0.21 | 0.3 / 0.2 / 0.19 / 0.207 | 0.3 / 0.22 / 0.18 / 0.196 | 0.3 / 0.22 / 0.18 / 0.196 |
| `cochange_without_reference` | 0.0 / 0.06 / 0.12 / 0.179 | 0.0 / 0.02 / 0.05 / 0.056 | 0.00 / 0.12 / 0.14 / 0.16 | 0.00 / 0.06 / 0.11 / 0.12 | 0.0 / 0.06 / 0.11 / 0.12 | 0.0 / 0.06 / 0.11 / 0.12 | 0.0 / 0.06 / 0.11 / 0.12 |
| `loc` | 0.4 / 0.30 / 0.23 / 0.343 | 0.5 / 0.36 / 0.29 / 0.326 | 0.50 / 0.36 / 0.29 / 0.33 | 0.50 / 0.36 / 0.29 / 0.32 | 記録なし | 記録なし | 0.5 / 0.36 / 0.29 / 0.315 |
| `module_cycles`(集合) | 42 / 1,125 | 57 / 1,125 | 40 / 580 | 42 / 580 | 記録なし | 記録なし | 42 / 580 |

### 順位の付く同一性の数

両方の範囲で同じである。`unit_churn` と `loc` は 2,583、`fan_in_x_commits` と `fan_in` は 1,307 で、どの列でも変わらない。

| | 第 1 回 | 第 2 回 | 8 | 9 | 11(`contain`) | 12 | 最後 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `clone_one_sided` | 154 | 154 | 154 | 154 | 154(`overlap` 379) | 154 | 154 |
| `cochange_without_reference` | 188 | 493 | 256 | 268 | 268 | 292 | 292 |

## 3. 動き

- **最後の列と 12 の列の差は、どの欄も 0 だった。** 3 コミット以上の `fan_in` と `loc` は 12 と 11 の記録に無く、9 の列と比べると、小数 2 桁に丸めた値で同じだった(`fan_in` の recall 0.065 と 0.07、`loc` の 0.315 と 0.32)。`module_cycles` も 9 の列と同じ(240、42)だった。
- 正解の数は、第 1 回 → 第 2 回で 755 → 760、67 → 89(系譜の正解)、8 → 9 で 760 → 778、89 → 92(前置きの doc と属性の帰属)と動き、9 以後は変わっていない。preserve は第 2 回から 30 である。
- 第 1 回から最後の列まで、1 コミット以上の P@100 は、`unit_churn` 0.74 → 0.71、`fan_in_x_commits` 0.58 → 0.47、`fan_in` 0.28 → 0.32、`clone_one_sided` 0.65 → 0.66、`cochange_without_reference` 0.49 → 0.40、`loc` 0.75 → 0.75 と動いた。3 コミット以上の P@100 は、`unit_churn` 0.27 → 0.32、`fan_in_x_commits` 0.18 → 0.15、`fan_in` 0.04 → 0.06、`clone_one_sided` 0.17 → 0.18、`cochange_without_reference` 0.12 → 0.11、`loc` 0.23 → 0.29 である。第 1 回と第 2 回の間で正解の作り方が変わっている(名前 → 名前か系譜)ので、この差は正解と信号の両方の変化を含む。

# 第 2 部 前後比較の評価

## 4. スイープの標本

### 4.1 手順

`retro/end`(46c37290、全履歴のキャッシュ)で、`compare_before=e223bc20`、`scip_before` を e223bc20 の索引にして `compare_units` を CSV で出した。数は前後比較の記録 3.3 節の表と同じだった(`kept` 6,282、うちコミット 1 以上 996。`renamed` 24 + `ambiguous` 7。`moved` 335 + `ambiguous` 55。`added` 1,276。`removed` 242。`missing` 0)。

標本は、出力の CSV の行の順に番号を付け、DuckDB で `SET threads = 1; SELECT setseed(0.42);` のあと、その順に `random()` を 1 つずつ振り、群ごとに小さい順に取った。群は `moved` の `ambiguous` でないもの 10、`moved` の `ambiguous` 10、`renamed` 10(31 行すべてではなく 10 行にした)、`added` 5、`removed` 5 である。各行について、`git show e223bc20:<path>` と `46c37290:<path>` の該当の箇所、`git grep` による両側の定義の位置、変更したコミットの `git show` を読み、範囲の中の `unit_successions`(`same_name` 以外)と `unit_at_before` の行を引いた。以下の「行」は CSV の行番号である。

### 4.2 結果の表

| 群 | 行 | 正しい | 誤り | 判断できない |
| --- | ---: | ---: | ---: | ---: |
| `moved`(`ambiguous` でない) | 10 | 10 | 0 | 0 |
| `moved`(`ambiguous`) | 10 | 4 | 6 | 0 |
| `renamed` | 10 | 10(うち 7 は前の名前が HEAD に残る) | 0 | 0 |
| `added` | 5 | 4 | 1 | 0 |
| `removed` | 5 | 3 | 1 | 1 |
| 計 | 40 | 31 | 8 | 1 |

「正しい」は、状態と相手の同一性が、`git diff` で見た変更と `compare_units` の文書の規則の両方に合うことである。

### 4.3 群ごとの読み

**`moved`(`ambiguous` でない)。** 10 行とも、前のファイルからその関数が消え、相手のファイルに同じ名前で現れていた。リンクは `content_match` 9 行(類似度 0.5〜1.0)、`file_rename` 1 行。例:6668 `WorkspaceShell.prev_tab`(`src/workspace/render.rs` → `navigation.rs`、36ea6022、類似度 0.5。可視性が `pub(super)` になっただけ)、6606 `TerminalView.cell_at`(`src/terminal/mod.rs` → `pointer.rs`、1b15a6a5)、6444 `KnowledgeStatus.as_str`(`knowledge.rs` → `knowledge/document.rs`、5451c800)。6697 の `ToolCallMark` の impl(fe4f704d のファイルの改名)は、`loc` が 14 → 15 なのにコミット 0 だった。中の method だけが変わり、innermost の帰属では impl の自分の行が変わっていないためである。

**`moved`(`ambiguous`)。** 正しかった 4 行は、類似度が最も高い組(6378 `toggle_mode` → `toggle_mode` 0.87、6326 `tests.approval_for_denials` → `approval_for_denials` 1.0、6563 `drops_empty_entries_between_commas` 1.0)と、本当の併合(6552:`src/runtime/agent.rs` と `terminal.rs` の `establish_for_drain` 2 つが、87fca482 で `src/runtime/common.rs` の総称の 1 つになった。類似度はどちらも 0.5)だった。誤りの 6 行は、どれも同じコミットで同じファイルから一緒に移された兄弟の関数への、類似度 0.5〜0.6 の `content_match` の組で、その前の同一性にも後の同一性にも、別に類似度 0.8〜1.0 の正しい組の行がある。

| 行 | 出力の組(類似度) | 正しい組(類似度) | コミット |
| --- | --- | --- | --- |
| 6335 | `tests.fold_bash_completion_reports_running_when_no_approval_is_pending` → `fold_finished_bash_result_keeps_an_occurrence_the_result_already_carries`(0.6) | 同名どうし(1.0) | 345662b3 |
| 6332 | 同じ前の同一性 → `approval_for_denials`(0.58) | 同上 | 345662b3 |
| 6380 | `mode_move` → `mode_cancel`(0.58) | `mode_move` → `mode_move`(0.8) | 36ea6022 |
| 6383 | `mode_commit` → `toggle_mode`(0.5) | `mode_commit` → `mode_commit`(0.82) | 36ea6022 |
| 6610 | `TerminalView.keys_as_escape_codes` → `pointer.rs` の `mouse_reporting`(0.56) | → `keyboard.rs` の `keys_as_escape_codes`(1.0) | 1b15a6a5 |
| 6431 | `render_stop_button` → `receipts.rs` の `render_continue_button`(0.55) | `render_stop_button` → `render_stop_button`(0.88) | fe4f704d |

状態の `moved` そのものは、前の unit が別のファイルに移った点で正しいが、その相手の行は存在しない対応である。`ambiguous` はこれらの行に正しく立っている。

**`renamed`。** 10 行とも、文書の規則(体が移った先を継ぐ)どおりで、相手は前の本体の多くを持つ unit だった。そのうち 3 行は、前の名前が HEAD に無い普通の改名である(6288 `tools/memory.rs` の `execute_auto` → `execute`、5ce74c00、0.63。6303 と 6305 はテストの改名)。残りの 7 行は `body_move` で、前の名前の unit が HEAD にも、より小さな別の unit として残っている。

| 行 | 前 → 後(コミット、類似度) | HEAD に残った前の名前 |
| --- | --- | --- |
| 6287 | `tools/fs/read.rs` `execute` → `render_content`(f120f173、0.65) | パスの解決と I/O の半分 |
| 6293 | `csi_u_text_key` → `finish_csi_u`(fda03325、0.53、`ambiguous`) | 前置きを組み、`finish_csi_u` を呼ぶ |
| 6294 | `cargo_test_artifact` → `find_cargo_test_artifact`(689e2ae8、0.79) | `OnceLock` のキャッシュで包む |
| 6300、6301 | `cancel_palette`、`cancel_session_manager` → `restore_focus_after_modal_cancel`(eac12328、0.6。4 つの `cancel_*` が 1 つに併合、`ambiguous`) | 自分の後始末と、共通の補助関数の呼び出し |
| 6309 | `classify_call` → `classify_input`(cf9a2daa、0.79) | `#[cfg(test)]` の包み |
| 6310 | `providers/rig/tests.rs` `recv` → `recv_provider`(b9a2f2ae、0.6、`ambiguous`) | 型の違う別の `recv` |

残った方は `added` の行として別に出る。`renamed` の行だけを読むと、6287 と 6300・6301 は抽出、ほかは改名と包みの新設である変更が、改名に見える。6310 は `crates/horizon-agent/src/tests.rs` の `recv_event` からも同じ `recv_provider` への組(0.6)を持ち、`ambiguous` が立つ。その組(別ファイルの同じ本文の関数)は誤りで、標本の 6310 の行(`recv` → `recv_provider`)は正しい。

**`added`。** 4 行は範囲の中で新しく書かれた unit だった(`writer.rs` のテスト、`tools/input/mod.rs` の `PreparedCall<'a>.new`、`tools/input/bounded.rs` の `Object`、`session_loop.rs` の `mod channels;`)。誤りは 7363 の `providers/rig/clearing.rs` `cleared_occurrence_ids_from_events` で、46c37290 で `cleared_call_ids_from_events` を改名し、40 行の網羅的な `match` を `_ => None` の 10 行にしたものである(`ToolCallId` → `OccurrenceId` も同時)。行の集合の類似度が 0.5 に届かずリンクが無いので、前の方は `removed`(8007 行)、後の方は `added` になった。正しくは `renamed` である。

**`removed`。** 正しかった 3 行は、`RegistryGuard.new`(74311e12 で `Registration::attach_process` の別の設計に置き換わった)、`execute_tier1_fs`(901edb40 で大きな計画の関数の中に書き込まれた)、`Persistence.append_events`(cc30ff0e で消された)である。誤りは 8111 の `knowledge.rs` の型 `ParsedKnowledgeMd` で、5451c800 で `knowledge/document.rs` に移り、すべてのフィールドに `pub(super)` が付いた。行がほとんどすべて変わったので類似度が届かず、HEAD の側は `added`(7363 行とは別の、`document.rs` の `ParsedKnowledgeMd` の行)になった。正しくは `moved` である。判断できなかったのは 8115 の `tools/bash/mod.rs` `completion_result_mut` で、beaa9d6b で `tools/bash/job.rs` に移り(`content_match` 1.0)、8618f584 で `BashCompletion` の method `result_mut`(腕を `Self::` で書き直したもの)に置き換わり、そこでリンクが切れた。HEAD には総称の `ToolCompletion<R>.result_mut`(`tools/completion.rs`、`added`、コミット 2、移動のコミット 1)がある。書き直された method を同じ unit とみなすかは、`git diff` からは決まらない。

### 4.4 誤りを作ったもの

- **`moved` の `ambiguous` の 6 行**:同じコミットで一緒に移された似た関数の間に、しきい値 0.5 ちょうど付近の `content_match` が複数できる。`unit_successions` は、閾値以上のすべての候補をリンクにする規則で(README の Similarity の項)、`compare_units` はそれをすべて組の行にする。
- **`added` の 1 行と `removed` の 1 行**:リンクが無いこと。どちらも、本文の行のほとんどが書き換わった移動・改名(可視性の一括の付与、網羅的な `match` の短縮)で、行の集合の類似度が閾値に届かなかった。
- 判断できない 1 行は、関数から method への書き直しでリンクが切れたものである。

### 4.5 `ambiguous` の標本以外の数

スイープの `ambiguous` は 62 行(`moved` 55、`renamed` 7)である。標本の 10 行の割合(6 / 10 が誤った組)を全体に当てはめることはしていない。

### 4.6 `kept` で `loc` が大きく変わった行

`loc_before` と `loc_after` の差が `loc_before` の 50 % より大きい `kept` の行は 123 行だった(増えた 56、減った 67)。種類は function 59、method 32、type 11、impl 11、module 10 で、コミット 0 のものが 15 行(impl 10、module 5)ある。コミット 0 のものは、中の unit だけが変わった入れ物である(innermost の帰属)。

同じ `setseed(0.42)` で 3 行を取って読んだ。

- `crates/horizon-agent/src/tools/web/fetch.rs` の `tests` モジュール(55 → 151、コミット 1):2460f43a でテストが足された。`git diff --stat` ではこのファイルは +208 / −124 である。
- `src/terminal/mod.rs` の `impl TerminalView`(356 → 43、コミット 1):範囲の中の 4 コミット(6473dcb5、92dc0b40、fda03325、1b15a6a5)が `mod.rs` を変え、method の多くが `keyboard.rs`、`pointer.rs` などの別の `impl TerminalView` に移った。移った method は、それぞれ `moved` の行(6606 の `cell_at` など)になる。impl のコミット 1 は、impl の自分の行に触れたコミットの数である。
- `crates/horizon-agentd/tests/e2e.rs` `corrupt_event_log_lines_are_reported_to_the_client_once_per_connection`(20 → 49、コミット 1):0d6186ec でテストが延び、壊れた末尾の行の扱いと、その後の最初の記録を確かめる部分が足された。

## 5. 決定モデルのブランチ

### 5.1 範囲と作者の説明

`2e6a5f32..2a49dc89` は 13 コミット(マージ 4、マージでないもの 9)、53 ファイル、+3,701 / −599 である。2e6a5f32 は 2a49dc89 の祖先で(`git merge-base` が 2e6a5f32 自身)、`compare_commit` はこれを受け付け(位置 1,558)、`snapshot_missing` は 3 つの問いのすべてで 0 だった。範囲のマージでないコミットはすべて 2e6a5f32 より後の日付で、位置は 1,559〜1,571 である。53 ファイルのうち `.rs` は 43 で、残りの 10(`docs/` の 6、`AGENTS.md`、`config.example.toml`、`.config/nextest.toml`、`SKILL.md`)は層の範囲の外にある。

作者の説明(`git log 2e6a5f32..2a49dc89` の題と本文、`docs/agent-decision-model-design.md`)の要点:a5b11f64 が `decision_model` の組と System One の HTTP の判定のクライアント(`horizon_agent::decision`)を足し、f6c7c4b5 がハーネス側のターン継続の判定(`next_speaker` の choice、閾値 0.8、1 ターン 3 回まで、`Event::ContinuationVerdict`)を足し、cad2cfab が承認の判定を 1 つの `noul` の問い(`ask_human`、閾値 0.2)にして 2 段階の chat の判定とその rig のクライアントと解析器を消し、ff10f1ef が決定モデルの無いときに `small_model` の chat の判定へ戻る道を復活させ(`judge_backend`)、db0d334f が main(MoA の削除)を取り込んで 5 つの衝突を解き、2a49dc89 が Git の操作の判定に sandbox と同じ付与を渡し、判定の方針を効果に基づく 1 つの文言にした(`metadata_grants` の補助関数)。

範囲のマージでないコミットの `unit_changes` は、a5b11f64 68 行、f6c7c4b5 68 行、cad2cfab 144 行(うち `removed` 81)、ff10f1ef 145 行(うち `added` 103)、2a49dc89 10 行で、文書だけのコミット 4 つには行が無い。

### 5.2 数

| `compare_units` | 行 | コミット 1 以上 |
| --- | ---: | ---: |
| `kept` | 8,668 | 125 |
| `kept`、`ambiguous` | 1 | 1 |
| `renamed` | 8 | 5 |
| `moved`、`ambiguous` | 1 | 1 |
| `added` | 174 | 174 |
| `removed` | 19 | 0 |
| `missing` | 0 | — |

`compare_graph` はモジュールの辺の `added` 66、`removed` 3、`kept` 3,451(うち unit の辺の数が変わったもの 43:増えた 38、減った 5)、`compare_cycles` は `kept` 25、`appeared` 0、`disappeared` 0、`ambiguous` 0 だった。出力の全体は作業ツリーの `target/code-db/out/dm-compare_{units,graph,cycles}.csv` に置いた(コミットしていない)。

`kept` の行について、両端の本文(前置きの doc と属性を含む、空白を詰めた行)を比べた。コミット 0 で本文が同じもの 8,520、コミット 0 で本文が違うもの 23、コミット 1 以上で本文が違うもの 71、コミット 1 以上で本文が同じもの 55 である。

### 5.3 (1)変更をどこまで表しているか

**足された unit。** `added` 174 行は、`judge/` 76(うちテスト 55)、`continuation.rs` と `continuation/tests.rs` 41(27)、`decision/` 38(16)、`horizon-config` 9(6)、その他 10(4)だった(テストは、パスが `tests.rs`・`tests/`・`test_support.rs` か、限定名が `tests` で始まるもの)。作者の説明の 3 つの新しい部品が、この行に出ている。

- `crates/horizon-agent/src/decision/client.rs` `HttpDecisionClient.new`(6 行、fan-in 2)、`decision/wire.rs` の型 `DecisionRequest`(fan-in 6)、`decision/config.rs` の `DecisionConfig`(fan-in 7)。
- `providers/rig/session/continuation.rs` `SessionLoopState.handle_continuation_check`(122 行)、`resolve_continuation_threshold`、`contract.rs` の型 `ContinuationVerdict`(fan-in 5)。
- `judge/request.rs` `decision_request`、`trim_user_messages`、`tools/bash/git.rs` `metadata_grants`(fan-in 4、2a49dc89 の共有の補助関数)。

**中が変わった unit。** コミット 1 以上で本文が違う `kept` は 71 行で、そのうちテストでないものは 46 行である。継続の判定を差し込んだ箇所(`providers/rig/session/turn.rs` `SessionLoopState.run_turn` 57 → 60、コミット 1)、判定の起動(`judge/handle.rs` `JudgeHandle.new` 15 → 34、コミット 3)、2a49dc89 の抽出の元(`tools/bash/git.rs` `validated_metadata_grants` 27 → 15、コミット 1)が、ここに出ている。

**改名と移動。** `renamed` 8 行は、`judge/mod.rs` の `run_judge` → `run_decision_judge`(ff10f1ef の `body_move`、0.76)、`judge/handle.rs` の `JudgeHandle.for_test` → `for_test_with_backend`(ff10f1ef、0.59)、テストの `PendingClient` の `ModelClient` → `DecisionClient` の 5 行(cad2cfab、`content_match` 2 と `inherited` 3)、`horizon-agentd/src/providers.rs` のテストの改名(a5b11f64)である。`moved` と `kept` の `ambiguous` の 2 行は、`judge/prompt.rs` の `tests.input` が cad2cfab で `judge/request.rs` に移り(`content_match` 0.67)、ff10f1ef で `prompt.rs` に同じ名前で戻ったことによる分割である(`judge/chat.rs` の 3 つ目の `tests.input` は `added`)。

**辺。** 足された辺 66 本は、出る側で分けると `judge/` 27(テストのモジュールから 15)、`continuation.rs` と `continuation/tests.rs` 25(12)、`decision/` 6(4)、`continuation.rs` へ入る辺 4(`interaction.rs`、`state.rs`、`tool_results.rs`、`turn.rs` から)、その他 4 である。`decision/` へ入る辺は 24 本ある。例:`judge/mod.rs` → `decision/client.rs`(unit の辺 3)、`providers/rig/session/continuation.rs` → `decision/wire.rs`(4)、`horizon-agentd/src/providers.rs` → `horizon-agent/src/decision/config.rs`(2、crate をまたぐ 3 本の 1 つ)。消えた辺 3 本(`judge/mod.rs` → `parse.rs`、`judge/mod.rs` → `prompt.rs`、`judge/mod.rs::tests` → `parse.rs`)は、chat の判定が `judge/chat.rs` に移ったことに対応し、足された `judge/chat.rs` → `parse.rs`・`prompt.rs`・`client.rs` の辺と対になる。

**循環。** 25 個のうち 24 個はメンバーも unit の辺の数も同じだった。1 つは、`providers/rig/session/input.rs` と `state.rs` の循環に `continuation.rs` が加わり、2 → 3 モジュール、unit の辺 17 → 28 になった。状態は `kept` で、名前(最小のメンバー)が `input.rs` から `continuation.rs` に変わった。

### 5.4 (2)雑音や誤解を招く行

| 所見 | 数 | 例 |
| --- | --- | --- |
| 消して戻したファイルの、本文が同じ `kept` にコミット 2 が付く | 55(`parse.rs` 26、`client.rs` 15、`prompt.rs` 11、`judge/mod.rs` の `mod client;`・`mod parse;`・`mod prompt;` 3) | `judge/parse.rs` `parse_stage2_json`(18 → 18、コミット 2)、`judge/client.rs` `RigModelClient.new`(5 → 5、2)、`judge/prompt.rs` `tests.supervisor_grants_are_rendered_in_a_separate_trusted_region`(19 → 19、2) |
| コミットをまたぐ移動が `removed` と `added` に分かれる | `judge/mod.rs` の `removed` 17 行のうち 13 行に、同じ限定名・種類の `added` が `judge/chat.rs` にある(cad2cfab で消え、ff10f1ef で戻った。リンクは同じコミットの中だけ) | `tests.stage1_unparseable_response_goes_to_stage2`(32 → 32)、`tests.injected_instructions_in_args_never_flip_the_stage1_verdict`(26 → 26)、`tests.ModelClient for ScriptedClient.complete`(13 → 13) |
| 2 段階の書き換えで、相手が両端の読みと違う | 1(`run_judge`)と、同じ名前の新しい `run_judge` の `added` 1 | 前の `run_judge`(68 行、chat の判定)→ `run_decision_judge`(39 行、`renamed`)。行の集合の類似度は前の `run_judge` と `run_decision_judge` で 0.12、`judge/chat.rs` `run_chat_judge`(78 行、`added`)で 0.71。cad2cfab が `run_judge` をその場で書き換え(`same_name`)、ff10f1ef が書き換えた本体を `run_decision_judge` に移し(`body_move`)、新しい 8 行の振り分けの `run_judge` を置いた |
| 2 段階の改名の途中でリンクが切れる | 2 組 | `judge/handle.rs` `tests.no_small_model_connection_leaves_human_approval_active`(5 行):cad2cfab で `tests.no_decision_model_connection_leaves_human_approval_active` に `content_match`(0.67)、ff10f1ef で `tests.no_connection_leaves_human_approval_active`(5 行、`added`)になったところで切れ、`removed`。`tests.the_judge_runs_the_small_model_pairs_model`(12 行、`removed`)と `tests.the_judge_runs_the_decision_model_pairs_model`(8 行、`added`、コミット 2) |
| テストの行が多い | `kept` でコミット 0 かつ本文が同じもの以外の 328 行のうち 192 がテスト(`added` 108 / 174、`removed` 19 / 19、`renamed` 6 / 8、`moved` 1 / 1、コミット 1 以上の `kept` 58 / 126)。足された辺 66 本のうち 33 本はテストのモジュールから出る | `continuation/tests.rs` `serve_replies`(49 行)、`decision/client.rs` `tests.serve_once`(36 行)、`judge/request.rs::tests` → `horizon-sandbox/src/policy.rs` の辺 |
| unit の辺 1 本だけの足された辺 | 17 / 66 | `judge/chat.rs` → `judge/prompt.rs`、`providers/rig/mapping/tests.rs` → `persistence/projection/duckdb/records.rs`、`providers/rig/session/interaction.rs` → `continuation.rs` |
| 移動のコミットだけで数えたコミット | `move_commits` 1 の行が 6、そのうち 4 行はコミットがその 1 つだけ | テストの `DecisionClient for PendingClient`(コミット 1、移動 1)、`DecisionClient for PendingClient.decide`(1、1)、`judge/request.rs` `tests.input`(`moved`、1、1) |

`ambiguous` は 2 行(上の `tests.input`)だけで、どちらも実際の分割だった。このブランチでは多く立ってはいない。範囲のマージ 4 つは `include_merges` が偽なので、`commits` にマージから来た数は無い(次の節の、マージだけが変えた行を参照)。

### 5.5 (3)レビューに要るのに出ていないもの

| 所見 | 数 | 例 |
| --- | --- | --- |
| `const` と `static` が unit でない | HEAD の `judge/`、`decision/`、`continuation.rs` に 37 個。`compare_units` に種類 `const` の行は無い(種類は function、method、type、impl、module、macro だけ) | 2a49dc89 が書き換えた `judge/request.rs` の `GIT_INSTRUCTIONS` と `BASE_INSTRUCTIONS`(この 2 つの hunk は unit の行を作らず、このコミットの `unit_changes` 10 行はテスト、`metadata_grants`、`validated_metadata_grants`、`user_content` などだけ)。`continuation.rs` の `DEFAULT_CONTINUATION_THRESHOLD`(0.8)と `MAX_CONTINUATIONS_PER_TURN`(3)。`judge/handle.rs` の `DEFAULT_ASK_THRESHOLD`(0.2) |
| 本文が変わったかの印が無い | コミット 1 以上の `kept` 126 行のうち 55 行は本文が同じで、71 行は違う。`commits` の列では区別できない | 5.4 節の 1 行目の例と、`SessionLoopState.run_turn`(57 → 60、コミット 1) |
| コミット 0 で本文が変わった `kept` | 23(impl 15、module 7、function 1) | `judge/handle.rs` `impl JudgeHandle`(155 → 206、コミット 0。中の method の変更は innermost で method に付く)、`horizon-config/src/lib.rs` `impl RawConfig`(67 → 86、0)、`horizon-agentd/src/session/connection.rs` `tests.two_provider_state`(53 → 54、0) |
| マージだけが変えた行が数えられない | 上の `tests.two_provider_state` の 1 行。db0d334f の衝突の解決(`git show --cc`)は `.rs` の 5 ファイル(`config.rs`、`continuation/tests.rs`、`session/state.rs`、`hub/mapping.rs`、`horizon-agentd/src/providers.rs`)に hunk を持つ | 横の枝の a5b11f64 は、その時点の名前 `tests.two_provider_state_with_moa` を変えた(`decision: None` の 1 行)。その名前はどちらの端にも無く、HEAD の `tests.two_provider_state` には db0d334f(マージ)を通って入った |
| どのコミットが変えたかが出ない | `commits` は数だけで、コミットの一覧も題も無い | `JudgeHandle.new`(コミット 3)、`prompt.rs` `user_content`(88 → 64、3)、`tests.git_metadata_operation_adds_a_git_guidance_region`(20 → 37、3) |
| 親と子のモジュールの相互の参照は辺に出るが循環に出ない | 2 組 | `judge/mod.rs` → `judge/chat.rs`(unit の辺 1)と逆向き(6)、`judge/mod.rs` → `judge/request.rs`(1)と逆向き(3)。`cycle_edges` が入れ子のモジュールの辺を除くので(8 個目の増分)、`compare_cycles` では `judge/` に変化が無い |
| 辺の重みの変化は `kept` | unit の辺の数が変わった `kept` 43 本 | `judge/mod.rs::tests` → `judge/client.rs`(23 → 3)、`horizon-config/src/tests.rs` → `lib.rs`(108 → 125)、`judge/handle.rs::tests` → `judge/handle.rs`(4 → 18) |
| Rust 以外のファイル | 53 ファイルのうち 10 | 設計の文書 `docs/agent-decision-model-design.md`(+183)、`config.example.toml`、`AGENTS.md` |

## 6. 記録と食い違った点、気づいた点

- 記録と食い違う数は無かった。最後の列は継承の記録の「後」と、スイープの `compare_units` の数と範囲の変更の数は前後比較の記録 3.3 節と、それぞれ一致した。
- 問いの不具合とみなしたものは無い。4 節と 5 節の誤りと食い違いは、どれも README と設計の文書が書いている規則か限界(閾値以上のすべての候補をリンクにすること、同じコミットの中のリンクだけであること、行の集合の類似度、体を追う規則、innermost の帰属、`include_merges`、入れ子のモジュールの辺を循環から除くこと)から出ていた。`const` と `static` が unit でないことは、`lang/rust.sql` の unit の種類による。
- 依頼文は「decision-model introduction」のブランチの範囲を 2e6a5f32 → 2a49dc89 としていた。2e6a5f32 は main の側のコミット(MoA の削除)で、ブランチの分岐点 969511b4 はその親である。範囲の 13 コミットはすべて 2e6a5f32 より後の日付で、`commit_order` の位置と祖先の関係は食い違わなかった。

## 7. 限界

- 後ろ向きの表は `production` の範囲だけで、`all` は扱っていない。8 と 9 の列は記録の丸め(小数 2 桁)のままである。11 の列は継承の記録の「前」の値で、クローンの側の記録の既定(`overlap`)の値ではない。
- スイープの標本は 40 行と `kept` の 3 行で、`renamed` は 31 行のうち 10 行である。誤りの割合を全体に広げてはいない。判定は 1 人の読みで、`moved` の `ambiguous` の正誤は、類似度の高い組を正しいとする読み方による。
- ブランチの評価は 1 本で、テストの判定は名前とパスによる近似である。本文の比較は、前置きの doc と属性を含む行を空白を詰めて比べたもので、同じ同一性を持つ複数の unit(2 つの `impl Foo`)は行の順に連ねた。
- `kept` でコミット 0 かつ本文が同じ 8,520 行が出力の大半を占めることは数えたが、読む側の手間は測っていない。

## 8. コマンドと時間

```sh
nice -n 19 duckdb-ext/build.sh
uv run --locked --script scripts/refactor-audit/run.py setup
git clone -q --no-checkout . target/code-db/retro/base   # その後 checkout e3c619a0
git clone -q --no-checkout . target/code-db/retro/end    # その後 checkout 46c37290
git clone -q --no-checkout . target/code-db/dm/head      # 2e6a5f32 で索引を作って取り出し、2a49dc89 で作り直す
R=$PWD/target/code-db/retro; DM=$PWD/target/code-db/dm/head
duckdb-ext/sql/run.sh index target/code-db/retro/base
duckdb-ext/sql/jscpd.sh $R/end $R/end/target/code-db/jscpd src crates
# 第 1 部(後ろ向き 2 の 9 節と同じ)
duckdb-ext/sql/run.sh sql $R/end duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 retro_changes=$R/changes.parquet
duckdb-ext/sql/run.sh sql $R/end duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 retro_reviews=… \
    retro_record_preserves=… retro_lineage_changes=… retro_lineage_preserves=…
duckdb-ext/sql/run.sh sql $R/base duckdb-ext/sql/retro/signals.sql retro_changes=… retro_reviews=… \
    retro_record_preserves=… retro_record_targets=… retro_lineage_changes=… retro_lineage_preserves=… \
    retro_ranks_out=… retro_scope=production retro_min_commits=1   # と 3
# 第 2 部
duckdb-ext/sql/run.sh query $R/end compare_units compare_before=e223bc20 scip_before=…/e223bc20.scip FORMAT=csv
duckdb-ext/sql/run.sh query DM compare_units compare_before=2e6a5f32 scip_before=…/2e6a5f32.scip FORMAT=csv
duckdb-ext/sql/run.sh query DM compare_graph compare_before=2e6a5f32 scip_before=… FORMAT=csv
duckdb-ext/sql/run.sh query DM compare_cycles compare_before=2e6a5f32 scip_before=… FORMAT=csv
```

| 段階 | 時間 |
| --- | ---: |
| e3c619a0 の `run.sh index`(rust-analyzer scip) | 157.8 s |
| 2e6a5f32 の `run.sh index` | 153.9 s |
| 2a49dc89 の `run.sh index` | 221.2 s |
| `retro/end` の `changes.sql` と `lineage.sql`(全履歴のキャッシュの初回の構築を含む) | 29.2 s |
| スイープの `compare_units`(キャッシュから) | 4.4 s |
| ブランチの `compare_units`(全履歴のキャッシュの初回の構築を含む) | 50.3 s |
| ブランチの `compare_graph`、`compare_cycles`(キャッシュから) | 3.6 s、5.2 s |

時間は 1 回ずつの壁時計の値で、最大の RSS は取っていない。`signals.sql` の時間は取っていない。
