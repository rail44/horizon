# コードデータベースの信号を 2026 年 9 月のリファクタリングに照らした記録(2026-10-26)

コードデータベースの SQL 層を実際に使い、その信号が、オーナーが 2026 年 9 月に実際に行ったリファクタリングの対象とどれだけ重なるかを後ろ向きに確かめた。あわせて、層を 1 つのコマンドで使う入口 `duckdb-ext/sql/run.sh` を足した。観察だけを記し、推奨は書かない。

オーナーの依頼は「実際にdoghoodして、仕組みの品質を検証できるようにしてもらえますか?」だった。依頼側が示した形(薄い起動スクリプト、e3c619a0 を基準にした後ろ向きの確認、別の worker による前向きのレビュー、記録)に、オーナーは 2026-10-26 に「はい。それで進めましょう。」と答えた。前向きのレビューは別の worker が [code-db-dogfood-prospective-2026-10-26.md](code-db-dogfood-prospective-2026-10-26.md) に書く。この記録はそのうち後ろ向きの確認である。

前提は [設計文書](../code-database-design.md)、[SQL 層の README](../../duckdb-ext/sql/README.md)、前の記録 [code-db-graph-2026-10-26.md](code-db-graph-2026-10-26.md) と [code-db-scip-2026-10-25.md](code-db-scip-2026-10-25.md)、調査の結論 [code-quality-analysis-trends-2026-10-24.md](code-quality-analysis-trends-2026-10-24.md) の「現行の監査ツールの各部品に対して証拠が言えること」の判定台帳の項である。そこには「ラベル付きの判定を積み重ねたものが、このリポジトリで最も妥当なベンチマークになり得る」とある。この記録は、9 月の記録と旧監査の台帳を、そのラベルの代わりに使った最初の試みである。

作業は worker の作業ツリーで `origin/code-quality-survey` の 2ebd0ecb から始めた。機械は前回までと同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している(作業中の load average は 14〜17)。DuckDB は CLI 2.0.0、rust-analyzer は 1.99.1、jscpd は旧監査の固定版 5.3.1 である。Rust は変えていない。

## 主要な観察

- **基準 e3c619a0 は、リファクタリングの履歴の祖先ではなかった。** e3c619a0 は main に入る前に付け直され、main 上の同じ題のコミットは e223bc20 である。両者の木の違いは、その直前に main に入った PR #60(MoA の chip と CLI、19 ファイル)だけだった。信号は依頼どおり e3c619a0 の木と履歴で計算し、リファクタリングの範囲は `e223bc20..46c37290` とした(1 節)。
- **範囲の 103 コミット(マージを除く)のうち、Rust の unit を変えたのは 84 コミットで、e3c619a0 にあった unit の同一性 6,903 個のうち 1,714 個(24.8 %)を変更または削除していた。** 本番の関数・メソッドに限ると 2,583 個のうち 755 個(29.2 %)である。3 コミット以上に限ると、どちらも 2.6 %(182 個、67 個)になる。
- **記録が名指しした 11 個のリファクタリング対象は、すべて変更された集合に入っていた。**
- **preserve の出どころ 41 件(台帳 27 件、記録の行 14 件)のうち 32 件が e3c619a0 の unit に対応し、同一性は 30 個になった。** 対応しなかった 9 件はすべて台帳の項目で、e3c619a0 の時点ではその名前の関数がそのファイルに無かった(2 節)。preserve の同一性 30 個のうち 15 個は、範囲の中で変更もされていた。
- **変更された集合を当てる precision は、どの範囲でも `unit_churn` と `loc` が上位で、`fan_in` が最も低かった。** 本番・1 コミット以上で、precision@100 は `loc` 0.75、`unit_churn` 0.74、`clone_one_sided` 0.65、`fan_in_x_commits` 0.58、`cochange_without_reference` 0.49、`fan_in` 0.28 で、無作為に選んだ場合の期待値は 0.29 である(3 節)。
- **どの信号でも、上位 100 に入った preserve の unit のうち、範囲の中で変更されなかったものは、`loc` の 3 個(`box_draw_geometry`、`scheme_from`、`legacy_bytes`)とモジュールの循環の 2 個(`is_available`、`scheme_from`)だけだった。** 履歴の信号の上位 100 に入った preserve の unit は、すべて範囲の中で変更されてもいた(4 節)。
- **e3c619a0 の索引は rust-analyzer で 226.8 s、最大 6.33 GB で作れ、キャッシュの初回の構築は 18.9 s、その後の `signals.sql` 全体は 2.6〜2.9 s だった。**

## 1. 範囲と基準

依頼文は「リファクタリングのコミットは e3c619a0 から sweep の終わりまでの履歴にある」としていた。`git merge-base --is-ancestor e3c619a0 46c37290` は偽で、e3c619a0 と 46c37290 の共通祖先は 43d05c13 である。e3c619a0(親 16d1406f)と e223bc20(親 fbcb7272)は同じ題「Add reproducible refactoring discovery and review tooling」を持ち、e223bc20 の親の側には 3bdf08ed(PR #60 のマージ)がある。`git diff --stat e3c619a0 e223bc20` は 19 ファイル、472 行の追加と 22 行の削除で、すべて PR #60 の変更だった。

決めたこと(依頼文が決めていない点):

- **信号は e3c619a0 の木と、その祖先の履歴で計算した。** オーナーが承認した基準がこのコミットだからである。PR #60 のコミットは基準の履歴に入らない。
- **リファクタリングの範囲は `e223bc20..46c37290` とした。** e223bc20 は e3c619a0 の付け直しで、その後の main のコミットはすべて e223bc20 の子孫である。
- **範囲の終わりは 46c37290 とした。** 旧監査の台帳 `scripts/refactor-audit/reviews.json` を最後に変えたコミットで(09-27 01:46)、9 月の記録の最後のコミット(b9a2f2ae、09-25)より後にある。その次の first-parent のコミット c96f685a(エージェントの接続の再生)以降は、ボードの再設計と ACP への移行の統合が続く。
- **範囲のコミットはすべて数えた。** マージ b3a06b54 は既定(`include_merges` が偽)で除かれる。そのマージで入った 95550d47(エージェントのプロンプトの文言の書き直し)はリファクタリングの記録に無いが、マージではないので数えた。変更された集合 1,714 個のうち、このコミットだけが変えた同一性は 22 個である。範囲には、ボードの作業(59e0934f、ee60e19e は 9 月 22 日の第一巡の一部)、テストの安定化(86601ea1)、記録だけのコミットも含まれる。

範囲は 104 コミット、マージを除いて 103 コミットで、そのうち `unit_changes` に行を持つのは 84 コミット(4,771 行、同一性 3,415 個)だった。残りは Rust 以外のファイルだけを変えている。

## 2. 正解の表

`retro_truth(path, unit_kind, qualified_name, decision, source)` は `duckdb-ext/sql/retro/signals.sql` が作る一時表である。unit の同一性は `unit_changes` と同じ `(path, kind, qualified_name)` で、同じ同一性を持つ複数の行(1 つのファイルの 2 つの `impl Foo`)は 1 つとし、`loc` は和をとった。

### refactored

範囲の終わりの複製(46c37290)で `since := e223bc20` として `unit_changes` を書き出し(`retro/changes.sql`、Parquet)、e3c619a0 の同一性のうち、その中で `modified` または `removed` と記録されたものを `refactored` とした。`unit_changes` の同一性は改名や移動を追わないので、範囲の中でファイルを移った unit は、元の場所で `removed` になる。入れ子の unit(impl、モジュール)は、中の変更をすべて受け取る。したがってこの集合は「リファクタリングの対象」ではなく、「範囲の中で変更された unit」である。`retro_min_commits` で、範囲の中で変更されたコミットの数の下限を決められる。

記録の表との照合には、記録が関数の名前で対象を書いている 2 つの記録の表から 11 個を取った(`retro/september-2026-record-targets.csv`)。[初回確認](refactoring-first-pass-2026-09-22.md) の `establish_for_drain`(agent、terminal)、`writer::perform`、`tree_order` と、[第一巡](refactoring-sweep-2026-09-22.md) の入口関数の表の 7 個(`dispatch_invoke`、`run_board`、`WorkspaceState::validate`、`spawn_workspace_restore`、grants の `resolve`、`SessionLoopState::run`、`resume_persisted_sessions`)である。11 個すべてが 1 コミット以上の集合に入っていた(`removed` が 4 個、`modified` が 7 個)。3 コミット以上に限ると、入っていたのは `SessionLoopState.run`(6 コミット)だけだった。他の記録の表は、領域や経路の単位で書かれていて、関数の名前で対象を書いていないので照合に使っていない。

### preserve

出どころは 2 つである。

- **旧監査の台帳**(46c37290 の `reviews.json`、27 件、すべて `preserve`)。鍵は `target.file`、`target.owner`、`target.name` である。`owner` が `<free>` のものは、限定名がその名前の unit か、インラインモジュールの中の同名の関数に、`owner` があるものは `owner.name` のメソッドに対応させた。cfg の違う 3 つの `is_available` は、1 つの同一性になる。
- **記録の preserve の行**(14 件、`retro/september-2026-record-preserves.csv`)。[初回確認](refactoring-first-pass-2026-09-22.md) の「`tools/catalog.rs::definitions` ― 行数だけを理由に分割しない」と、[第一巡](refactoring-sweep-2026-09-22.md) の領域ごとの表のうち、判断の欄が維持だけで、関数や型の名前をバッククォートで書いている行(horizon-control の `wire::parse_line`、daemon-testkit の `process::{DaemonProcess,wait_for_exit}`、logd の `hub::ingest`、sandbox の `helper::resolve`、sandbox-proxy の `handler::{target_host,forbidden}`、terminal-core の `legacy_bytes`、terminald の `terminal::{spawn_terminal,run_writer}`、wire の `daemon::{bind_listener,run}`、theme の `scheme_from`)である。記録は `hub::ingest` のようにモジュールの道筋で書くので、名前の最後の部分が一致する unit に対応させた(`LogHub for Hub.ingest`、`DaemonProcess` は型と impl の 2 つ)。この抜き出しは手作業である。

他の記録の「維持」は、ほとんどが関数ではなく契約や境界(「取消の優先、未処理入力の保留……を維持」)について書いていて、unit に対応させる規則を決められなかったので、使っていない。たとえば初回確認の「agent/terminalの接続復旧ループ ― 判断部分は維持」は名前を書いておらず、対応させていない(この行が指すと思われる `src/runtime/agent.rs` と `src/runtime/terminal.rs` の `spawn` は、5 節の候補の一覧に出てくる)。

対応の結果:

| 出どころ | 件数 | 対応した | 対応しなかった |
| --- | ---: | ---: | ---: |
| 台帳 | 27 | 18 | 9 |
| 記録の行 | 14 | 14 | 0 |
| 計 | 41 | 32 | 9 |

対応した 32 件は、重複(`is_available` の 3 件、`legacy_bytes` と `scheme_from` が両方の出どころにあること)と `DaemonProcess` の 2 つを差し引きして、同一性 30 個になった。本番の関数・メソッドに限ると 28 個である。

対応しなかった 9 件は、どれも判定が付けられたコミットの時点の名前で、e3c619a0 にはその名前の関数がそのファイルに無かった。

| 台帳の項目 | e3c619a0 での状態 |
| --- | --- |
| `crates/horizon-agent/src/policy.rs` `plan_prepared_call` | ファイルはあるが、関数が無い |
| `src/workspace/modals.rs` `WorkspaceShell.fetch_picker` | 同上 |
| `crates/horizon-agentd/src/session/completion.rs` `publish_tool_update` | 同上 |
| `crates/horizon-agent/src/tools/execution.rs` `execute_synchronous` | 同上 |
| `crates/horizon-agent/src/tools/bash/shell.rs` `tokenize` | ファイルが無い。同名の関数は `tools/bash/git.rs` にある |
| `src/terminal/paint.rs` `paint_terminal` | ファイルが無い。同名の関数は `src/terminal/mod.rs` にある |
| `crates/horizon-agent/src/tools/completion.rs` `ToolCompletion.live_request` | ファイルも関数も無い |
| `crates/horizon-agent/src/providers/rig/conversation/mod.rs` `ConversationHistory.apply` | 同上 |

`tokenize` と `paint_terminal` は、範囲の中でファイルを移った関数への判定とみなせるが、層の同一性は移動を追わないので、対応させなかった。

preserve の同一性 30 個のうち 15 個(本番では 28 個のうち 15 個)は、範囲の中で変更もされていた。台帳の判定は、その unit の周りを変えたコミットで付けられたものが多い(`apply_agent_event_to_frame` の判定は 6cab1f6e で、このコミットは `fold.rs` を変えている)。したがって、preserve は「手を付けなかった」ではなく、「その入口の形を残すと判断した」と読むのが記録に近い。

## 3. 信号の順位と precision

e3c619a0 の複製で、層に入っているクエリの本文をそのまま `query()` で実行し(`retro/signals.sql` が `queries/*.sql` を読む)、各クエリの並びをそのまま順位にした。同順位の並べ方はクエリの並びの鍵に従う(`unit_churn` はコミット数が同じなら `loc` の大きい順)。同一性の順位は、その最初の行の位置である。

| 信号 | 順位の付け方 |
| --- | --- |
| `unit_churn` | クエリの並び(コミット数、`loc` の順) |
| `fan_in_x_commits` | `unit_fan_in_churn` の行を `fan_in_files × commits` の大きい順に並べ直したもの |
| `fan_in` | `unit_fan_in_churn` の並び(fan-in、コミット数の順) |
| `clone_one_sided` | `clone_change_consistency` の並び(片側だけの変更の数の順)。組の両側の unit に、その組の位置を与える |
| `cochange_without_reference` | クエリの並び(結合度の順)。組の両側の unit に、その組の位置を与える。閾値は既定(`min_shared` 3、`min_coupling_pct` 30) |
| `loc` | `loc` の大きい順(素朴な基準線) |
| `module_cycles` | 順位なし。循環に入るモジュールに属する unit の集合 |

範囲(scope)は 2 つ計算した。`all` はすべての同一性、`production` は関数とメソッドのうちテストでないもの(`test` タグに含まれず、パスが `tests.rs`、`*_tests.rs`、`tests/` でない)である。範囲を絞ったときの順位は、その範囲の中で付け直した。precision@k は上位 k 個のうち変更された集合に入る割合、recall@100 は変更された集合のうち上位 100 個に入る割合である。変更された集合が大きいと、recall@100 の上限は小さい(`all`・1 コミット以上で 100 / 1,714 = 0.058)。

### `all`、1 コミット以上(1,714 / 6,903、期待値 0.248)

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 6,903 | 1.0 | 0.86 | 0.83 | 0.048 |
| `loc` | 6,903 | 0.9 | 0.80 | 0.76 | 0.044 |
| `clone_one_sided` | 652 | 0.7 | 0.76 | 0.69 | 0.040 |
| `fan_in_x_commits` | 2,107 | 0.7 | 0.66 | 0.55 | 0.032 |
| `cochange_without_reference` | 529 | 0.7 | 0.56 | 0.33 | 0.019 |
| `fan_in` | 2,107 | 0.3 | 0.40 | 0.34 | 0.020 |
| `module_cycles`(集合) | 1,551 | | | 594 / 1,551 = 0.38 | |

### `production`、1 コミット以上(755 / 2,583、期待値 0.292)

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.9 | 0.82 | 0.74 | 0.098 |
| `loc` | 2,583 | 0.8 | 0.78 | 0.75 | 0.099 |
| `clone_one_sided` | 154 | 0.9 | 0.74 | 0.65 | 0.086 |
| `fan_in_x_commits` | 1,307 | 0.7 | 0.64 | 0.58 | 0.077 |
| `cochange_without_reference` | 188 | 0.9 | 0.30 | 0.49 | 0.065 |
| `fan_in` | 1,307 | 0.2 | 0.32 | 0.28 | 0.037 |
| `module_cycles`(集合) | 1,125 | | | 466 / 1,125 = 0.41 | |

### `all`、3 コミット以上(182 / 6,903、期待値 0.026)

| 信号 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: |
| `unit_churn` | 0.6 | 0.36 | 0.36 | 0.198 |
| `loc` | 0.6 | 0.42 | 0.36 | 0.198 |
| `fan_in_x_commits` | 0.4 | 0.28 | 0.24 | 0.132 |
| `clone_one_sided` | 0.3 | 0.26 | 0.21 | 0.115 |
| `fan_in` | 0.1 | 0.08 | 0.09 | 0.049 |
| `cochange_without_reference` | 0.0 | 0.06 | 0.05 | 0.027 |
| `module_cycles`(集合) | | | 60 / 1,551 = 0.039 | |

### `production`、3 コミット以上(67 / 2,583、期待値 0.026)

| 信号 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: |
| `unit_churn` | 0.3 | 0.32 | 0.27 | 0.403 |
| `loc` | 0.4 | 0.30 | 0.23 | 0.343 |
| `fan_in_x_commits` | 0.4 | 0.22 | 0.18 | 0.269 |
| `clone_one_sided` | 0.5 | 0.22 | 0.17 | 0.254 |
| `cochange_without_reference` | 0.0 | 0.06 | 0.12 | 0.179 |
| `fan_in` | 0.1 | 0.06 | 0.04 | 0.060 |
| `module_cycles`(集合) | | | 42 / 1,125 = 0.037 | |

順位の付いた数は 1 コミット以上と同じである(範囲が同じなら順位は変わらず、正解の集合だけが変わる)。`all`・3 コミット以上で `unit_churn` と `loc` の P@10・P@100 が同じ値になったのは、この表で見た限りの一致で、上位の顔ぶれが同じかは確かめていない。

正解の同一性ごとの順位は [code-db-dogfood-retrospective-2026-10-26-ranks.csv](code-db-dogfood-retrospective-2026-10-26-ranks.csv) に置いた(`all`、1 コミット以上、1,742 行。列は `decision`、同一性、`source`(変更の種類と範囲の中のコミット数、または preserve の出どころ)、6 つの信号の順位、循環に入るか)。`production` の順位と 3 コミット以上は、5 節のコマンドで出し直せる。

## 4. preserve の unit の位置

preserve の unit が信号の上位にあることを、オーナー自身の判断による誤報とみなす、というのが依頼文の読み方である。ただし 2 節のとおり、preserve の半分は範囲の中で変更もされている。上位 100 に入った数を、その 2 つに分けて数えた。

| 信号 | `production` 上位 100 の preserve | うち変更もされた | 変更されなかった | `all` 上位 100 の preserve |
| --- | ---: | ---: | --- | ---: |
| `unit_churn` | 7 | 7 | なし | 4 |
| `fan_in_x_commits` | 5 | 5 | なし | 1 |
| `fan_in` | 2 | 2 | なし | 1 |
| `clone_one_sided` | 6 | 6 | なし | 4 |
| `cochange_without_reference` | 3 | 3 | なし | 0 |
| `loc` | 13 | 10 | `box_draw_geometry`、`scheme_from`、`legacy_bytes` | 7 |
| `module_cycles`(集合) | 9 | 7 | `is_available`、`scheme_from` | 9 |

`production` での preserve の unit の順位(NULL はその信号に現れないこと):

| unit | 変更も | churn | fan-in×commits | fan-in | clone | co-change | loc | 循環 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| `tools/catalog.rs` `definitions` | ○ | 1 | 1 | 48 | 82 | 183 | 1 | ○ |
| `keymap.rs` `command_for` | ○ | 17 | | | 1 | 45 | 243 | |
| `session/run.rs` `run_session` | ○ | 19 | 66 | 614 | | 166 | 16 | ○ |
| `session_loop.rs` `run_terminal_core` | ○ | 24 | 28 | 307 | 8 | 127 | 9 | |
| `event_log/writer.rs` `rebuild_and_open_duckdb_projection` | ○ | 51 | | | | 86 | 97 | |
| `frame/fold.rs` `apply_agent_event_to_frame` | ○ | 62 | 20 | 110 | 3 | 47 | 20 | |
| `fs/edit.rs` `execute` | ○ | 67 | 162 | 630 | | 153 | 98 | ○ |
| `session/turn.rs` `SessionLoopState.apply_turn_outcome` | ○ | 132 | 138 | 321 | 45 | 185 | 168 | ○ |
| `control/src/wire.rs` `parse_line` | | 136 | | | | | 213 | |
| `theme/scheme.rs` `scheme_from` | | 343 | 134 | 131 | 107 | | 17 | ○ |
| `terminald/src/terminal.rs` `run_writer` | ○ | 354 | | | | | 86 | |
| `sandbox/src/helper.rs` `resolve` | ○ | 377 | 704 | 762 | | | 166 | |
| `linux/open.rs` `match_initial_capability` | | 381 | | | | | 176 | |
| `rig/clearing.rs` `project_cleared` | ○ | 425 | | | | | 424 | |
| `logd/src/hub.rs` `LogHub for Hub.ingest` | | 443 | | | | | 464 | |
| `rig/completion.rs` `run_provider_stream` | ○ | 713 | | | | | 13 | ○ |
| `terminal/glyphs.rs` `box_draw_geometry` | | 712 | | | 133 | | 10 | |
| `tool_call/view.rs` `build_tool_call_views` | ○ | 715 | 60 | 8 | 121 | | 31 | ○ |
| `terminald/src/terminal.rs` `spawn_terminal` | ○ | 719 | | | 16 | | 58 | |
| `wire/src/daemon.rs` `run` | | 861 | 587 | 512 | | | 433 | |
| `wire/src/daemon.rs` `bind_listener` | | 908 | 399 | 261 | | | 549 | |
| `session/input.rs` `Inputs.restore` | ○ | 921 | 816 | 874 | | | 567 | ○ |
| `preview/watch.rs` `debounce_loop` | | 975 | | | | | 649 | |
| `sandbox-proxy/src/handler.rs` `target_host` | | 1,251 | | | | | 1,073 | |
| `daemon-testkit/src/process.rs` `wait_for_exit` | | 1,424 | 266 | 153 | | | 1,337 | |
| `sandbox-proxy/src/handler.rs` `forbidden` | | 1,425 | | | | | 1,338 | |
| `sandbox/src/lib.rs` `is_available` | | 2,094 | 230 | 102 | | | 1,344 | ○ |
| `kitty_keyboard.rs` `legacy_bytes` | | 2,305 | | | | | 28 | |

変更されなかった preserve の unit 13 個は、履歴の信号(churn、fan-in×commits、clone、co-change)ではどれも上位 100 に入らなかった。`loc` の上位にあった 3 個(`box_draw_geometry` 10 位、`scheme_from` 17 位、`legacy_bytes` 28 位)は、旧監査の大きさの信号で候補になり、オーナーが残すと判断したものである(第一巡の表は `legacy_bytes` を「プロトコルの対応表を維持」、`scheme_from` を「seedから各roleの色を導出するまとまりを維持」としている)。

## 5. どちらにも入らなかった上位の候補

`production`、1 コミット以上で、範囲の中で変更されず、preserve でもない unit の、各信号の上位 20 個である(順位は信号の中の順位)。どれも判断していない。前向きのレビューは、現在の木の候補を別の worker が判断する。

- `unit_churn`: 4 `command_enabled`(`crates/horizon-workspace/src/commands.rs`)、11 `core_commands`(同)、31 `snapshot_frame`(`crates/horizon-terminal-core/src/core/render.rs`)、34 `Render for TerminalView.render`(`src/terminal/mod.rs`)、38 `BoardPaneView.new`(`src/board_pane.rs`)、39 `Default for RigAgentConfig.default`(`crates/horizon-agent/src/config.rs`)、43 `WorkspaceShell.spawn_agent_resume`(`src/workspace/session_lifecycle.rs`)、45 `AgentSession.new`(`src/agent/session.rs`)、49 `Render for BoardPaneView.render`(`src/board_pane.rs`)、55 `Render for AgentView.render`(`src/agent/view/mod.rs`)、59 `external_name`(`crates/horizon-cli/src/commands.rs`)、60 `BoardPaneView.render_detail`(`src/board_pane/detail.rs`)、63 `ListDelegate for BoardListDelegate.render_item`(`src/board_pane/list.rs`)、65 `ThemeSettingsView.new`(`src/theme_settings/mod.rs`)、71 `BoardPaneView.spawn_load`(`src/board_pane.rs`)、75 `TerminalFrame.from_text`(`crates/horizon-terminal-core/src/types/frame.rs`)、76 `resolve`(`src/theme/ansi.rs`)、77 `run`(`crates/horizon-agentd/src/main.rs`)、78 `main`(`src/main.rs`)、80 `derive_bindings`(`src/workspace/bindings.rs`)
- `fan_in_x_commits`: 3 `Default for RigAgentConfig.default`(`crates/horizon-agent/src/config.rs`)、7 `text_muted`(`src/theme/accessors.rs`)、10 `SessionId.new`(`crates/horizon-wire/src/session.rs`)、11 `text_primary`(`src/theme/accessors.rs`)、13 `background`(同)、16 `accent`(同)、17 `danger`(同)、22 `text_subtle`(同)、23 `surface_selected`(同)、29 `error_output`(`crates/horizon-agent/src/tools/mod.rs`)、31 `reload_from`(`src/theme/scheme.rs`)、32 `TerminalFrame.from_text`(`crates/horizon-terminal-core/src/types/frame.rs`)、35 `SessionId.as_uuid`(`crates/horizon-wire/src/session.rs`)、39 `readable_on`(`src/theme/palette.rs`)、41 `border`(`src/theme/accessors.rs`)、42 `terminal_color_scheme`(`src/theme/ansi.rs`)、44 `core_commands`(`crates/horizon-workspace/src/commands.rs`)、46 `spawn_session_thread`(`crates/horizon-agentd/src/session/spawn.rs`)、52 `Appender.new`(`crates/horizon-agent/src/persistence/event_log/appender.rs`)、54 `Store.open`(`crates/horizon-agent/src/persistence/projection/duckdb/mod.rs`)
- `fan_in`: 1 `SessionId.new`(`crates/horizon-wire/src/session.rs`)、3 `error_output`(`crates/horizon-agent/src/tools/mod.rs`)、4 `SessionId.as_uuid`(`crates/horizon-wire/src/session.rs`)、5 `text_muted`(`src/theme/accessors.rs`)、6 `text_primary`(同)、7 `WriterHandle.flush`(`crates/horizon-agent/src/persistence/event_log/writer.rs`)、9 `background`(`src/theme/accessors.rs`)、10 `accent`(同)、11 `danger`(同)、13 `SessionId.new`(`crates/horizon-workspace/src/session_id.rs`)、14 `Default for RigAgentConfig.default`(`crates/horizon-agent/src/config.rs`)、16 `text_subtle`(`src/theme/accessors.rs`)、17 `surface_selected`(同)、20 `readable_on`(`src/theme/palette.rs`)、21 `std::ops::Index<I> for JsonValue.index`(`crates/horizon-agent/src/contract.rs`)、22 `WriterHandle.open`(`crates/horizon-agent/src/persistence/event_log/writer.rs`)、23 `SharedDuckdbStore.unavailable`(`crates/horizon-agent/src/persistence/projection/duckdb/shared_store.rs`)、24 `Connection.new`(`crates/horizon-agentd/src/session/connection.rs`)、25 `agent_frame_from_events`(`crates/horizon-agent/src/frame/fold.rs`)、26 `ToolSessionState.workspace_root`(`crates/horizon-agent/src/tools/state.rs`)
- `clone_one_sided`: 2 `term_key_code`(`src/terminal/input.rs`)、13 `Store.append_record_uncommitted`(`crates/horizon-agent/src/persistence/projection/duckdb/append.rs`)、17 `BoardPaneView.build`(`src/board_pane.rs`)、18 `BoardPaneView.open_detail`(同)、19 `BoardPaneView.render_detail`(`src/board_pane/detail.rs`)、20 `BoardPaneView.task_link`(同)、21 `create_session_args`(`crates/horizon-cli/src/commands.rs`)、22 `preview_args`(同)、40 `panic_message`(`crates/horizon-agent/src/tools/web/mod.rs`)、47 `spawn`(`src/runtime/agent.rs`)、48 `spawn`(`src/runtime/terminal.rs`)、49 `Hub.agent_attachment`(`crates/horizon-agentd/src/hub.rs`)、50 `SessionHub for Hub.hello`(同)、54 `main_root`(`crates/horizon-agent/src/knowledge.rs`)、55 `main_root`(`crates/horizon-board/src/path.rs`)、56 `today`(`crates/horizon-agent/src/knowledge.rs`)、57 `format_timestamp`(`src/board_pane/model.rs`)、58 `moa_entry_warnings`(`crates/horizon-config/src/warnings.rs`)、59 `provider_entry_warnings`(同)、64 `select_first_row_on_open`(`src/board_pane/model.rs`)
- `cochange_without_reference`: 8 `NamedProviderConfig.default_model`(`crates/horizon-agent/src/config.rs`)、11 `classify_call`(`crates/horizon-agent/src/transcript/mod.rs`)、12 `AllowlistProxy.spawn`(`crates/horizon-sandbox-proxy/src/proxy.rs`)、13 `Allowlist.is_allowed`(`crates/horizon-sandbox-proxy/src/allowlist.rs`)、14 `main`(`crates/horizon-sandbox/src/bin/horizon-sandbox-network-probe.rs`)、15 `sgr_mouse_input`(`crates/horizon-terminal-core/src/core/input.rs`)、16 `event_type_subfield`(`crates/horizon-terminal-core/src/protocol/kitty_keyboard.rs`)、17 `ViewKind.title`(`crates/horizon-workspace/src/types/kind.rs`)、18 `From<ViewKind> for ViewKindState.from`(`crates/horizon-workspace/src/persistence.rs`)、19 `From<ViewKindState> for ViewKind.from`(同)、20 `accent`(`src/theme/accessors.rs`)、21 `terminal_color_scheme`(`src/theme/ansi.rs`)、22 `to_hsla`(同)、23 `apply_gpui_component_theme`(`src/theme/gpui_component.rs`)、24 `reload_from`(`src/theme/scheme.rs`)、25 `background`(`src/theme/accessors.rs`)、26 `border`(同)、27 `danger`(同)、28 `diff_added_surface`(同)、29 `diff_added_text`(同)
- `loc`: 4 `BoardPaneView.render_detail`(`src/board_pane/detail.rs`)、14 `core_commands`(`crates/horizon-workspace/src/commands.rs`)、29 `spawn`(`src/runtime/agent.rs`)、32 `derive_bindings`(`src/workspace/bindings.rs`)、33 `ListDelegate for BoardListDelegate.render_item`(`src/board_pane/list.rs`)、36 `sample_events`(`src/board_pane/previews.rs`)、47 `box_draw_segments`(`src/terminal/glyphs.rs`)、48 `ThemeSettingsView.new`(`src/theme_settings/mod.rs`)、52 `spawn`(`src/runtime/terminal.rs`)、53 `handle_network_notification`(`crates/horizon-sandbox-runtime/src/linux/network.rs`)、55 `run`(`crates/horizon-cli/src/lib.rs`)、60 `execute`(`crates/horizon-sandbox-runtime/src/execute.rs`)、61 `handle_open_notification`(`crates/horizon-sandbox-runtime/src/linux/open.rs`)、62 `WorkspaceShell.render_tab_strip`(`src/workspace/render.rs`)、68 `gpui_component_theme_config`(`src/theme/gpui_component.rs`)、69 `Render for BoardPaneView.render`(`src/board_pane.rs`)、75 `process_project`(`crates/horizon-agentd/src/board_flow/dispatch.rs`)、83 `ensure_stream_started`(`crates/horizon-sandbox/src/macos/denials.rs`)、88 `BoardPaneView.board_command`(`src/board_pane/operations.rs`)、90 `Render for TerminalView.render`(`src/terminal/mod.rs`)

この 120 個のうち 19 個はボード(`src/board_pane*`、`horizon-board`、agentd の `board_flow`)の unit である。オーナーは 2026-09-22 に、ボードを以後のリファクタリングの候補から外した(第一巡の記録「board関連(画面・CLI・horizon-board・horizon-logd)は別作業で進めるため、以後のこのリファクタリングの候補選定・変更対象から除外する」)。これらが範囲で変更されていないのは、その決定による。`src/runtime/agent.rs` と `src/runtime/terminal.rs` の `spawn` は、2 節で触れた、名前の無い preserve の行が指すと思われる unit である。`src/theme/accessors.rs` の関数は、互いを参照せずにまとめて変わる表のような関数群で、fan-in と co-change の両方の上位に並んだ。`all` の一覧は、テストのモジュールとテスト関数、大きな impl と型が多くを占める(5 節のコマンドの出力にある)。

## 6. 選択の偏り

9 月のリファクタリングは、対象を旧監査の信号で選んだ。記録は、RCA の認知的複雑度と行数、jscpd のクローン、ast-grep の一致を「選定の入口」とし、そのうえで呼び出し元・同じ責務の実装・テストを確かめて判断したと書いている(第一巡「解析値は選定の入口とし……」、初回確認「数値の大きさに加え……」)。したがって:

- **大きさとクローンの信号(`loc`、`clone_one_sided`)と正解の重なりは、選び方そのものを一部なぞっている。** `loc` はここでは素朴な基準線として置いたが、旧監査が実際に使った信号の近縁である。`clone_one_sided` は jscpd の同じ報告から作るが、片側だけの変更という履歴の条件を足している。
- **履歴の信号(`unit_churn`、`fan_in_x_commits`、`cochange_without_reference`)は旧監査に無かったので、それらとの重なりは独立な一致である。** ただし `unit_churn` は「過去によく変わった unit は、次の期間にも変わる」という関係を測っており、正解が「範囲の中で変更された unit」である以上、リファクタリングでない変更(機能の追加に伴う変更、テストの書き換え)でも重なる。`unit_churn` の同順位は `loc` で分けている。
- **信号の上位にあって正解に無いことは、反証ではない。** sweep が見なかった(5 節)、範囲から外した(ボード)、見て残したが記録に名前が無い、のどれでも、正解に入らない。逆に、正解の 1 コミット以上の集合は、リファクタリングに伴って触れただけの unit を多く含む。
- 正解の preserve は、旧監査の候補になったものに限られる。旧監査が候補にしなかった unit には、preserve の判定がそもそも無い。

## 7. うまくいかなかったこと、決めたこと

- **基準が範囲の祖先でなかった**(1 節)。範囲を e223bc20 からとしたのは、この記録の判断である。
- **記録の「維持」の多くは unit に対応しなかった**(2 節)。手作業で 14 行を抜き出し、残りは使っていない。
- **台帳の 9 件は、判定の時点の名前で書かれていて、基準の木に無かった。** 移動を追わない同一性では、移動したもの(2 件)も対応しない。
- **正解の「refactored」は「変更された」である。** 範囲の中の変更をすべて数えると、基準の同一性の 4 分の 1 が入り、precision の期待値が高くなる。3 コミット以上の版を足したのは、この影響を見るためで、閾値 3 に根拠は無い。
- **`index` の初回の実行は、計測中に `run.sh` を書き換えたため、終わりで終了コード 2 を返した**(sh は実行中のスクリプトを少しずつ読む)。索引そのものは書けていて(rust-analyzer の行は成功)、以後の計測はその索引を使った。

## 8. 入口 `run.sh`

`duckdb-ext/sql/run.sh` は POSIX sh で、Horizon のチェックアウトのどこから呼んでもそのルートに移って動く。

```sh
duckdb-ext/sql/run.sh index REPO                      # SCIP 索引と jscpd の報告
duckdb-ext/sql/run.sh query REPO unit_churn LIMIT=20  # queries/unit_churn.sql を表で
duckdb-ext/sql/run.sh query REPO cochange_without_reference min_coupling_pct=50 FORMAT=box
duckdb-ext/sql/run.sh sql REPO FILE.sql [NAME=VALUE ...]
duckdb-ext/sql/run.sh shell REPO
```

`query`・`sql`・`shell` は `repo` と与えた変数を設定し、README の順にビューのファイルを読み(索引があれば `views/scip.sql` と `views/graph.sql` も)、`refresh.sql` を読んでから、クエリ・ファイル・対話に移る。読み込みの出力は捨てる。`LIMIT` はクエリを `SELECT * FROM (…) LIMIT n` で包む。依頼文は CLI の `.maxrows` と包む方法のどちらかを求めており、クエリのファイルを変えずにどの出力形式でも同じに効くので、包む方にした(`.maxrows` は試していない)。`NAME=VALUE` は数値・`true`・`false`・`NULL` ならそのまま、それ以外は文字列として渡し(`since=e3c619a0`)、`NAME:=EXPR` は SQL の式として渡す。誤りは 1 行で、何をすればよいかを書く(jscpd の報告が無い、索引が要るクエリで索引が無い、拡張が未ビルド、CLI が無い、jscpd が未導入)。

この記録の計測は、すべて `run.sh` を通した。`check.py` には後ろ向きの確認を足していない。`retro/changes.sql` と `retro/signals.sql` は変数で基準・範囲・出どころを受け取るので、別の範囲にもそのまま使える。

## 9. コマンドと時間

```sh
git clone -q --no-checkout . target/code-db/retro/base && git -C target/code-db/retro/base checkout -q e3c619a0
git clone -q --no-checkout . target/code-db/retro/end  && git -C target/code-db/retro/end  checkout -q 46c37290
duckdb-ext/sql/run.sh index target/code-db/retro/base
duckdb-ext/sql/jscpd.sh target/code-db/retro/end target/code-db/retro/end/target/code-db/jscpd src crates
R=$PWD/target/code-db/retro
duckdb-ext/sql/run.sh sql $R/end duckdb-ext/sql/retro/changes.sql since=e223bc20 retro_changes=$R/changes.parquet
duckdb-ext/sql/run.sh sql $R/base duckdb-ext/sql/retro/signals.sql \
    retro_changes=$R/changes.parquet retro_reviews=$R/end/scripts/refactor-audit/reviews.json \
    retro_record_preserves=$PWD/duckdb-ext/sql/retro/september-2026-record-preserves.csv \
    retro_record_targets=$PWD/duckdb-ext/sql/retro/september-2026-record-targets.csv \
    retro_ranks_out=$R/ranks.csv retro_scope=production retro_min_commits=1
```

| 段階 | 時間 | 最大 RSS |
| --- | ---: | ---: |
| 基準の jscpd(2 つの報告) | 0.7 s | |
| 基準の `rust-analyzer scip`(索引 23 MB) | 226.8 s | 6.33 GB |
| 終わりの複製の jscpd | 0.8 s | |
| 終わりの `changes.sql`(キャッシュの初回の構築、`since = e223bc20` を含む) | 22.2 s | 3.16 GB |
| 基準のキャッシュの初回の構築(全履歴、`query` 1 回分) | 18.9 s | 6.34 GB |
| 基準のキャッシュの上のクエリ 1 つ | 2.3 s | 0.62 GB |
| `signals.sql`(4 つの範囲と閾値の組で各 1 回) | 2.6〜2.9 s | 1.0〜1.1 GB |

基準のキャッシュは 449 MB、書き出した blob は 117 MB だった。時間は `run.sh` を包んだ Python の `resource.getrusage(RUSAGE_CHILDREN)` で取った 1 回ずつの値で、`nice -n 19` で動かした。
