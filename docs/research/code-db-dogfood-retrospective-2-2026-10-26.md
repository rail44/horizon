# コードデータベースの信号を 2026 年 9 月のリファクタリングに照らした記録、第 2 回(2026-10-26)

[第 1 回の後ろ向きの確認](code-db-dogfood-retrospective-2026-10-26.md) を、7 つ目の増分で入った unit の系譜([code-db-lineage-2026-10-26.md](code-db-lineage-2026-10-26.md))のうえでやり直した。基準の木(e3c619a0)とリファクタリングの範囲(`e223bc20..46c37290`)は第 1 回と同じで、信号は系譜に基づく履歴で計算し、正解の表も系譜を通して揃え、第 1 回からの precision の変化を表にした。SQL、シェル、`check.py` だけを使い、Rust は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、オーナーは 2026-10-26 に、系譜を足したあとで「実際にdoghoodして、仕組みの品質や改善点を再評価しましょうか」と依頼し、依頼側が示した形(この後ろ向きの確認と、別の worker による前向きの読み)に「オッケーです。着手をお願いします。」と答えた。前向きの読みは別の worker が `code-db-dogfood-reading-2-2026-10-26.md` に書く。この記録はそのうち後ろ向きの半分である。

作業は worker の作業ツリーで行った。作業ツリーのブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 9a3c83e9 に合わせた(変更の無い状態で `git reset --hard 9a3c83e9`)。測定はこのリポジトリの使い捨ての複製 2 つ(`target/code-db/retro/base` を e3c619a0、`target/code-db/retro/end` を 46c37290)で行い、作業ツリーのファイルと履歴は測定に使っていない。機械は前回までと同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している(作業中の 1 分の load average は 2〜28、スワップを 10〜12 GB 使っていた)。DuckDB は CLI 2.0.0、rust-analyzer は 1.99.1、jscpd は旧監査の固定版 5.3.1 である。

## 主要な観察

- **第 1 回の表は、今の SQL でそのまま出し直せた。** 履歴の信号を名前で数える版(`retro/signals_by_name.sql`)と名前の正解で、4 つの表のすべての値と、第 1 回の順位の CSV(1,742 行)が行ごとに一致した(3 節)。
- **系譜を通しても、正解はほとんど変わらなかった。** 範囲で変更または削除された同一性 2,069 個のうち、基準の unit に名前で対応するのは 1,714 個、名前か系譜で対応するのは 1,801 個(+87)だった。正解の「refactored」は、1 コミット以上で 1,714 → 1,722 個(+8)、3 コミット以上で 182 → 222 個(+40)になった。本番の関数・メソッドでは 755 → 760 個、67 → 89 個である(2 節)。
- **preserve は、第 1 回に対応しなかった台帳の 8 件のうち 2 件(`tokenize`、`paint_terminal`)が系譜で対応した。** 同一性は 30 → 32 個で、増えた 2 個はどちらも範囲の中で変更もされていた。第 1 回の記録は台帳の対応を「18 件、対応しなかった 9 件」としているが、同じ SQL の出力では 19 件と 8 件だった(2 節)。
- **precision の変化は、信号の側の変化(A)でほぼ決まり、正解の変化(B と A の差)は 3 コミット以上でだけ目立った。** 本番・1 コミット以上の P@100 は、`unit_churn` 0.74 → 0.71、`fan_in_x_commits` 0.58 → 0.43、`cochange_without_reference` 0.49 → 0.41、`clone_one_sided` 0.65 → 0.68 で、`loc` と `fan_in` はほぼ変わらなかった(4 節)。
- **`cochange_without_reference` は、系譜で順位の付いた組が 2.4〜2.6 倍に増え、本番の P@10 が 0.9 から 0.4 に下がった。** 上位の新しい組のうち調べた 2 組は、同じファイルから同じコミットで何度も一緒に移された小さな関数どうしで、共通のコミットの多くは移動と最初の追加のコミットだった(4 節)。
- **履歴の信号の上位 100 に、範囲で変更されなかった preserve の unit が入るようになった。** `scheme_from` が `unit_churn` で 343 → 40 位、`fan_in_x_commits` で 134 → 23 位、`wait_for_exit` が `fan_in_x_commits` で 266 → 99 位である。第 1 回の「履歴の信号の上位 100 に入った preserve の unit は、すべて範囲の中で変更されてもいた」は、系譜の信号では成り立たない(5 節)。
- **基準の複製で、系譜のビューを含む更新したすべてのビューとクエリが、ライブと一致した**(`check.py refresh`、7 節)。

## 1. 組んだもの

| ファイル | 変更 |
| --- | --- |
| `retro/changes.sql` | `since` の代わりに `retro_since` を受け取れるようにした。そのときキャッシュは全履歴を持ち、`retro_since..HEAD` のコミットの変更だけを書き出す |
| `retro/lineage.sql`(新規) | 範囲の終わりの複製で、範囲の各変更と各 preserve の出どころから系譜をさかのぼり、範囲の始まりに届く同一性を Parquet に書き出す |
| `retro/signals.sql` | `retro_lineage_changes`、`retro_lineage_preserves` を与えると、正解を名前か系譜で作る。`retro_all_ranks_out` で範囲のすべての順位を書き出す。与えなければ第 1 回と同じ |
| `retro/signals_by_name.sql`(新規) | `unit_lineage_segments` を各 unit 自身の同一性だけにしてから `retro/signals.sql` を読む。履歴の信号はどれもこのビューを通してコミットを数えるので、名前で数えた第 1 回の信号になる |
| `README.md` | `retro/` の使い方と配置の表 |

`retro/changes.sql` を `since` と `retro_since` の両方で動かし、範囲の変更の行(4,771 行、84 コミット、同一性 3,415 個)が一致することを確かめた。

## 2. 正解を系譜で揃える

### 決めたこと

系譜のビュー(`unit_lineage`)は今の unit から始まるので、範囲の中で削除された unit の系譜をそのままでは持たない。そこで `retro/lineage.sql` は、`unit_lineage_segments` と同じ再帰を、今の各 unit の代わりに「範囲の各変更の同一性を、そのコミットの時点で」始点にして回す。依頼文の「範囲で変更された各 unit を、`unit_lineage` を通して基準の時点の祖先の同一性に写す」を、削除された unit も含めてこう読んだ。規則は次のとおりである。

- **範囲の終わりの複製は全履歴でキャッシュした**(`since` なし)。純粋なファイルの改名(hunk の無い改名)の `file_rename` は、旧パスの直前の変更が履歴にあるときだけ引かれるので、範囲だけの履歴では範囲の中で初めて触れられたファイルの改名を落とすためである。
- **たどるのは範囲のコミット(`git_commits(since := e223bc20)`、マージを含む 104 個)の継承の関係(`same_name` 以外)だけで、系譜を切るのも範囲の中の `body_move` だけである。** 範囲の中のリンクは 460 本、切れ目は 24 個だった。
- **始点は、範囲の `modified` と `removed` の変更(同一性 × コミット、3,086 個、マージは除く)である。** 始点自身の切れ目は、そのコミット以前のものを数える(今の unit の場合と同じで、`body_move` のコミットでの残り殻の変更は、残り殻の新しい系譜に属する)。
- **たどり着いた同一性のうち、たどり着いたリンクより前に範囲の中の切れ目が無いもの(`from_ord` が 0、つまり範囲の始まり e223bc20 まで系譜が届くもの)を、基準の候補とした。** 依頼文の「最初に現れたのが基準のコミット以前である祖先」をこう読んだ。
- **基準の木 e3c619a0 は範囲の祖先ではない**(e223bc20 はその付け直しで、違いは PR #60 の 19 ファイル。第 1 回の 1 節)。候補の同一性は、第 1 回と同じく、e3c619a0 の複製の unit に `(path, kind, qualified_name)` で対応させた。e223bc20 にあって e3c619a0 に無い PR #60 の unit は、第 1 回と同じく落ちる。
- **系譜の正解は、名前の正解との和である。** 1 つの変更は、自分の同一性と、系譜でたどり着いた基準の同一性の両方に数え、コミットは同一性ごとに重複なしで数える。残り殻(`body_move` で本体を渡した unit)の、そのコミット以後の変更は、名前では基準の同一性に数えられ、系譜では数えられないが、和なので残る。
- **preserve の出どころは、範囲の終わり(46c37290)の unit に第 1 回と同じ名前の規則で対応させ、その unit から、台帳の項目は判定のコミット(すべて範囲の中にあった)の時点で、記録の行は範囲の終わりの時点でさかのぼった。** 台帳の名前は判定の時点の名前で、記録の行には日付しか無いためである。範囲の終わりに名前のまま無い出どころは、系譜の側では対応しない(名前の側はそのまま残る)。

### 変更の対応

| | 名前だけ(第 1 回) | 名前か系譜(今回) | 差 |
| --- | ---: | ---: | ---: |
| 範囲で変更・削除された同一性(2,069 個)のうち、基準の unit に対応するもの | 1,714 | 1,801 | +87 |
| 正解の refactored、`all`・1 コミット以上 | 1,714 | 1,722 | +8 |
| 同、`all`・3 コミット以上 | 182 | 222 | +40 |
| 同、`production`・1 コミット以上 | 755 | 760 | +5 |
| 同、`production`・3 コミット以上 | 67 | 89 | +22 |

系譜だけで対応した 87 個の同一性が基準の同一性にたどり着いたリンク(鎖のうち、その基準の同一性から出ているもの)は、`content_match` 70、`file_rename` 12、`body_move` 5 だった(深さ 1 以上で届いた基準の同一性は 88 個)。そのうち 80 個は、名前でもすでに範囲で変更か削除されていた(移動の元で `removed` になっていた)。1 コミット以上の集合に新しく入った 8 個は、すべて `src/agent/view/rows.rs` の unit で(`ToolCallMark` の型と impl、`tool_call_glyph` など)、fe4f704d で `src/agent/view/transcript/rows.rs` に改名されたファイルの unit が、改名の後で変更されたものだった(リンクは `file_rename`)。名前では、改名で名前を保った unit の変更は新しいパスの同一性に記録され、旧パスの同一性には残らないので、これらは変更されなかったことになっていた。

系譜でコミットの数が増えた基準の同一性は 86 個で、そのうち 40 個が 3 コミットの閾値を越えた。増えた数の大きいものは、`crates/horizon-agentd/src/session/completion.rs` のテスト関数(`tests.fold_domain_grant_required_reissues_the_fetch_without_contacting_the_provider` が 1 → 8 など)と、`tools/bash/mod.rs` の `spawn_host`(1 → 6)、`run_job_body`(1 → 5)で、どれも範囲の中で別のファイルに移され、移った先で変更が続いたものである。

記録が名指しした 11 個のリファクタリング対象の照合は変わらなかった(1 コミット以上に 11 個、3 コミット以上に `SessionLoopState.run` の 1 個)。

### preserve の対応

| 出どころ | 件数 | 名前で対応(第 1 回の SQL) | 名前か系譜で対応 |
| --- | ---: | ---: | ---: |
| 台帳 | 27 | 19 | 21 |
| 記録の行 | 14 | 14 | 14 |
| 計 | 41 | 33 | 35 |

第 1 回の記録の表は台帳を「対応した 18、対応しなかった 9」としているが、同じ SQL の出力(`## Preserve sources mapped …`)で対応しなかった台帳の項目は 8 件で、第 1 回の表に並ぶ 8 行と同じだった。系譜の記録が「9 件と書いているが、その表に並ぶのは 8 件」と指摘したのと同じ食い違いで、対応した側の数も 18 ではなく 19 である。同一性の数(30 個、本番 28 個)は第 1 回の記録どおりである。

系譜で新しく対応したのは、系譜の記録(6 節)と同じ 2 件だった。

| 台帳の項目 | 系譜でたどり着いた基準の unit | リンク |
| --- | --- | --- |
| `tools/bash/shell.rs` `tokenize`(901edb40) | `tools/bash/git.rs` `tokenize` | f120f173 の `content_match` |
| `src/terminal/paint.rs` `paint_terminal`(9f820cce) | `src/terminal/mod.rs` `paint_terminal` | 1b15a6a5 の `content_match` |

残りの 6 件(`plan_prepared_call`、`fetch_picker`、`live_request`、`publish_tool_update`、`execute_synchronous`、`ConversationHistory.apply`)は、範囲の中で新しく書かれた関数で、系譜の深さ 0 の自分自身にしか届かず、その名前は基準の木に無い。preserve の同一性は 30 → 32 個(本番 28 → 30 個)、そのうち範囲の中で変更もされていたものは 15 → 17 個になった。増えた 2 個はどちらも移動の元で `removed` になっていた。

記録の行の `tools/catalog.rs` `definitions` は、範囲の中の cf9a2daa で本体が `build_definitions` に移った残り殻なので、範囲の終わりからさかのぼっても範囲の始まりには届かない。名前の対応(基準の `definitions`)はそのまま残る。

## 3. 3 つの組み合わせ

| 記号 | 信号の履歴 | 正解 | 実行 |
| --- | --- | --- | --- |
| R1 | 名前(`commits_by_name` と同じ数え方) | 名前 | `retro/signals_by_name.sql` |
| A | 系譜 | 名前 | `retro/signals.sql` |
| B | 系譜 | 名前か系譜 | `retro/signals.sql` と `lineage.sql` の 2 つのファイル |

信号の順位の付け方と範囲(`all`、`production`)は第 1 回の 3 節と同じである。R1 は第 1 回を今の SQL で出し直したもので、第 1 回の記録の 4 つの表のすべての値(順位の付いた数、P@10/50/100、recall@100、循環の集合)と、第 1 回の順位の CSV が行ごとに一致した。したがって、以下の R1 は第 1 回の値でもある。A と B の信号の順位は同じで、違うのは正解だけである。

## 4. precision と recall の変化

各欄は「R1 / A / B(A − R1、B − R1)」である。期待値(無作為に選んだときの precision)は正解ごとに違うので、見出しに両方を書いた。`module_cycles` は順位の無い集合で、欄は正解に入る数(割合)である。

### `all`、1 コミット以上(名前 1,714 / 6,903、期待値 0.248。系譜 1,722 / 6,903、期待値 0.249)

| 信号 | 順位の付いた数(R1 → A) | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 6,903 | 1.0 / 1.0 / 1.0 (±0, ±0) | 0.86 / 0.82 / 0.82 (-0.04, -0.04) | 0.83 / 0.79 / 0.79 (-0.04, -0.04) | 0.048 / 0.046 / 0.046 (-0.002, -0.002) |
| `loc` | 6,903 | 0.9 / 0.9 / 0.9 (±0, ±0) | 0.80 / 0.80 / 0.80 (±0, ±0) | 0.76 / 0.76 / 0.76 (±0, ±0) | 0.044 / 0.044 / 0.044 (±0, ±0) |
| `clone_one_sided` | 652 | 0.7 / 0.8 / 0.8 (+0.1, +0.1) | 0.76 / 0.76 / 0.76 (±0, ±0) | 0.69 / 0.81 / 0.81 (+0.12, +0.12) | 0.040 / 0.047 / 0.047 (+0.007, +0.007) |
| `fan_in_x_commits` | 2,107 | 0.7 / 0.9 / 0.9 (+0.2, +0.2) | 0.66 / 0.68 / 0.68 (+0.02, +0.02) | 0.55 / 0.57 / 0.57 (+0.02, +0.02) | 0.032 / 0.033 / 0.033 (+0.001, +0.001) |
| `cochange_without_reference` | 529 → 1,265 | 0.7 / 0.5 / 0.5 (-0.2, -0.2) | 0.56 / 0.46 / 0.46 (-0.10, -0.10) | 0.33 / 0.46 / 0.46 (+0.13, +0.13) | 0.019 / 0.027 / 0.027 (+0.008, +0.008) |
| `fan_in` | 2,107 | 0.3 / 0.3 / 0.3 (±0, ±0) | 0.40 / 0.40 / 0.40 (±0, ±0) | 0.34 / 0.34 / 0.34 (±0, ±0) | 0.020 / 0.020 / 0.020 (±0, ±0) |
| `module_cycles`(集合) | 1,551 | | | 594 / 594 / 601 個(0.383 / 0.383 / 0.387) | |

### `production`、1 コミット以上(名前 755 / 2,583、期待値 0.292。系譜 760 / 2,583、期待値 0.294)

| 信号 | 順位の付いた数(R1 → A) | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.9 / 0.9 / 0.9 (±0, ±0) | 0.82 / 0.76 / 0.76 (-0.06, -0.06) | 0.74 / 0.71 / 0.71 (-0.03, -0.03) | 0.098 / 0.094 / 0.093 (-0.004, -0.005) |
| `loc` | 2,583 | 0.8 / 0.8 / 0.8 (±0, ±0) | 0.78 / 0.78 / 0.78 (±0, ±0) | 0.75 / 0.75 / 0.75 (±0, ±0) | 0.099 / 0.099 / 0.099 (±0, ±0) |
| `clone_one_sided` | 154 | 0.9 / 0.8 / 0.8 (-0.1, -0.1) | 0.74 / 0.76 / 0.76 (+0.02, +0.02) | 0.65 / 0.68 / 0.68 (+0.03, +0.03) | 0.086 / 0.090 / 0.089 (+0.004, +0.003) |
| `fan_in_x_commits` | 1,307 | 0.7 / 0.8 / 0.8 (+0.1, +0.1) | 0.64 / 0.52 / 0.52 (-0.12, -0.12) | 0.58 / 0.43 / 0.43 (-0.15, -0.15) | 0.077 / 0.057 / 0.057 (-0.020, -0.020) |
| `cochange_without_reference` | 188 → 493 | 0.9 / 0.4 / 0.4 (-0.5, -0.5) | 0.30 / 0.36 / 0.36 (+0.06, +0.06) | 0.49 / 0.41 / 0.41 (-0.08, -0.08) | 0.065 / 0.054 / 0.054 (-0.011, -0.011) |
| `fan_in` | 1,307 | 0.2 / 0.2 / 0.2 (±0, ±0) | 0.32 / 0.32 / 0.32 (±0, ±0) | 0.28 / 0.29 / 0.29 (+0.01, +0.01) | 0.037 / 0.038 / 0.038 (+0.001, +0.001) |
| `module_cycles`(集合) | 1,125 | | | 466 / 466 / 471 個(0.414 / 0.414 / 0.419) | |

### `all`、3 コミット以上(名前 182 / 6,903、期待値 0.026。系譜 222 / 6,903、期待値 0.032)

| 信号 | 順位の付いた数(R1 → A) | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 6,903 | 0.6 / 0.8 / 0.8 (+0.2, +0.2) | 0.36 / 0.36 / 0.38 (±0, +0.02) | 0.36 / 0.33 / 0.35 (-0.03, -0.01) | 0.198 / 0.181 / 0.158 (-0.017, -0.040) |
| `loc` | 6,903 | 0.6 / 0.6 / 0.6 (±0, ±0) | 0.42 / 0.42 / 0.44 (±0, +0.02) | 0.36 / 0.36 / 0.38 (±0, +0.02) | 0.198 / 0.198 / 0.171 (±0, -0.027) |
| `clone_one_sided` | 652 | 0.3 / 0.4 / 0.4 (+0.1, +0.1) | 0.26 / 0.18 / 0.30 (-0.08, +0.04) | 0.21 / 0.17 / 0.24 (-0.04, +0.03) | 0.115 / 0.093 / 0.108 (-0.022, -0.007) |
| `fan_in_x_commits` | 2,107 | 0.4 / 0.4 / 0.5 (±0, +0.1) | 0.28 / 0.32 / 0.38 (+0.04, +0.10) | 0.24 / 0.23 / 0.27 (-0.01, +0.03) | 0.132 / 0.126 / 0.122 (-0.006, -0.010) |
| `cochange_without_reference` | 529 → 1,265 | 0.0 / 0.0 / 0.0 (±0, ±0) | 0.06 / 0.00 / 0.00 (-0.06, -0.06) | 0.05 / 0.03 / 0.03 (-0.02, -0.02) | 0.027 / 0.016 / 0.014 (-0.011, -0.013) |
| `fan_in` | 2,107 | 0.1 / 0.1 / 0.2 (±0, +0.1) | 0.08 / 0.08 / 0.14 (±0, +0.06) | 0.09 / 0.09 / 0.13 (±0, +0.04) | 0.049 / 0.049 / 0.059 (±0, +0.010) |
| `module_cycles`(集合) | 1,551 | | | 60 / 60 / 77 個(0.039 / 0.039 / 0.050) | |

### `production`、3 コミット以上(名前 67 / 2,583、期待値 0.026。系譜 89 / 2,583、期待値 0.034)

| 信号 | 順位の付いた数(R1 → A) | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.3 / 0.5 / 0.5 (+0.2, +0.2) | 0.32 / 0.32 / 0.34 (±0, +0.02) | 0.27 / 0.26 / 0.30 (-0.01, +0.03) | 0.403 / 0.388 / 0.337 (-0.015, -0.066) |
| `loc` | 2,583 | 0.4 / 0.4 / 0.5 (±0, +0.1) | 0.30 / 0.30 / 0.36 (±0, +0.06) | 0.23 / 0.23 / 0.29 (±0, +0.06) | 0.343 / 0.343 / 0.326 (±0, -0.017) |
| `clone_one_sided` | 154 | 0.5 / 0.4 / 0.4 (-0.1, -0.1) | 0.22 / 0.18 / 0.18 (-0.04, -0.04) | 0.17 / 0.16 / 0.18 (-0.01, +0.01) | 0.254 / 0.239 / 0.202 (-0.015, -0.052) |
| `fan_in_x_commits` | 1,307 | 0.4 / 0.5 / 0.6 (+0.1, +0.2) | 0.22 / 0.22 / 0.24 (±0, +0.02) | 0.18 / 0.15 / 0.16 (-0.03, -0.02) | 0.269 / 0.224 / 0.180 (-0.045, -0.089) |
| `cochange_without_reference` | 188 → 493 | 0.0 / 0.0 / 0.0 (±0, ±0) | 0.06 / 0.02 / 0.02 (-0.04, -0.04) | 0.12 / 0.05 / 0.05 (-0.07, -0.07) | 0.179 / 0.075 / 0.056 (-0.104, -0.123) |
| `fan_in` | 1,307 | 0.1 / 0.1 / 0.2 (±0, +0.1) | 0.06 / 0.06 / 0.08 (±0, +0.02) | 0.04 / 0.04 / 0.05 (±0, +0.01) | 0.060 / 0.060 / 0.056 (±0, -0.004) |
| `module_cycles`(集合) | 1,125 | | | 42 / 42 / 57 個(0.037 / 0.037 / 0.051) | |

### 変わったこと、変わらなかったこと

- **`loc` と `fan_in` の順位は、系譜で変わらない(`fan_in` は同じ fan-in の中の並びだけが変わる)。** A の値が R1 と同じか 0.01 しか違わないのはこのためで、B との差は正解の変化だけである。3 コミット以上では、系譜の正解で `loc` の P@100 が 0.23 → 0.29(本番)になった。新しく 3 コミット以上になった 22 個(本番)のうち 6 個が `loc` の上位 100 にあった(`AgentTranscript.render_item`、`AgentTranscript.render_tool_call_row`、`prepare_environment` など。どれも範囲の中で別の場所に移され、移った先でも変更されたもの)。
- **1 コミット以上では、正解の変化(B − A)はどの信号でも 0.01 以下だった。** 正解に足されたのが 8 個(本番 5 個)だけだからである。
- **`unit_churn` は、1 コミット以上の P@50・P@100 が 0.03〜0.06 下がり、3 コミット以上の P@10 が 0.2 上がった。** 下がった分は、範囲で変更されなかったのに系譜でコミットが増えた unit が上位に入ったことによる(5 節の `run_gui` 2 → 19、`gpui_component_theme_config` 1 → 11 など)。上がった分は、範囲の前に移動を繰り返した unit が上位 10 に来て、それが範囲でも 3 コミット以上変更されたことによる(本番の上位 10 で、`run_session` 13 → 42 コミット、`run_provider_stream` 1 → 31、`spawn_rig_session` が新しく入った)。
- **`fan_in_x_commits` は、本番の P@100 が 0.58 → 0.43 に下がった。** 6 節の一覧には、範囲で変更されていない fan-in の大きい小さなメソッドが、系譜のコミットで積を増やして新しく入っている(`WriterHandle.open` 1 → 9 コミット、`crates/horizon-workspace` の `SessionId.new` など)。上位 100 全体の入れ替わりの内訳は数えていない。上位 10 は 0.7 → 0.8 に上がった。
- **`cochange_without_reference` は、順位の付いた組が 2.4〜2.6 倍になった。** 系譜で各 unit のコミットが増え、`min_shared` 3 を越える組が増えたためである。上位の新しい組は、たとえば `crates/horizon-workspace/src/query.rs` の `Workspace.tab_count` と `src/session.rs` の `Workspace.session_is_referenced`(結合度 100 %、共通 5 コミット)で、2 つの共通のコミットは、最初の追加(b19e9e32)、2 度のファイル間の移動(abedcf4b、05bcc113。どちらも移った先の `added`)、可視性の一括の変更(ac33849e)、新しいクレートへの改名を伴う変更(64078019)だった。名前ではどちらも 1 コミットで、組にならなかった。`tool_result_message` と `permission_for_tool`(`tools/catalog.rs`)の組も同じ形で、共通 7 コミットのうち 3 つが移った先の `added` である。系譜の規則では、リンクのコミットは後継の側の変更として数えるので、一緒に移された unit は移動のたびに共通のコミットを 1 つ得る。本番の上位 10 のうち正解の外にあった 6 個(6 節の 1、3〜7 位)は、この 2 組と同じファイル(`tools/execution.rs`、`horizon-workspace` の `query.rs`、`session.rs`)の unit で、本番の P@10 は 0.9 → 0.4 に下がった。ほかの組の共通のコミットの内訳は数えていない。
- **`clone_one_sided` は、`all` の P@100 が 0.69 → 0.81 に上がり、本番の P@10 が 0.9 → 0.8 に下がった。** 順位の付いた組の数は変わらず(組は jscpd の報告で決まる)、片側だけの変更の数が系譜のコミットで変わって並びが変わった。
- **`module_cycles` は信号が変わらず、正解の変化だけで 3 コミット以上の割合が 0.037〜0.039 → 0.050〜0.051 になった。**

## 5. preserve の unit の位置

`production`、1 コミット以上で、各信号の上位 100 に入った preserve の unit の数(R1 → A → B)。B では preserve が 2 個増えている。

| 信号 | 上位 100 の preserve | うち変更もされた | 変更されなかったもの(B) |
| --- | ---: | ---: | --- |
| `unit_churn` | 7 → 9 → 10 | 7 → 8 → 9 | `scheme_from` |
| `fan_in_x_commits` | 5 → 7 → 7 | 5 → 5 → 5 | `scheme_from`、`wait_for_exit` |
| `fan_in` | 2 → 2 → 2 | 2 → 2 → 2 | なし |
| `clone_one_sided` | 6 → 6 → 6 | 6 → 6 → 6 | なし |
| `cochange_without_reference` | 3 → 1 → 1 | 3 → 1 → 1 | なし |
| `loc` | 13 → 13 → 15 | 10 → 10 → 12 | `box_draw_geometry`、`scheme_from`、`legacy_bytes` |
| `module_cycles`(集合) | 9 → 9 → 10 | 7 → 7 → 8 | `is_available`、`scheme_from` |

`all` では、`unit_churn` 4 → 7 → 8(変更されなかったのは `scheme_from`)、`fan_in_x_commits` 1 → 3 → 3、`clone_one_sided` 4 → 3 → 3、`loc` 7 → 7 → 8、循環 9 → 9 → 10 だった。

`scheme_from` は、名前で 2 コミット、系譜で 14 コミットになった。祖先は `src/theme.rs` の `scheme_from`(f89061dc の `content_match`)と、`shell-gpui/src/theme.rs`(04d9f0ee の改名)である。`wait_for_exit`(`crates/horizon-daemon-testkit/src/process.rs`)は名前で 1、系譜で 5 コミットで、c06aef06 で agentd と terminald の `tests/e2e.rs` にあった同名の関数 2 つが 1 つに統合された(統合の後継は両方の履歴の和を受け取る)。どちらも範囲の中では変更されず、オーナーが残すと判断した unit である。

`production` での preserve の unit の順位(左が R1、右が系譜の信号。B の preserve 32 個のうち本番の 30 個。矢印の片側や欄が空なのは、その信号に現れないこと。`paint_terminal` と `tokenize` は B で初めて preserve になったので、R1 の欄は空である)。

| unit | 変更も | churn | fan-in×commits | fan-in | clone | co-change | loc | 循環 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `tools/catalog.rs` `definitions` | ○ | 1 → 1 | 1 → 1 | 48 → 48 | 82 → 88 | 183 → | 1 | ○ |
| `session/run.rs` `run_session` | ○ | 19 → 2 | 66 → 35 | 614 → 608 | | 166 → 359 | 16 | ○ |
| `rig/completion.rs` `run_provider_stream` | ○ | 713 → 6 | | | | → 464 | 13 | ○ |
| `frame/fold.rs` `apply_agent_event_to_frame` | ○ | 62 → 14 | 20 → 8 | 110 → 108 | 3 → 1 | 47 → 226 | 20 | |
| `session_loop.rs` `run_terminal_core` | ○ | 24 → 16 | 28 → 34 | 307 → 305 | 8 → 4 | 127 → 406 | 9 | |
| `src/terminal/mod.rs` `paint_terminal`(系譜で対応) | ○ | → 17 | | | | → 463 | 11 | ○ |
| `keymap.rs` `command_for` | ○ | 17 → 39 | | | 1 → 7 | 45 → 409 | 243 | |
| `theme/scheme.rs` `scheme_from` | | 343 → 40 | 134 → 23 | 131 → 111 | 107 → 102 | → 363 | 17 | ○ |
| `tool_call/view.rs` `build_tool_call_views` | ○ | 715 → 49 | 60 → 2 | 8 → 7 | 121 → 103 | → 354 | 31 | ○ |
| `terminald/src/terminal.rs` `run_writer` | ○ | 354 → 80 | | | | → 417 | 86 | |
| `event_log/writer.rs` `rebuild_and_open_duckdb_projection` | ○ | 51 → 102 | | | | 86 → 346 | 97 | |
| `fs/edit.rs` `execute` | ○ | 67 → 103 | 162 → 285 | 630 → 636 | | 153 → 371 | 98 | ○ |
| `terminald/src/terminal.rs` `spawn_terminal` | ○ | 719 → 136 | | | 16 → 47 | → 342 | 58 | |
| `wire/src/daemon.rs` `bind_listener` | | 908 → 261 | 399 → 149 | 261 → 187 | | → 182 | 549 | |
| `daemon-testkit/src/process.rs` `wait_for_exit` | | 1,424 → 295 | 266 → 99 | 153 → 118 | | → 181 | 1,337 | |
| `session/turn.rs` `SessionLoopState.apply_turn_outcome` | ○ | 132 → 331 | 138 → 262 | 321 → 352 | 45 → 63 | 185 → 481 | 168 | ○ |
| `control/src/wire.rs` `parse_line` | | 136 → 336 | | | | → 178 | 213 | |
| `rig/clearing.rs` `project_cleared` | ○ | 425 → 351 | | | | → 98 | 424 | |
| `tools/bash/git.rs` `tokenize`(系譜で対応) | ○ | → 506 | → 593 | → 736 | → 115 | | 87 | |
| `sandbox/src/helper.rs` `resolve` | ○ | 377 → 775 | 704 → 858 | 762 → 868 | | | 166 | |
| `linux/open.rs` `match_initial_capability` | | 381 → 778 | | | | | 176 | |
| `logd/src/hub.rs` `LogHub for Hub.ingest` | | 443 → 830 | | | | | 464 | |
| `terminal/glyphs.rs` `box_draw_geometry` | | 712 → 1,278 | | | 133 → 144 | | 10 | |
| `kitty_keyboard.rs` `legacy_bytes` | | 2,305 → 1,279 | | | | | 28 | |
| `wire/src/daemon.rs` `run` | | 861 → 1,384 | 587 → 734 | 512 → 560 | | | 433 | |
| `session/input.rs` `Inputs.restore` | ○ | 921 → 1,434 | 816 → 997 | 874 → 1,007 | | | 567 | ○ |
| `preview/watch.rs` `debounce_loop` | | 975 → 1,483 | | | | | 649 | |
| `sandbox-proxy/src/handler.rs` `target_host` | | 1,251 → 1,699 | | | | | 1,073 | |
| `sandbox-proxy/src/handler.rs` `forbidden` | | 1,425 → 1,844 | | | | | 1,338 | |
| `sandbox/src/lib.rs` `is_available` | | 2,094 → 2,391 | 230 → 389 | 102 → 107 | | | 1,344 | ○ |

「変更も」は B の正解で、R1 の正解とは `paint_terminal` と `tokenize` の行だけが違う(R1 ではどちらも preserve でない)。`co-change` の欄で R1 が空なのは、R1 の 188 組にその unit が無かったことである。`loc` と循環は系譜で変わらない。

## 6. どちらにも入らなかった上位の候補

`production`、1 コミット以上で、B の正解(範囲で変更されず、preserve でもない)の外にある unit の、系譜の信号での各信号の上位 20 個である(順位は信号の中の順位)。`*` は、R1(第 1 回)の同じ一覧に無かったもの、つまり系譜で上位 20 に入ったものである。`*` の付いた 37 個は、どれも R1 でも正解の外にあり、R1 の一覧から外れた 37 個もどれも B の正解に入っていない。したがって一覧の入れ替わりは、正解の変化ではなく、すべて信号の順位の変化による。`loc` の一覧は R1 と同じである。どれも判断していない。

- `unit_churn`: 8 `command_enabled`(`crates/horizon-workspace/src/commands.rs`)、11 `core_commands`(同)、18 `Render for AgentView.render`(`src/agent/view/mod.rs`)、25 `run_gui`(`src/entry.rs`)\*、32 `snapshot_frame`(`crates/horizon-terminal-core/src/core/render.rs`)、38 `WorkspaceShell.spawn_agent_resume`(`src/workspace/session_lifecycle.rs`)、41 `Render for TerminalView.render`(`src/terminal/mod.rs`)、43 `external_name`(`crates/horizon-cli/src/commands.rs`)、44 `ListDelegate for BoardListDelegate.render_item`(`src/board_pane/list.rs`)、47 `TerminalFrame.from_text`(`crates/horizon-terminal-core/src/types/frame.rs`)、48 `handle_connection`(`crates/horizon-agentd/src/main.rs`)\*、51 `WorkspaceShell.render_tab_strip`(`src/workspace/render.rs`)\*、53 `Store.append_record_uncommitted`(`crates/horizon-agent/src/persistence/projection/duckdb/append.rs`)\*、55 `ProviderRegistry.start_session`(`crates/horizon-agent/src/registry.rs`)\*、56 `Default for RigAgentConfig.default`(`crates/horizon-agent/src/config.rs`)、58 `gpui_component_theme_config`(`src/theme/gpui_component.rs`)\*、63 `spawn`(`src/runtime/agent.rs`)\*、68 `BoardPaneView.build`(`src/board_pane.rs`)\*、77 `run`(`crates/horizon-agentd/src/main.rs`)、82 `TerminalCore.encode_key`(`crates/horizon-terminal-core/src/core.rs`)\*
- `fan_in_x_commits`: 4 `Default for RigAgentConfig.default`(`crates/horizon-agent/src/config.rs`)、10 `SessionId.new`(`crates/horizon-wire/src/session.rs`)、11 `WriterHandle.open`(`crates/horizon-agent/src/persistence/event_log/writer.rs`)\*、13 `text_muted`(`src/theme/accessors.rs`)、14 `SessionId.new`(`crates/horizon-workspace/src/session_id.rs`)\*、16 `error_output`(`crates/horizon-agent/src/tools/mod.rs`)、20 `text_primary`(`src/theme/accessors.rs`)、21 `agent_frame_from_events`(`crates/horizon-agent/src/frame/fold.rs`)\*、24 `Connection.new`(`crates/horizon-agentd/src/session/connection.rs`)\*、25 `background`(`src/theme/accessors.rs`)、26 `send_session_event`(`crates/horizon-agentd/src/session/events.rs`)\*、29 `Workspace.session_summaries`(`crates/horizon-workspace/src/query.rs`)\*、31 `accent`(`src/theme/accessors.rs`)、32 `danger`(同)、36 `truncate_chars`(`crates/horizon-agent/src/transcript/tool_call/util.rs`)\*、37 `Workspace.tab_count`(`crates/horizon-workspace/src/query.rs`)\*、38 `Appender.new`(`crates/horizon-agent/src/persistence/event_log/appender.rs`)、41 `Store.events_for_session`(`crates/horizon-agent/src/persistence/projection/duckdb/query.rs`)\*、42 `TerminalFrame.from_text`(`crates/horizon-terminal-core/src/types/frame.rs`)、43 `SessionId.as_uuid`(`crates/horizon-wire/src/session.rs`)
- `fan_in`: 1 `SessionId.new`(`crates/horizon-wire/src/session.rs`)、3 `error_output`(`crates/horizon-agent/src/tools/mod.rs`)、4 `SessionId.as_uuid`(`crates/horizon-wire/src/session.rs`)、5 `text_muted`(`src/theme/accessors.rs`)、6 `text_primary`(同)、8 `WriterHandle.flush`(`crates/horizon-agent/src/persistence/event_log/writer.rs`)、9 `background`(`src/theme/accessors.rs`)、10 `accent`(同)、11 `danger`(同)、13 `SessionId.new`(`crates/horizon-workspace/src/session_id.rs`)、14 `Default for RigAgentConfig.default`(`crates/horizon-agent/src/config.rs`)、15 `WriterHandle.open`(`crates/horizon-agent/src/persistence/event_log/writer.rs`)、18 `Connection.new`(`crates/horizon-agentd/src/session/connection.rs`)、19 `text_subtle`(`src/theme/accessors.rs`)、20 `surface_selected`(同)、22 `readable_on`(`src/theme/palette.rs`)、23 `std::ops::Index<I> for JsonValue.index`(`crates/horizon-agent/src/contract.rs`)、24 `SharedDuckdbStore.unavailable`(`crates/horizon-agent/src/persistence/projection/duckdb/shared_store.rs`)、25 `agent_frame_from_events`(`crates/horizon-agent/src/frame/fold.rs`)、26 `send_session_event`(`crates/horizon-agentd/src/session/events.rs`)\*
- `clone_one_sided`: 8 `term_key_code`(`src/terminal/input.rs`)、9 `BoardPaneView.build`(`src/board_pane.rs`)、30 `spawn`(`src/runtime/agent.rs`)、31 `spawn`(`src/runtime/terminal.rs`)、32 `BoardPaneView.open_detail`(`src/board_pane.rs`)、34 `Store.append_record_uncommitted`(`crates/horizon-agent/src/persistence/projection/duckdb/append.rs`)、37 `Hub.agent_attachment`(`crates/horizon-agentd/src/hub.rs`)、43 `create_session_args`(`crates/horizon-cli/src/commands.rs`)、44 `preview_args`(同)、48 `BoardPaneView.render_detail`(`src/board_pane/detail.rs`)、49 `BoardPaneView.task_link`(同)、50 `select_first_row_on_open`(`src/board_pane/model.rs`)、51 `select_first_row_on_open`(`src/workspace/modals.rs`)\*、57 `today`(`crates/horizon-agent/src/knowledge.rs`)、58 `format_timestamp`(`src/board_pane/model.rs`)、60 `panic_message`(`crates/horizon-agent/src/tools/web/mod.rs`)、62 `SessionHub for Hub.hello`(`crates/horizon-agentd/src/hub.rs`)、66 `drop_position_from_half`(`src/board_pane/model.rs`)\*、67 `sibling_move`(同)\*、68 `main_root`(`crates/horizon-agent/src/knowledge.rs`)
- `cochange_without_reference`: 1 `tool_result_message`(`crates/horizon-agent/src/tools/execution.rs`)\*、3 `Workspace.detached_session_count`(`crates/horizon-workspace/src/query.rs`)\*、4 `Workspace.terminate_session`(`crates/horizon-workspace/src/session.rs`)\*、5 `Workspace.tab_count`(`crates/horizon-workspace/src/query.rs`)\*、6 `Workspace.session_is_referenced`(`crates/horizon-workspace/src/session.rs`)\*、7 `Workspace.visible_panes`(`crates/horizon-workspace/src/query.rs`)\*、13 `str_field`(`crates/horizon-agent/src/transcript/tool_call/util.rs`)\*、15 `cap_lines_tail`(同)\*、16 `cap_lines_head`(同)\*、17 `TerminalHub for Hub.list_terminals`(`crates/horizon-terminald/src/hub.rs`)\*、18 `Hub.new`(`crates/horizon-agentd/src/hub.rs`)\*、20 `AgentdHandle.parts`(`src/runtime/mod.rs`)\*、21 `background`(`src/theme/accessors.rs`)、22 `terminal_color_scheme`(`src/theme/ansi.rs`)、23 `reload_from`(`src/theme/scheme.rs`)、25 `NamedProviderConfig.default_model`(`crates/horizon-agent/src/config.rs`)、32 `file_name`(`crates/horizon-agent/src/transcript/mod.rs`)\*、36 `grants_for_project`(`crates/horizon-agentd/src/session/setup.rs`)\*、37 `AllowlistProxy.spawn_shared`(`crates/horizon-sandbox-proxy/src/proxy.rs`)\*、38 `Allowlist.is_allowed`(`crates/horizon-sandbox-proxy/src/allowlist.rs`)
- `loc`: 第 1 回の 5 節と同じ 20 個(順位も同じ)。

`*` の付いた unit の、系譜の値と名前の値(コミット数、または積)は、たとえば `run_gui` 19 と 2(祖先は `src/main.rs` の `run_gui` と、その前の `body_move` で本体を渡した `main`)、`gpui_component_theme_config` 11 と 1、`WriterHandle.open` の積 81 と 9 である。`cochange_without_reference` の `*` の 15 個は、4 節で書いた、一緒に移された unit の組の片側である。`src/runtime/agent.rs` の `spawn` は、第 1 回が「名前の無い preserve の行が指すと思われる」とした unit で、系譜の `unit_churn` で 63 位に入った(名前で 4、系譜で 10 コミット)。

## 7. 基準の複製での一致の確認

`check.py refresh` を基準の複製で 1 回実行した(このコマンドは更新を 2 回行い、それぞれ、更新したすべてのビュー(系譜の 5 つを含む)とクエリをライブと比べる)。キャッシュは、その前に `run.sh` で作ったものである(信号の計算に使ったものと同じ)。

| | 時間 |
| --- | ---: |
| 1 回目の更新(何も変わっていない) | 0.26 s |
| 2 回目の更新(同) | 14.57 s(`files` 9.4 s、`checkpoint` 4.8 s) |
| コマンド全体(ライブの比較を含む) | 650.6 s、最大 18.5 GB |

どちらの更新でも、すべてのビューとクエリがライブと一致した。2 回目の更新が 1 回目の 50 倍かかったのは、ほかの作業と重なった時間の値で(`files` の段は何も変えていない)、原因は調べていない。コマンド全体の時間はライブの比較が占める。

## 8. 限界

- **範囲の中のリンクだけをたどった。** 基準より前のリンクは、基準の時点の同一性がすでに後継なので要らないが、範囲の中でも同じコミットの中のリンクしか無いこと(削除と別のコミットでの再作成はつながらない)、半分以上書き換えた移動は切れること、本体を複数の unit やファイルに移しても残り殻は切れないことは、系譜の記録の限界のとおりである。
- **移動のコミットは、後継の側の変更として数える。** 正解の側では、移動の元の `removed` が名前でもすでに数えられているので影響は小さい(1 コミット以上で +8 個)。信号の側では、一緒に移された unit が共通のコミットを得て、`cochange_without_reference` の上位を占めた(4 節)。
- **正解は「範囲の中で変更された unit」のままで、リファクタリングの対象そのものではない。** 第 1 回の 6 節の選択の偏り(大きさとクローンの信号は旧監査の選び方をなぞる、履歴の信号は「よく変わったものはまた変わる」を測る、上位にあって正解に無いことは反証ではない、preserve は旧監査の候補に限られる)は、そのまま当てはまる。系譜の信号は、範囲より前の改名と移動の履歴も数えるので、範囲の中で移動を繰り返した領域(エージェントのモジュール、ワークスペースのモデル)の unit が、正解の側でも信号の側でも増える。
- **preserve の系譜は、範囲の終わりに同じ名前で残っている unit からしかたどっていない。** 台帳の判定の後、範囲の終わりまでに名前が変わった項目は、系譜の側では対応しない。今回の 41 件では、範囲の終わりにその名前の unit が無かったのは台帳の `rig/clearing.rs` `project_cleared`(398b746a)の 1 件で(範囲の最後のコミット 46c37290 がこのファイルを変えている)、名前の対応は基準で残っている。台帳の判定のコミットは 27 件すべて範囲の中にあった。
- **閾値 3 には根拠が無い**(第 1 回と同じ)。`lineage_threshold` は既定の 0.5 だけで計算した。

## 9. コマンドと時間

```sh
nice -n 19 duckdb-ext/build.sh
uv run --locked --script scripts/refactor-audit/run.py setup
git clone -q --no-checkout . target/code-db/retro/base   # その後 checkout e3c619a0
git clone -q --no-checkout . target/code-db/retro/end    # その後 checkout 46c37290
R=$PWD/target/code-db/retro
(cd $R/base && rust-analyzer scip . --output target/code-db/index.scip)
duckdb-ext/sql/jscpd.sh $R/base $R/base/target/code-db/jscpd src crates preview-plugin/src
duckdb-ext/sql/jscpd.sh $R/end $R/end/target/code-db/jscpd src crates
# 第 1 回の形(since、別のキャッシュ)と今回の形(全履歴)で、範囲の変更が一致することの確認
duckdb-ext/sql/run.sh sql $R/end duckdb-ext/sql/retro/changes.sql since=e223bc20 \
    retro_changes=$R/changes-r1.parquet cache_path=$R/end/target/code-db/cache-since.duckdb
duckdb-ext/sql/run.sh sql $R/end duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 \
    retro_changes=$R/changes.parquet
duckdb-ext/sql/run.sh sql $R/end duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 \
    retro_reviews=$R/end/scripts/refactor-audit/reviews.json \
    retro_record_preserves=$PWD/duckdb-ext/sql/retro/september-2026-record-preserves.csv \
    retro_lineage_changes=$R/lineage-changes.parquet retro_lineage_preserves=$R/lineage-preserves.parquet
duckdb-ext/sql/run.sh query $R/base unit_churn LIMIT=5   # 基準のキャッシュの初回の構築
# R1: signals_by_name.sql、A: signals.sql、B: signals.sql に 2 つの系譜のファイル。
# それぞれ retro_scope=all|production、retro_min_commits=1|3 で
duckdb-ext/sql/run.sh sql $R/base duckdb-ext/sql/retro/signals.sql \
    retro_changes=$R/changes.parquet retro_reviews=$R/end/scripts/refactor-audit/reviews.json \
    retro_record_preserves=$PWD/duckdb-ext/sql/retro/september-2026-record-preserves.csv \
    retro_record_targets=$PWD/duckdb-ext/sql/retro/september-2026-record-targets.csv \
    retro_lineage_changes=$R/lineage-changes.parquet retro_lineage_preserves=$R/lineage-preserves.parquet \
    retro_ranks_out=$R/ranks.csv retro_all_ranks_out=$R/all.csv retro_scope=production retro_min_commits=1
uv run --locked --script duckdb-ext/sql/check.py refresh $R/base
```

| 段階 | 時間 | 最大 RSS |
| --- | ---: | ---: |
| 基準の `rust-analyzer scip`(索引 23.9 MB) | 186.9 s | 6.32 GB |
| 終わりの `changes.sql`、`since`(第 1 回の形、キャッシュの初回の構築を含む) | 9.3 s | 3.36 GB |
| 終わりの `changes.sql`、`retro_since`(全履歴のキャッシュの初回の構築、系譜を含む) | 24.7 s | 6.46 GB |
| 終わりの `lineage.sql` | 1.7 s | 0.65 GB |
| 基準のキャッシュの初回の構築(全履歴、系譜と SCIP を含む、`query` 1 回分) | 17.9 s | 6.41 GB |
| `signals.sql`(A と B、4 つの組で各 1 回) | 2.2〜2.9 s | 1.24〜1.34 GB |
| `signals_by_name.sql`(R1、同) | 4.4〜5.2 s | 4.63〜6.14 GB |
| `check.py refresh`(7 節) | 650.6 s | 18.5 GB |

時間は `run.sh` などを包んだ Python の `resource.getrusage(RUSAGE_CHILDREN)` で取った 1 回ずつの値で、`nice -n 19` で動かした。jscpd の時間は取っていない。基準のキャッシュは 470 MB、終わりのキャッシュ(全履歴)は 566 MB だった。第 1 回の終わりの `changes.sql` は 22.2 s だったが、今回の同じ形は 9.3 s で、違いの原因は調べていない。

正解の同一性ごとの順位(B、`all`、1 コミット以上、1,754 行。列は第 1 回と同じで、`source` の「(N by name)」は名前で数えたコミット数、「via lineage」は系譜だけで対応した preserve)は [code-db-dogfood-retrospective-2-2026-10-26-ranks.csv](code-db-dogfood-retrospective-2-2026-10-26-ranks.csv) に置いた。
