# 行の追跡で今の unit の履歴を作れるかの事前の確かめ(2026-10-29)

コードデータベースの変更の候補についての、既存のデータだけを使った測定の記録である。実装はしていない。今の `unit_history`(blob の層、つまり過去のファイルの内容をコミットごとに unit に分けたものと、その上の系譜のビュー)が与える今の unit の履歴を、`git log -L` のように今の unit の行の範囲を hunk に沿って過去へたどる方法でどこまで再現できるかを、unit ごとに比べた。

## オーナーの発言と前提

依頼文によれば、依頼側が blob の層の役割を説明したあと、オーナーは「そもそもblobを対象にしなくてよいのではって寄り」と述べ、この事前の確かめを「はい。お願いします。」と承認した(2026-10-29)。依頼文が示した考えは次のとおりである。品質とは今のコードを保守するうえでの障害の少なさなので、履歴は今の unit の過去としてだけ要る。比較の前の側や後ろ向きの検証の基準の木のような、少数の過去のコミットの姿は、そのコミットの SCIP の索引から得られる。この確かめは、行の追跡が今の `unit_history` をどこまで再現するか、とくに系譜(7 番目と 12 番目の増分の `file_rename`、`content_match`、`body_move`、`inherited`)が名前とファイルをまたいで本体をつないでいるところで何を失うかを決めるためのものである。

測定はすべて `origin/code-quality-survey` の 4845212d の使い捨ての複製の上で行い、`duckdb-ext/` は変えていない。推奨は書かない。

## 主要な観察

- **標本 65 個(関数・メソッド 55、型・module・impl 10)で、`unit_history` の 730 コミットのうち 474 が `git log -L` と一致し、256(35.1 %)は `-L` に無かった。`-L` だけにあるコミットは 20 だった。** 集めた Jaccard 係数は 0.632、unit ごとの平均は 0.665、2 つの集合が一致した unit は 25 個である(5 節)。
- **`-L` に無かった 256 のうち 243(標本の `unit_history` の 33.3 %)は、系譜が別のファイルへつないだ移動の先にあった。** 元のファイルが消えた移動(ファイルの分割)112、元のファイルが残った抽出の `content_match` 93、別のファイルへの `body_move` 21、git が元のファイルを別のファイルへの改名と判定した移動 17 である。残りは、同じファイルの中での改名を伴う `body_move` 6、名前は続くが `-L` の行がそのコミットで新しく書かれたことになるもの 4、前身の行のうち今の範囲に残らなかった行への変更 2、`-L` が別の経路で前身に達したもの 1 だった(4 節)。
- **`inherited` のリンクと `file_rename` のリンクの先のコミットは、標本では 1 つも失われなかった。** `-L` はファイルの改名を自分でたどり(`--follow` は `-L` には無く、要らない)、impl の改名で触れられなかった method の行はそのまま続くからである。`AgentdState.new` の 15 → 5 と `AgentdHandle.begin_reload` の 2 → 1 は、`inherited` ではなく、それぞれファイルの分割(0d72fc00)と、同じファイルの中で行が別の impl の写しに対応づけられたこと(a4fd9605)で失われた(4 節)。
- **今の unit 9,427 個すべてに `-L` を走らせると、テストでない関数・メソッド 3,518 個の `unit_history` 9,407 コミットのうち 2,043(21.7 %)が `-L` に無く、そのうち 1,839 が別のファイルへの移動の先だった(自動の分類)。** 標本の割合が高いのは、層の作り方が churn の上位と他ファイルの系譜を厚く取ったためである(5 節)。
- **数の順位は、標本で Spearman 0.743(関数・メソッドで 0.720)、テストでない関数・メソッド全体で 0.666 だった。** 全体の上位 10 件の重なりは 6/10、上位 100 件は 60/100 である。`run_session` は 1 位から 30 位、`paint_terminal` は 27 位から 4,216 位、`ToolSessionBuilder.with_root` は 35 位から 4,576 位に下がる(5 節)。
- **`-L` だけにあるコミット(標本 20)は、多くが `-L` の行の写像が別のコードの旧い行へ続いたものだった(13)。** ほかは、系譜の閾値に届かなかった改名を `-L` がたどったもの 2、`body_move` の残り殻で系譜が意図して切った履歴を `-L` が名前の残った行でたどったもの 2、差分が行を別の名前の unit の旧い行に対応づけたため、データベースが unit にコミットを 1 つも記録しないもの 3 である(4 節)。
- **型・module・impl を 1 つの範囲で追うと、中の unit の変更をすべて数える。** テストでない囲む unit で `-L` だけにあるコミット 1,658 のうち 1,186 は `attribution := 'all'` の `unit_history` にあった。自分の行(頭と閉じ括弧など)を複数の `-L` に分けて渡すと、`-L` だけのコミットの多い impl 12 個で Jaccard は 0.04 から 0.47 になった(8 節)。
- **`-L` の 1 回は平均 11.6 ms で、9,427 個すべてを 1 本ずつ順に 109.8 s で走らせた。** `-L` は `-M`・`-C` を受け付けるが結果は変わらなかった。`git blame -C -w` は他ファイルへの移動で失われたコミットの一部(4 つの unit で 7〜16 個)を 0.02〜0.10 s で返した(6、7 節)。

## 1. 手順

### 複製と準備

- 作業ツリーの `target/code-db/repo` に、このリポジトリを `git clone --no-checkout` し、4845212d を取り出した(コミット 1,639、マージを除いて 1,296)。複製の HEAD がそのまま測定の範囲の終わりである。
- 複製の中で `duckdb-ext/build.sh` を `nice -n 19` で走らせた(拡張 2 つと `code-db`)。
- `code-db query` は jscpd の報告を要求するので、`duckdb-ext/sql/jscpd.sh` を複製に対して直接走らせた(`JSCPD` に別の作業ツリーの jscpd 5.3.1 を指定)。SCIP の索引は作っていない。使ったクエリはどれも索引を要らない。
- 永続のキャッシュは、最初の `code-db query REPO unit_churn` で範囲の全体について作られた(既定の `include_merges` false、`attribution` innermost。そのクエリ全体で 22.6 s)。
- `code-db sql` で次をすべて CSV に書き出した:`units`(`tags` の `test` から `unit_tests` と同じ規則でテストかどうか)、`unit_history`、`unit_lineage_segments` を `unit_changes` に結んだ「どの同一性がどのコミットを渡したか」、`unit_lineage`、`unit_successions`、`commit_order`、`unit_changes`、`commits`、`hunks`。同じものを `attribution=all` でも書き出した。

`unit_history` の列は `unit_id, sha, change_kind, depth, via_link` で、マージのコミットは含まれていなかった(既定のとおり)。今の unit は 9,427 個(テスト 3,948)で、`unit_history` は 21,859 行である。前の記録の 9,264 個より多いのは、HEAD が後だからである。

### 標本

乱数の種は 20261029(Python の `random.Random`)。`unit_churn` の順(コミット数の降順、行数の降順、path、開始行)で全 unit に順位を付け、テストでない unit から引いた。

- 指定の 5 個:`crates/horizon-agent/src/tools/catalog.rs` の `build_definitions`、`crates/horizon-agentd/src/session/run.rs` の `run_session`、`src/runtime/mod.rs` の `AgentdHandle.begin_reload` と `AgentdHandle.stop_and_wait`、`AgentdState.new`。
- 他ファイルの系譜の 3 個:`unit_history` のうち、深さ 1 以上で今の path と違うファイルの同一性が渡したコミットの数が最も多い 3 個(テストを除く)。この数え方の 1 位は `run_session`(30)で、指定に入っているので次の 3 個を取った:`spawn_rig_session`(22)、`paint_terminal`(22)、`ToolSessionBuilder.with_root`(21)。この数え方はファイルの改名の先も含む。
- 関数・メソッドから、順位 1〜100 で 15、101〜1,000 で 15、1,001 以降でコミット 1 以上から 15、コミット 0 から 3(テストでない関数・メソッドでコミット 0 は 2 個しかなく、2 個になった)。
- 型・module・impl から、順位 1〜1,000 で 3、1,001 以降でコミット 1 以上から 3、コミット 1 以上の全体から 2、impl だけから 2。順位で層を分けた 8 個には impl が入らなかったので、impl の 2 個を足した。囲む unit は 8 節で別に扱う。

### `git log -L`

各 unit について、複製で次を走らせた。

```sh
git log -L<lead_start_line>,<end_line>:<path> --no-merges --format=%H --no-patch 4845212d
```

`lead_start_line` は先行する doc コメントと属性の始まりで、innermost の帰属が unit に数える範囲の始まりと同じである。先行する doc コメントのある 37 個では `[start_line, end_line]` の変種も走らせた。

### 比べ方と分け方

unit ごとに、`unit_history` のコミットの集合(UH)と `-L` の集合を比べ、両方、UH だけ、`-L` だけ、Jaccard を数えた。

UH だけのコミットは、まず自動の規則で分けた。各コミットを渡した同一性に、今の unit から `unit_successions`(`same_name` 以外)をさかのぼる連鎖を Python で組み直し(`unit_lineage_segments` の窓と `body_move` の切れ目の規則を写した)、最も短い連鎖を取った。連鎖のリンクを新しい方から見て、そのリンクのコミットが `-L` の最も古いコミットより古くないもの(`-L` がその下へ届いていないもの)を壁とし、その種類と、リンクの前身と後継の path が違うときは `git show -M --name-status` での前身のファイルの扱い(削除、残る、改名)で分類した。壁が別のファイルへのリンクでないもの(同じファイルの中のリンクの 6 と、壁の無い 7)は、手で `-L` のパッチ出力とコミットの差分を読んで分けた。`-L` だけのコミット(20)は、すべて `-L` のパッチ出力と、そのコミットのそのファイルの `unit_changes`(両方の帰属)を読んで分けた。

## 2. `git log -L` の挙動の確かめ

- **ファイルの改名をたどる。** `run_session` の `-L` は 24 コミットのうち 3 つを `crates/horizon-sessiond/src/session/run.rs` で返した(b293051a のディレクトリの改名の前)。`AgentdHandle.begin_reload` の `-L` は a4fd9605 を `src/sessiond/mod.rs` で返した(8d8cdd0e の改名の前)。`git log` の文書(git 2.55.0)は `-L` に `--follow` を挙げず、パス指定も許さない。改名は追加の指定なしにたどられた。
- **`-M`・`-C` は効かない。** `-L` に `-M`、`-C`、`-C -C`、`-C -C -C` を付けてもエラーにならず、4 つの unit で集合はどれも付けないときと同じだった(7 節)。文書も `-L` に写しの検出を書いていない。
- **`--no-merges`** は表示からマージを除くだけで、追跡はマージを通って続く。`unit_history` もマージを含まないので、どちらもマージの以外のコミットを比べている。
- **`-L` の差分は git 自身のもの(xdiff、既定の Myers と indent heuristic)で、データベースの hunk は gix の `-U0` Myers である。** 複製の git 設定に `diff.algorithm` などの上書きは無かった。
- **先行する doc を除いた範囲**(`[start_line, end_line]`)では、37 個のうち 7 個で集合が変わり、どれも小さくなった。UH と一致するコミットは 283 から 270 に減り、増えたものは無い。doc だけの変更は innermost の帰属が unit に数えるので、比べるのは `lead_start_line` からの範囲の方である。以下の数はすべてそちらである。

## 3. 標本の表

「順位」は `unit_churn` の順の全 9,427 個の中の順位、UH は `unit_history` のコミット数である。内訳の記号は 4 節の分類である。

| 層 | 種類 | path | unit | 順位 | UH | -L | 両方 | UH のみ | -L のみ | Jaccard | UH のみの内訳 | -L のみの内訳 |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- |
| 指定 | function | `crates/horizon-agent/src/tools/catalog.rs` | `build_definitions` | 2 | 49 | 43 | 43 | 6 | 0 | 0.88 | B4 G2 |  |
| 指定 | function | `crates/horizon-agentd/src/session/run.rs` | `run_session` | 1 | 51 | 24 | 24 | 27 | 0 | 0.47 | A27 |  |
| 指定 | method | `src/runtime/mod.rs` | `AgentdHandle.begin_reload` | 3415 | 2 | 1 | 1 | 1 | 0 | 0.50 | F1 |  |
| 指定 | method | `src/runtime/mod.rs` | `AgentdHandle.stop_and_wait` | 3858 | 2 | 1 | 1 | 1 | 0 | 0.50 | F1 |  |
| 指定 | method | `crates/horizon-agentd/src/session/state.rs` | `AgentdState.new` | 76 | 15 | 5 | 5 | 10 | 0 | 0.33 | A10 |  |
| 他ファイル | function | `crates/horizon-agent/src/providers/rig/session/mod.rs` | `spawn_rig_session` | 9 | 34 | 12 | 12 | 22 | 0 | 0.35 | A22 |  |
| 他ファイル | function | `src/terminal/paint.rs` | `paint_terminal` | 27 | 23 | 1 | 1 | 22 | 0 | 0.04 | B22 |  |
| 他ファイル | method | `crates/horizon-agent/src/tools/state/builder.rs` | `ToolSessionBuilder.with_root` | 35 | 22 | 1 | 1 | 21 | 0 | 0.05 | C21 |  |
| 1–100 | method | `src/agent/session.rs` | `AgentSession.new_attachment` | 54 | 17 | 17 | 17 | 0 | 0 | 1.00 |  |  |
| 1–100 | function | `crates/horizon-cli/src/commands.rs` | `external_name` | 79 | 15 | 15 | 15 | 0 | 0 | 1.00 |  |  |
| 1–100 | function | `crates/horizon-agent/src/tools/explore.rs` | `fold_until_terminal` | 48 | 17 | 17 | 17 | 0 | 0 | 1.00 |  |  |
| 1–100 | method | `src/terminal/keyboard.rs` | `TerminalView.handle_key` | 84 | 14 | 1 | 1 | 13 | 0 | 0.07 | B13 |  |
| 1–100 | function | `crates/horizon-agent/src/frame/fold.rs` | `apply_agent_event_to_frame` | 15 | 29 | 12 | 12 | 17 | 0 | 0.41 | D17 |  |
| 1–100 | function | `crates/horizon-cli/src/commands.rs` | `to_request` | 38 | 21 | 21 | 21 | 0 | 0 | 1.00 |  |  |
| 1–100 | function | `crates/horizon-agentd/src/main.rs` | `main` | 16 | 28 | 28 | 28 | 0 | 0 | 1.00 |  |  |
| 1–100 | function | `crates/horizon-agent/src/tools/bash/job.rs` | `spawn_host` | 47 | 18 | 6 | 6 | 12 | 0 | 0.33 | B12 |  |
| 1–100 | method | `src/terminal/session.rs` | `TerminalSession.spawn` | 10 | 34 | 32 | 32 | 2 | 0 | 0.94 | B2 |  |
| 1–100 | method | `src/workspace/render.rs` | `Render for WorkspaceShell.render` | 8 | 34 | 21 | 21 | 13 | 0 | 0.62 | A13 |  |
| 1–100 | function | `crates/horizon-cli/src/cli.rs` | `parse` | 37 | 21 | 21 | 21 | 0 | 0 | 1.00 |  |  |
| 1–100 | function | `crates/horizon-agent/src/providers/rig/completion.rs` | `run_provider_stream` | 7 | 35 | 32 | 32 | 3 | 0 | 0.91 | B3 |  |
| 1–100 | function | `crates/horizon-agent/src/transcript/tool_call/view.rs` | `build_tool_call_views` | 44 | 19 | 8 | 8 | 11 | 0 | 0.42 | A11 |  |
| 1–100 | function | `crates/horizon-agent/src/tools/processing.rs` | `process_agent_provider_event` | 17 | 27 | 23 | 23 | 4 | 0 | 0.85 | B4 |  |
| 1–100 | method | `crates/horizon-agent/src/registry.rs` | `ProviderRegistry.builtin_with_config` | 46 | 18 | 8 | 8 | 10 | 0 | 0.44 | B10 |  |
| 101–1000 | function | `crates/horizon-wire/src/spawn.rs` | `resolve_daemon_binary` | 827 | 5 | 8 | 5 | 0 | 3 | 0.62 |  | a1 b2 |
| 101–1000 | method | `src/terminal/pointer.rs` | `TerminalView.handle_mouse_up` | 820 | 5 | 1 | 1 | 4 | 0 | 0.20 | B4 |  |
| 101–1000 | function | `crates/horizon-agent/src/transcript/tool_call/view.rs` | `running_row_expandable` | 693 | 6 | 2 | 2 | 4 | 0 | 0.33 | A4 |  |
| 101–1000 | method | `crates/horizon-workspace/src/types/entity.rs` | `Pane.new` | 662 | 6 | 2 | 2 | 4 | 0 | 0.33 | B4 |  |
| 101–1000 | function | `crates/horizon-agent/src/tools/approval.rs` | `resolve_git_operation` | 372 | 7 | 7 | 7 | 0 | 0 | 1.00 |  |  |
| 101–1000 | function | `crates/horizon-agentd/src/session/completion.rs` | `fold_tool_completion` | 104 | 13 | 8 | 8 | 5 | 0 | 0.62 | A5 |  |
| 101–1000 | function | `crates/horizon-sandbox/src/linux/mod.rs` | `execute_supervised_helper` | 538 | 6 | 6 | 6 | 0 | 0 | 1.00 |  |  |
| 101–1000 | function | `src/runtime/agent.rs` | `handle_op` | 118 | 12 | 14 | 12 | 0 | 2 | 0.86 |  | a2 |
| 101–1000 | method | `crates/horizon-agent/src/providers/rig/session/turn.rs` | `SessionLoopState.halt_turn_loop` | 425 | 7 | 7 | 7 | 0 | 0 | 1.00 |  |  |
| 101–1000 | method | `crates/horizon-agent/src/persistence/event_log/writer.rs` | `WriterHandle.flush` | 212 | 10 | 8 | 8 | 2 | 0 | 0.80 | B2 |  |
| 101–1000 | function | `crates/horizon-agentd/src/session/approval.rs` | `begin_reissued_approval` | 392 | 7 | 6 | 6 | 1 | 0 | 0.86 | A1 |  |
| 101–1000 | method | `crates/horizon-agent/src/providers/rig/session/interaction.rs` | `SessionLoopState.begin_interaction` | 854 | 5 | 5 | 5 | 0 | 0 | 1.00 |  |  |
| 101–1000 | function | `crates/horizon-agent/src/frame/queries.rs` | `pending_approval_call_ids_in` | 114 | 13 | 6 | 6 | 7 | 0 | 0.46 | E6 F1 |  |
| 101–1000 | function | `src/terminal/diagnostics.rs` | `dump_frame` | 518 | 6 | 1 | 1 | 5 | 0 | 0.17 | B5 |  |
| 101–1000 | method | `crates/horizon-agentd/src/session/connection.rs` | `Connection.session_list` | 319 | 8 | 2 | 2 | 6 | 0 | 0.25 | A6 |  |
| 1001– | method | `crates/horizon-agentd/src/hub.rs` | `Shared.command` | 6790 | 1 | 9 | 1 | 0 | 8 | 0.11 |  | a8 |
| 1001– | function | `duckdb-ext/git-history/src/functions.rs` | `strings` | 5970 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 1001– | method | `crates/horizon-workspace/src/persistence.rs` | `PaneKindState.expected_session_kind` | 7139 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 1001– | method | `crates/horizon-agentd/src/hub/attachment.rs` | `Asks.ask` | 7759 | 1 | 2 | 1 | 0 | 1 | 0.50 |  | a1 |
| 1001– | method | `crates/horizon-agentd/src/hub.rs` | `Shared.continue_turn` | 3193 | 2 | 2 | 2 | 0 | 0 | 1.00 |  |  |
| 1001– | method | `src/board/live.rs` | `LineSource for SubscribeStream.next_line` | 2268 | 3 | 1 | 1 | 2 | 0 | 0.33 | B1 H1 |  |
| 1001– | function | `src/agent/view/units.rs` | `card` | 1367 | 4 | 6 | 4 | 0 | 2 | 0.67 |  | c2 |
| 1001– | method | `crates/horizon-agent/src/tools/explore/worker.rs` | `ChildWork.finish` | 8392 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 1001– | function | `crates/horizon-agent/src/tools/bash/recent.rs` | `base_command` | 5301 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 1001– | function | `crates/horizon-agentd/src/board_flow/dispatch.rs` | `handle_event` | 2499 | 2 | 2 | 2 | 0 | 0 | 1.00 |  |  |
| 1001– | function | `crates/horizon-sandbox-runtime/src/execute.rs` | `become_child_subreaper` | 6073 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 1001– | function | `crates/horizon-agent/src/tools/bash/registry.rs` | `lock` | 3726 | 2 | 3 | 2 | 0 | 1 | 0.67 |  | a1 |
| 1001– | method | `crates/horizon-agentd/src/session/board.rs` | `BoardHost for AgentdBoardHost.list` | 2256 | 3 | 3 | 3 | 0 | 0 | 1.00 |  |  |
| 1001– | function | `src/terminal/paint.rs` | `paint_row_items` | 2679 | 2 | 1 | 1 | 1 | 0 | 0.50 | B1 |  |
| 1001– | function | `src/agent/turns/mod.rs` | `pluralize` | 2178 | 3 | 2 | 2 | 1 | 0 | 0.67 | A1 |  |
| 0 件 | method | `src/runtime/mod.rs` | `TerminaldHandle.stop_and_wait` | 9427 | 0 | 1 | 0 | 0 | 1 | 0.00 |  | d1 |
| 0 件 | method | `src/runtime/mod.rs` | `TerminaldHandle.broadcast_terminal_color_scheme` | 9426 | 0 | 2 | 0 | 0 | 2 | 0.00 |  | d2 |
| 型等 1–1000 | type | `crates/horizon-agent/src/tools/processing.rs` | `Processing` | 967 | 5 | 3 | 3 | 2 | 0 | 0.60 | B2 |  |
| 型等 1–1000 | type | `crates/horizon-agent/src/tools/state.rs` | `RecallContext` | 1000 | 5 | 5 | 5 | 0 | 0 | 1.00 |  |  |
| 型等 1–1000 | type | `src/agent/view/mod.rs` | `AgentView` | 42 | 20 | 7 | 7 | 13 | 0 | 0.35 | A12 F1 |  |
| 型等 1001– | module | `src/workspace/mod.rs` | `modals` | 9414 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 型等 1001– | type | `duckdb-ext/scip-index-core/src/lib.rs` | `Range` | 7527 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 型等 1001– | type | `crates/horizon-sandbox/src/policy.rs` | `UnixSocketConnectGrant` | 6612 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| 型等 任意 | type | `src/terminal/scrollback.rs` | `VisibleScrollback` | 984 | 5 | 1 | 1 | 4 | 0 | 0.20 | B4 |  |
| 型等 任意 | module | `duckdb-ext/git-history-core/src/lib.rs` | `blobs` | 9227 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| impl | impl | `src/theme_settings/seed.rs` | `HueSlot` | 4549 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |
| impl | impl | `src/preview/host.rs` | `PreviewHostRoot` | 5987 | 1 | 1 | 1 | 0 | 0 | 1.00 |  |  |

標本の型・module・impl 10 個は、中に unit を持たない struct、`mod x;` の宣言、中の unit の無い impl だけだった。中の unit を持つ囲む unit の比較は 8 節にある。

## 4. 片側だけのコミットの分類

### `unit_history` にあり `-L` に無いもの(256)

| 記号 | 分類 | 関数・メソッド | 型等 | 計 | 標本の `unit_history` に占める割合 |
| --- | --- | ---: | ---: | ---: | ---: |
| A | 別のファイルへの `content_match`、元のファイルは削除 | 100 | 12 | 112 | 15.3 % |
| B | 別のファイルへの `content_match`、元のファイルは残る | 87 | 6 | 93 | 12.7 % |
| C | 別のファイルへの `body_move`、元のファイルは残る | 21 | 0 | 21 | 2.9 % |
| D | 別のファイルへの `content_match`、git は元のファイルを別のファイルへの改名と判定 | 17 | 0 | 17 | 2.3 % |
| E | 同じファイルの中の、改名を伴う `body_move` | 6 | 0 | 6 | 0.8 % |
| F | 同一性は続くが、`-L` の行はそのコミットで新しく書かれたことになる | 3 | 1 | 4 | 0.5 % |
| G | 前身の、今の範囲に残らなかった行への変更 | 2 | 0 | 2 | 0.3 % |
| H | `-L` が別の経路で前身に達し、系譜のリンクのコミットを通らない | 1 | 0 | 1 | 0.1 % |
| 計 | | 237 | 19 | 256 | 35.1 % |

`inherited`、`file_rename`、同じファイルの中の `content_match` を壁とするものは標本に無かった。

- **A(元のファイルが消えた移動)。** ファイルをディレクトリに分けたコミットである。`run_session` の 27 と `AgentdState.new` の 10、`Connection.session_list` の 6 などは 0d72fc00(`crates/horizon-sessiond/src/session.rs` を削除し、`session/run.rs`、`session/state.rs` など 11 ファイルを追加)、`spawn_rig_session` の 22 は faeccb05(`providers/rig/session.rs` → `session/mod.rs`)、`Render for WorkspaceShell.render` の 13 は 7b588c08(`src/workspace.rs` → `src/workspace/render.rs`)、`AgentView` の 12 は 7441ae10(`src/agent/view.rs` → `src/agent/view/mod.rs`)。どれも git は元のファイルを削除と判定し(改名の相手を選ばない)、`-L` はそのコミットで範囲の全体が新しいファイルに追加されたとして止まる(0d72fc00 の `-L` の差分は `new file mode`、`@@ -0,0 +40,280 @@`)。
- **B(元のファイルが残る抽出)。** `paint_terminal` の 22、`TerminalView.handle_key` の 13、`dump_frame` の 5、`TerminalView.handle_mouse_up` の 4、`paint_row_items` の 1 は 1b15a6a5(`src/terminal/mod.rs` から `paint.rs`、`keyboard.rs`、`pointer.rs`、`diagnostics.rs` を切り出し、`mod.rs` は 1,280 行の変更で残る)。`spawn_host` の 12 は beaa9d6b、`ProviderRegistry.builtin_with_config` の 10 は ee77f42b(`contract.rs` → `registry.rs`)、`build_definitions` の 4 は a6b0c63f(`src/agent/tools/mod.rs` → `catalog.rs`)とその前の連鎖。
- **C(別のファイルへの `body_move`)。** すべて `ToolSessionBuilder.with_root` の 4f65ca21(`ToolSessionState.with_root` の本体が `tools/state/builder.rs` の `ToolSessionBuilder.with_root` に移り、名前は `state.rs` に残った)。
- **D(git の改名の判定が別のファイルを選んだ)。** すべて `apply_agent_event_to_frame` の f04c7bf1。git は `crates/horizon-agent/src/frame.rs` を `frame/queries.rs` への改名(R052)と判定し、関数は `frame/fold.rs` に移った。`-L` は `fold.rs` の範囲をそのコミットでの追加として止まる。
- **E(同じファイルの中の `body_move`)。** すべて `pending_approval_call_ids_in`。`crates/horizon-agent/src/frame.rs` の中の 728d17fe と 18bc166d の `body_move` で、`-L` の差分は 728d17fe で範囲を追加(`@@ -172,0 +155,10 @@`)とし、それより前に届かない。
- **F(名前は続き、行は新しくなる)。** `AgentdHandle.begin_reload` と `stop_and_wait` の 67cffe45。a4fd9605 で `src/sessiond/mod.rs` に `TerminaldHandle` が加わり、git の差分は `SessiondHandle.begin_reload` の旧い行を新しい `TerminaldHandle` の写しに対応づけ、`SessiondHandle` の側を追加とした(`-L` の差分は `@@ -324,0 +379,13 @@`)。データベースは同じ名前の同一性として 67cffe45 からの履歴を続ける。`AgentView` の b23b269c は、3f908482 で struct が同じファイルの中の別の位置に書き直された(`-L` の差分は `@@ -353,0 +148,8 @@`)。`pending_approval_call_ids_in` の c2762367 は、のちの書き換えで `-L` の過去の範囲が関数の最初の行までに縮み(4733ea35 の `-L` の差分は `let mut pending` の行で終わる)、c2762367 が変えた本体の行が範囲の外になった。
- **G(残らなかった行)。** `build_definitions` の b11f2d91 と b656cc27。どちらも `definitions` の署名の行(`pub(crate) fn definitions()` など)を変えた。cf9a2daa の `body_move` で `definitions` の本体は `build_definitions` に移ったが、署名の行は残り殻の `definitions` に残った。系譜は前身の同一性のコミットをすべて渡し、`-L` は今の範囲に続く行のコミットだけを返す。
- **H(別の経路)。** `LineSource for SubscribeStream.next_line` の f224263a。系譜は f224263a の `content_match`(`src/board_pane/live.rs` → `src/board_next/live.rs`、別の枝でのコミット)を通るが、`-L` は 34223101 の改名(`board_next/live.rs` → `board/live.rs`、R100)の後、マージ 4431743c を経て `board_pane/live.rs` の 8e057dd7 に達し、f224263a を返さなかった。マージで `-L` がどの親とどのファイルを選んだかの仕組みは確かめていない。

### `-L` にあり `unit_history` に無いもの(20)

| 記号 | 分類 | 数 | 例 |
| --- | --- | ---: | --- |
| a | `-L` の行の写像が、そのコミットで書かれた範囲の外の旧い行へ続いた | 13 | `Shared.command` 8、`handle_op` 2、`Asks.ask` 1、`lock` 1、`resolve_daemon_binary` 1 |
| b | 系譜の閾値に届かなかった改名を `-L` がたどった | 2 | `resolve_daemon_binary` の 4e7fb81e、a5bbe2f6 |
| c | `body_move` の残り殻で、系譜が切った履歴を `-L` が名前の残った行でたどった | 2 | `card` の 24653bcc、a03d3208 |
| d | 差分が行を別の名前の unit の旧い行に対応づけ、データベースはその unit にコミットを記録しない | 3 | `TerminaldHandle.stop_and_wait` 1、`TerminaldHandle.broadcast_terminal_color_scheme` 2 |

attribution の違い(空行、先行する doc、中の unit)で説明できるものは、標本の 20 には無かった(どれも `attribution=all` の `unit_history` にも無い)。

- **a。** `Shared.command` は c6cf00c8 で追加された 10 行の関数だが、`-L` はその前に 8 コミットを返した。普通の差分では、その 10 行は `@@ -56,16 +81,162 @@` の置換の hunk の中にあり、`-L` はその旧い側(`SessionHub for Hub.hello` の版の取り決めの行)を範囲の過去として追った。`handle_op` は 7ce390cb で追加されたが(`-L` の表示は `@@ -418,0 +500,114 @@`)、`-L` は `src/sessiond/connection.rs` の `handshake` の行の 3cf63464 と 67cffe45 を返した。`Asks.ask` は `start` の行、`lock` は `kill_process_group` の doc コメントの行、`resolve_daemon_binary` の fc6fe5cf は `spawn_agentd` の末尾の 2 行に続いた。旧い側の hunk 全体を範囲に含める写像によるものか、書き換えの中の `}` などの共通の行の対応づけによるものかは、例ごとに分けていない。母集団で読んだ 1 例(`status_output`、`crates/horizon-agent/src/tools/bash/exec.rs`)では、過去の範囲が 511 行(隣の `run_sandboxed` の全体)に広がっていた。
- **b。** 4e7fb81e で `crates/horizon-agent/src/client.rs` の `resolve_agentd_binary` が `resolve_sessiond_binary` になった。名前と doc コメントの多くの行が変わり、`unit_successions` にこの組のリンクは無い。`-L` は行の対応をたどって、その前の a5bbe2f6(`src/agent/agentd_client.rs` での追加)まで届いた。
- **c。** 3811c017 で `src/agent/next/units.rs` の `card` の本体が `frame` に移った(`body_move`、0.636)。オーナーの決定(履歴は本体を追う)どおり、系譜は残り殻の `card` を 3811c017 から始め、`-L` は名前とともに残った行をたどってその前の 2 コミットを返す。
- **d。** 前の記録(`code-db-inherited-lineage-2026-10-27.md` 2 節)が履歴の空の unit として残した 2 つである。a4fd9605 で `TerminaldHandle` の 2 つの method が `src/sessiond/mod.rs` に入ったとき、差分はその行を `SessiondHandle` の同じ内容の旧い行に対応づけたので、hunk がこの 2 つの unit の行に触れず、`unit_changes` には追加の行すら無い。`-L` は対応づけられた旧い行をたどり、67cffe45(`SessiondHandle.stop_and_wait` の追加)、123b6a03 と 7ce390cb(`SessiondHandle.broadcast_terminal_color_scheme` の追加と変更)を返した。F の `AgentdHandle.begin_reload` と `stop_and_wait` は、同じ対応づけの反対の側である。前の記録の「8d8cdd0e の `file_rename` だけが系譜にあり、改名前の同一性にも変更が無い」と矛盾はせず、変更が無い理由がこの対応づけだった。

## 5. 集計

### 標本

| 範囲 | unit | UH | -L | 両方 | UH のみ | -L のみ | 集めた Jaccard | 平均 Jaccard | 一致した集合 | UH のうち失う割合 | Spearman |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 関数・メソッド | 55 | 689 | 472 | 452 | 237 | 20 | 0.638 | 0.638 | 18 | 34.4 % | 0.720 |
| 型・module・impl | 10 | 41 | 22 | 22 | 19 | 0 | 0.537 | 0.815 | 7 | 46.3 % | 0.845 |
| 全体 | 65 | 730 | 494 | 474 | 256 | 20 | 0.632 | 0.665 | 25 | 35.1 % | 0.743 |

Spearman は、標本の unit の UH の数と `-L` の数の順位相関(同順位は平均の順位)である。層ごとの集めた Jaccard は、指定 0.622、他ファイル 0.177、1〜100 位 0.755、101〜1,000 位 0.645、1,001 位以降 0.600、0 件 0.000、型等 1〜1,000 位 0.500、型等 1,001 位以降 1.000、型等任意 0.333、impl 1.000 だった。

### 母集団(今の unit すべて)

標本の割合は層の作り方に左右されるので、9,427 個すべてに同じ `-L` を走らせ、数を比べた。片側だけのコミットの分類は 1 節の自動の規則だけで、手では読んでいない。

| 範囲 | unit | UH | -L | 両方 | UH のみ(UH に占める割合) | -L のみ | 集めた Jaccard | 一致した集合 | Spearman | 上位 10 の重なり | 上位 100 の重なり |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| すべて | 9,427 | 21,859 | 22,602 | 17,745 | 4,114(18.8 %) | 4,857 | 0.664 | 6,014(63.8 %) | 0.629 | 1/10 | 47/100 |
| テストでない | 5,479 | 13,292 | 13,560 | 10,612 | 2,680(20.2 %) | 2,948 | 0.653 | 3,546(64.7 %) | 0.639 | 2/10 | 49/100 |
| テストでない関数・メソッド | 3,518 | 9,407 | 8,654 | 7,364 | 2,043(21.7 %) | 1,290 | 0.688 | 2,218(63.0 %) | 0.666 | 6/10 | 60/100 |
| テストでない型・module・impl・macro | 1,961 | 3,885 | 4,906 | 3,248 | 637(16.4 %) | 1,658 | 0.586 | 1,328(67.7 %) | 0.581 | 2/10 | 42/100 |

上位の重なりは、それぞれの数の降順(同数は unit_id の順)の上位 k 件の共通の数である。`-L` の数の上位 10 件のうち 5 件は impl(`TerminalSession` 46、`session_lifecycle.rs` の `WorkspaceShell` 44 など。UH はどれも 1〜5)で、8 節の囲む unit の問題がそのまま順位に出る。

テストでない関数・メソッドの UH だけの 2,043 の自動の分類:別のファイルへの `content_match` で元のファイルが残る 1,062(11.3 %)、元のファイルが削除 685(7.3 %)、git が別のファイルへの改名と判定 50、同じファイルへの改名と判定 5、別のファイルへの `body_move` 32、別のファイルへの `inherited` 5、同じファイルの中の `body_move` 15、`content_match` 6、`inherited` 5、`file_rename` 3、壁のリンクが無く `-L` の届く範囲の中 117、壁のリンクが無く `-L` の最も古いコミットより古い 58。別のファイルへの移動の合計は 1,839(UH の 19.5 %)である。`file_rename` を壁とした 13(型等の 10 を含む)は、どれも git も改名と判定したコミット(R072〜R099)で、自動の規則の近似によるものと考えるが、読んでいない(9 節)。

テストでない関数・メソッドの `-L` だけの 1,290 は、どれも `attribution=all` の UH にも無かった。そのうち 1,141 は unit の UH の最初のコミットより古く(`-L` が系譜の始まりより前へ続いた。標本の a、b、d の形)、88 は unit 自身の `body_move` の切れ目より前(c の形)、61 は UH の期間の中だった。

### 指定の例

| unit | UH(順位) | -L(順位) | 失うコミットと理由 |
| --- | --- | --- | --- |
| `build_definitions` | 49(2) | 43(3) | 6:B 4(a6b0c63f の `src/agent/tools/mod.rs` → `catalog.rs` と、その前の `src/agent/tools.rs`、`src/agent_tools.rs` の連鎖)、G 2 |
| `run_session` | 51(1) | 24(30) | 27:A(0d72fc00 のファイルの分割) |
| `AgentdHandle.begin_reload` | 2(3,415) | 1(6,544) | 1:F(a4fd9605)。8d8cdd0e の `inherited` は `-L` も越えた |
| `AgentdHandle.stop_and_wait` | 2(3,858) | 1(8,137) | 1:F(a4fd9605) |
| `AgentdState.new` | 15(76) | 5(869) | 10:A(0d72fc00)。8d8cdd0e の `inherited`(同じファイルの中の impl の改名)は `-L` も越えた |
| `spawn_rig_session` | 34(9) | 12(154) | 22:A(faeccb05) |
| `paint_terminal` | 23(27) | 1(4,216) | 22:B(1b15a6a5) |
| `ToolSessionBuilder.with_root` | 22(35) | 1(4,576) | 21:C(4f65ca21) |

## 6. 費用

- 標本の 65 回:平均 13.7 ms、中央値 12.8 ms、最大 27.9 ms。
- 9,427 個すべてを 1 本ずつ順に(`nice -n 19`、プロセスの起動を含む、同じ機械でページキャッシュは温まった状態):計 109.8 s、平均 11.6 ms、中央値 10.7 ms、99 パーセンタイル 28.7 ms、最大 36.9 ms。エラーは無かった。
- 囲む unit の自分の行を複数の `-L` で渡したもの(8 節):1 回 13〜22 ms。
- 比べるものとして、この複製のキャッシュの最初の作成を含む `unit_churn` の 1 回目は 22.6 s だった(blob の層を含む)。

1 回のパスで済ませる実装の見込み(推論だけで、測っていない)。キャッシュの `hunks`(gix の `-U0`、この範囲で 34,848 行)と `file_changes` の `old_path` があれば、今の各 unit の行の区間を、コミットを新しい方から 1 度だけ歩いて、そのファイルの hunk で写していける。仕事の量はコミットごとに「追っている区間を持つファイルの hunk の数」に比例し、unit ごとに履歴を歩き直す `git log -L` の繰り返しは無い。blob の構文解析も要らない。ただし、その結果は `git log -L` と同じにはならない。hunk の置き方は gix の Myers の `-U0` に従い(git の xdiff と indent heuristic ではない)、置換の hunk の旧い側をどこまで範囲に含めるか(4 節の a はこの規則で変わる)と、マージでどの親をたどるかを決める必要がある。

## 7. 別のファイルへの移動を取り戻せるか(手順 6)

別のファイルへの移動(A〜D)が失うコミットの大半なので、4 つの unit で確かめた。

| unit | UH | -L | 失う | `-L` に `-M`/`-C`/`-C -C`/`-C -C -C` | `blame -w` | `blame -C -w` | `blame -C -C -w` | `blame -C -C -C -w` |
| --- | ---: | ---: | ---: | --- | --- | --- | --- | --- |
| `paint_terminal` | 23 | 1 | 22 | どれも同じ 1、取り戻し 0 | 1(0)、0.01 s | 17(16)、0.08 s | 18(16)、0.38 s | 18(16)、0.54 s |
| `run_session` | 51 | 24 | 27 | どれも同じ 24、取り戻し 0 | 15(0)、0.01 s | 27(12)、0.10 s | 27(12)、0.26 s | 27(12)、0.69 s |
| `ToolSessionBuilder.with_root` | 22 | 1 | 21 | どれも同じ 1、取り戻し 0 | 1(0)、0.01 s | 16(15)、0.02 s | 16(15)、0.11 s | 16(15)、0.12 s |
| `AgentdState.new` | 15 | 5 | 10 | どれも同じ 5、取り戻し 0 | 5(0)、0.01 s | 13(7)、0.04 s | 13(7)、0.11 s | 13(7)、0.17 s |

blame の欄は、HEAD の同じ範囲の blame が返した異なるコミットの数(括弧は、そのうち `-L` が失ったコミット)と時間である。`git log -L` は写しの検出の指定を受け付けるが、4 つとも集合は変わらなかった。`git blame -C` は移動元のファイル(`src/terminal/mod.rs`、`crates/horizon-sessiond/src/session.rs`、`crates/horizon-agent/src/tools/state.rs` など)の行まで戻り、失ったコミットの 7〜16 個を返した。blame のコミットは 1 つを除いてすべて UH の中にあった(`paint_terminal` の `-C -C` では `spikes/gpui-terminal/src/main.rs` まで戻った 1 つ、`AgentdState.new` で 1 つが UH の外)。blame は行ごとに最後のコミットしか返さないので、その数は履歴の下限であり、行の変更の全体を得るには、blame が示す移動元の範囲から改めて `-L` をつなぐ必要がある(試していない)。

## 8. 型・module・impl(囲む unit)

innermost の帰属は、囲む unit に自分の行(頭、閉じ括弧、中の unit のあいだの行)だけを数える。`-L` の 1 つの範囲は中の unit の行も含むので、中の unit を持つ unit では比べられない。母集団のテストでない型・module・impl・macro で `-L` だけにある 1,658 のうち 1,186 は `attribution=all` の UH にあった。

`-L` は同じファイルの範囲を何本でも受け取る。HEAD の時点の自分の行(`[lead_start_line, end_line]` から中の直接の unit の `[lead_start_line, end_line]` を除き、空行を除いた連続した行の並び)を、それぞれ `-L` に渡して走らせた。`-L` だけのコミットの最も多いテストでない囲む unit 12 個(どれも impl で、自分の行は頭と閉じ括弧の 2 つの並びだった)の合計は次のとおりである。

| | コミット | UH と一致 | UH との集めた Jaccard |
| --- | ---: | ---: | ---: |
| UH | 25 | | |
| 1 つの範囲の `-L` | 392 | 16 | 0.04 |
| 自分の行の複数の `-L` | 22 | 15 | 0.469 |

自分の行に分けると数の桁はそろうが、一致は半分程度だった(`TerminalView` の impl では自分の行の `-L` だけが 3、UH だけが 3)。自分の行は HEAD での位置で、過去に中の unit の境界が動いたときの扱いは `-L` の写像に任せている。

## 9. 限界

- 1 つのリポジトリの 1 つの範囲(4845212d までの 1,639 コミット、マージを除いて 1,296)だけである。
- 自動の分類の壁は、「`-L` の最も古いコミットがリンクのコミットより古いか」で `-L` がそのリンクを越えたかを判断する近似である。4 節の a のように `-L` が別のコードへ続いて古いコミットに届くと、本当の壁が隠れる。標本では、壁が別のファイルへのリンクに当たったもの(A〜D、243)は、失ったコミットがすべて `-L` の最も古いコミットより古いことで確かめ、移動のコミットの差分は 0d72fc00、1b15a6a5、f04c7bf1 などで読んだ。壁が別のファイルへのリンクでない 13 と `-L` だけの 20 は手で読んだ。母集団の分類は読んでいない。
- 4 節の a の仕組み(置換の hunk の旧い側全体を範囲にするのか、共通の行の対応づけか)は例ごとに分けておらず、H のマージでの経路も確かめていない。
- `-L` は `--no-merges` で走らせ、マージ自身の差分は比べていない(`unit_history` の既定もマージを含まない)。
- 時間は共有のデスクトップでの 1 回の測定で、ページキャッシュは温まっていた。
- 標本の層は churn の上位と他ファイルの系譜を厚く取ったので、標本の割合を母集団の割合として読まない。母集団の数は 5 節の表にある。
- 囲む unit の自分の行の `-L`(8 節)は、`-L` だけのコミットの多い 12 個だけで、手で読んでいない。
