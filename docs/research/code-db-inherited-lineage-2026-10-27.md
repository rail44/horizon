# コードデータベースの系譜の継承の規則(2026-10-27)

コードデータベースの SQL 層の 12 個目の増分である。[クローンの側の規則の記録](code-db-clone-sides-2026-10-27.md)(以下「クローンの側の記録」)が残した 2 つの点について、`clone_change_consistency` の側の既定を `contain` に戻し、系譜に継承の規則(リンクの種類 `inherited`)を足し、fixture の確かめを足し、今の候補と後ろ向きの precision への効き方を測った。SQL と `check.py` だけを変え、Rust は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、依頼側が示した推奨(既定を `contain` に戻す、N2 の境は組んだままにする、継承の規則を足す)に、オーナーは 2026-10-27 に「はい。では作業をお願いします。」と答えた。

前提は、クローンの側の記録(`overlap` が precision を下げ、真の陽性を戻さなかったこと、impl の改名で行に触れられなかった method の系譜が切れ、33 個の unit の履歴が空だったこと)、[系譜の記録](code-db-lineage-2026-10-26.md)(4 つのリンクの規則とオーナーの 2 つの判断)、`duckdb-ext/sql/` の SQL と README である。記録の決定は問い直していない。

## 要約

- **`clone_sides` の既定を `contain` に戻した。** `overlap` は変数で残る。fixture は、既定が `contain` の値になることと、`overlap` の値の両方を確かめる。
- **継承の規則を `derived_unit_successions` に組んだ。** あるコミットの `content_match` か `body_move` の前身の内側の unit(前の側で、限定名が前身の限定名に `.` で続く unit)を、後継のパスと限定名に同じ相対名を付けた同一性につなぐ(`inherited`)。推測した同一性が後の側の unit にあること、どちらの同一性もそのコミットで自分の `content_match`・`body_move`・`file_rename` を持たないことを条件にした(1 節)。
- **HEAD の複製で、`unit_history` が空の unit は 33 → 3 になった。** 依頼側の事前の確かめ(33 → 6、27 個に 57 コミット)より 3 個と 8 コミット多い。違いは、2 段以上の入れ子で相対名を保つこと(3 個、5 コミット)と、つながった後の祖先が自分のリンクをさらにたどること(2 個、3 コミット)で説明できた(2 節)。事前の確かめの方法を写した SQL は、同じキャッシュで 27 個、57 コミットを再現した。
- **リンクは `inherited` が 143 本増え、ほかの種類は変わらなかった。** 13 のコミットにあり、`ambiguous` は 10 本だった。前身が複数の後継を持つ `inherited` は 4 組で、すべて親のリンク自体が多対多か `ambiguous` のものだった(3 節)。
- **コミット数が変わった unit は 74 個(すべて増加、計 172 コミット)。** churn の上位 10 件と fan-in × churn の上位 10 件は変わらなかった。`unit_cochange` は 3,761 → 3,888 行、`cochange_without_reference` は 1,203 → 1,254 行になった。`contain` の `clone_change_consistency` は 4 行の数が変わり、上位 10 件に `AgentRoutes.register_agent` と `TerminalRoutes.register_terminal`(2 / 5、両側 2)が入り、`open_palette` と `open_view_chooser` が 11 位に出た(4 節)。
- **後ろ向き(`production`)の P@k の動きは、多くが 0.01 以内だったが、0.01 を超えたものもあった。** クローンの P@50(1 コミット以上)0.72 → 0.76、co-change の P@10 0.6 → 0.7・P@100 0.43 → 0.40、`fan_in_x_commits` の P@50 0.5 → 0.48 など(5 節)。正解の数(778、92)は変わらなかった。
- **更新、計画、検査。** 継承の関係の導出のビューの本文が変わるので、`lineage_key` が変わり、前のキャッシュの最初の更新で継承の関係が作り直された(1,625 コミット、21,113 行、系譜の層 5.6 s)。空のキャッシュからの更新とその後の更新の時間は、同じ機械の前の SQL と振れの中で同じだった。`check.py explain` の数は前と同じだった。`check.py refresh` と `check.py fixture` の結果は 6 節に書いた。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 2c82920d に合わせた(変更の無い状態で `git reset --hard`)。測定はこのリポジトリの使い捨ての複製 3 つで行い、作業ツリーのファイルと履歴は測定に使っていない。

- `target/code-db/head`:2c82920d。
- `target/code-db/retro/base`:e3c619a0(後ろ向きの基準の木)。
- `target/code-db/retro/end`:46c37290(リファクタリングの範囲の終わり)。

3 つとも作り直した(`git clone` と `checkout`、索引と jscpd の報告、DuckDB の拡張のビルド)。前の SQL は、複製 `head` の中の 2c82920d の `duckdb-ext/sql` と、`git archive` で `target/code-db/oldsql` に取り出した同じもの(`check.py measure-refresh` は作業ツリー自身の複製を受け付けないため)で動かした。前と後は `cache_path` で別のキャッシュのファイルを使った。前の SQL の測定は `clone_sides := 'contain'` で行い、後の SQL は既定(`contain`)で行ったので、比べた差は系譜の差だけである。

機械はオーナーの対話用デスクトップで、他の作業と共有している。作業中、ディスクの空きは 18 GB から 15 GB に減っていき、`00_load.sql` の spill の上限はその半分(7〜9 GB)だった。DuckDB CLI 2.0.0、rust-analyzer、jscpd 5.3.1 を使い、すべて `nice -n 19` で動かした。測定用の SQL とシェルは `target/code-db/scripts/` に置き、コミットしていない。時間は Python の `resource.getrusage(RUSAGE_CHILDREN)` で包んだ 1 回ずつの値である。

```sh
nice -n 19 duckdb-ext/build.sh
uv run --locked --script scripts/refactor-audit/run.py setup
duckdb-ext/sql/run.sh index target/code-db/head             # rust-analyzer scip 200.2 s
duckdb-ext/sql/run.sh index target/code-db/retro/base
duckdb-ext/sql/jscpd.sh target/code-db/retro/end target/code-db/retro/end/target/code-db/jscpd src crates
# 前の SQL と後の SQL で、単位の履歴、継承の関係、クエリの結果を DuckDB のファイルに書き出す
target/code-db/head/duckdb-ext/sql/run.sh sql target/code-db/head export.sql m_out=before.duckdb clone_sides=contain cache_path=…/cache-before.duckdb
duckdb-ext/sql/run.sh sql target/code-db/head export.sql m_out=after.duckdb cache_path=…/cache-after.duckdb   # 初回の構築を含めて 41.1 s、最大 9.4 GB
# 後ろ向き(前と後の SQL のそれぞれで。キャッシュは cache_path で分けた)
run.sh sql END duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 …
run.sh sql END duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 …
run.sh sql BASE duckdb-ext/sql/retro/signals.sql retro_scope=production retro_min_commits=1|3 clone_sides=contain …
uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/head --runs 2   # 前は oldsql の check.py
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/head                     # 前は複製の中の check.py
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/head
uv run --locked --script duckdb-ext/sql/check.py fixture
```

## 1. 組んだもの

### `clone_sides` の既定

`queries/clone_change_consistency.sql` の `clone_sides` の既定を `'contain'` にした。`'overlap'` は前の増分のまま使える。クエリの注釈、README の説明と変数の表、`check.py` の history fixture と counting fixture を合わせた。fixture は、変数を設定しないときの値が `contain` の値と同じであることと、`'contain'` を明示した値、`'overlap'` を明示した値を確かめる。counting fixture の `cache.sql` を通した比べ方では、既定の行に加えて `'overlap'` の行も比べるようにした(前は `'contain'` の行を足していた)。

### 継承の規則(`views/lineage.sql`)

`derived_unit_successions` に、5 つ目のリンクの種類 `inherited` を足した。1 つのコミットの中で求める。

1. 親のリンク:そのコミットの `content_match` と `body_move`(`matched_links`)。
2. 候補:親の前身と同じパスの前の側の unit で、限定名が前身の限定名に `.` で続くもの。後継の同一性は、親の後継のパスと、親の後継の限定名に同じ相対名を付けた限定名、同じ種類とする。相対名は親から下の全体なので、module の改名は、その中の impl・型・関数と、impl の method まで、1 つの親のリンクから届く(2 段の入れ子は 1 本のリンクになる)。
3. 条件(b):推測した後継の同一性が、そのコミットの後の側の unit にある。前の側と後の側の unit は `unit_change_sides`(変更されたファイルの両側のすべての unit)からとるので、追加の読み込みは無い。
4. 条件(a)と、自分のまま続くものの除外:前身の同一性がそのコミットで `content_match`・`body_move`・`file_rename` の前身でなく、後継の同一性が `content_match`・`body_move` の後継でないこと。さらに、前身の同一性が同じパスの後の側に無く、後継の同一性が同じパスの前の側に無いこと(どちらかがあれば、その同一性は自分のまま続いている)。
5. 列:`similarity` は、hunk がどちらの側の内側の unit にも触れていなければ 1、触れていれば、その組の行の集合の類似度(共通の行が無ければ 0)。`ambiguous` は、親のリンクが `ambiguous` のとき、または前身か後継がそのコミットで複数の `inherited` を持つとき。

`unit_lineage_segments`、`unit_history`、churn、co-change は変えていない。`inherited` は他のリンクと同じく歩かれ、そのコミットは `via_link` になる。`views/graph.sql` の `unit_commits` の注釈と README の `via_link` の説明に `inherited` を足した。

最初の版は、条件の確かめと類似度を相関のある副問い合わせ(`NOT EXISTS` と、`image_keys`・`scored` を読むスカラーの副問い合わせ)で書いた。fixture は通ったが、HEAD の複製の空のキャッシュからの更新で、spill の上限(8.4 GiB)を超えて止まった。`SEMI JOIN`・`ANTI JOIN` と、物化した補助の CTE(自分のリンクを持つ同一性、内側の unit の像、その組の類似度)に書き直し、同じ更新が 41.1 s(書き出し全体)で通った。

### キャッシュ

`refresh.sql` の `lineage_key` は、継承の関係を導出するビューの本文の md5 を含むので、`derived_unit_successions` を変えたことで変わる。`refresh.sql` は変えていない。前の SQL で作ったキャッシュの写しに後の SQL で更新をかけると、`lineage` が `rebuilt` になり、1,625 コミットが導出され、継承の関係は 21,113 行(前の 20,970 行と `inherited` の 143 行)になった。系譜の層は 5.6 s、更新全体は 6.3 s だった。

## 2. 履歴が空の unit

今の unit 9,264 個のうち、`unit_history` が空のものは 33 → 3 になった。残ったのは、`crates/horizon-board/src/event.rs` の `tests`(module。囲む unit が無い)、`TerminaldHandle.broadcast_terminal_color_scheme` と `TerminaldHandle.stop_and_wait`(8d8cdd0e の `file_rename` だけが系譜にあり、改名前の同一性にも変更が無い)である。事前の確かめの見込みどおり、`TerminaldHandle` の 2 つは 0 のままだった。

| unit | 後のコミット | 事前の確かめ |
| --- | ---: | ---: |
| `HostTools for AgentdHostTools.execute_auto` | 5 | 5 |
| `AgentdState.writer` | 4 | 2 |
| `AgentdState.skipped_lines_summary` | 3 | 2 |
| `ExplorationHost for AgentdExplorationHost.terminate` | 3 | 3 |
| `tests.real_git.*` の関数と型 10 個 | 1〜3 | 同じ |
| `tests.real_git.Drop for EnclosingRepoGuard`、`tests.real_git.EnclosingRepoGuard`(型と impl) | 2 | 2 |
| `tests.real_git.Drop for EnclosingRepoGuard.drop` | 2 | 0 |
| `tests.real_git.EnclosingRepoGuard.capture` | 2 | 0 |
| `AgentdState.mark_resume_ready`、`AgentdState.set_writer` | 2 | 2 |
| `Default for State.default` | 2 | 2 |
| `AgentdHandle.begin_reload`、`AgentdHandle.stop_and_wait` | 2 | 2 |
| `AgentdHandle.same_runtime` | 1 | 1 |
| `tests.DecisionClient for PendingClient.decide.OnDrop`、`… .Drop for OnDrop` | 1 | 1 |
| `tests.DecisionClient for PendingClient.decide.Drop for OnDrop.drop` | 1 | 0 |
| 計 | 30 個、65 コミット | 27 個、57 コミット |

事前の確かめの方法(囲む unit を上へたどり、その系譜の深さ 1 以上の同一性に `.` と unit の名前を付けて、同じ区間の `unit_changes` を数える)を SQL に写し、前の SQL のキャッシュで動かすと、27 個、57 コミットになり、依頼文の数と一致した。違いは次の 2 つだった。

- **2 段以上の入れ子での相対名(3 個、5 コミット)。** 事前の確かめの方法は、祖先の名前に unit 自身の名前だけを付ける。`tests.real_git.EnclosingRepoGuard.capture` の module の祖先 `tests.isolation_announcement` からは `tests.isolation_announcement.capture` を探すことになり、見つからない。組んだ規則は親のリンクから下の相対名(`EnclosingRepoGuard.capture`)を保つので、`tests.isolation_announcement.EnclosingRepoGuard.capture` につながる。`… .Drop for EnclosingRepoGuard.drop`(d7a3046a の module の改名)と、`decide.Drop for OnDrop.drop`(cad2cfab の method `complete` → `decide` の `content_match`)も同じである。
- **つながった後の祖先のリンク(2 個、3 コミット)。** `AgentdState.writer` と `AgentdState.skipped_lines_summary` は、8d8cdd0e の `inherited` で `SessiondState.*` につながった後、0d72fc00 の method 自身の `content_match` で `Connection.*` へ、さらに 4e7fb81e の `file_rename` と `inherited` へ歩く。事前の確かめの方法は囲む unit の系譜の名前だけを使うので、method 自身の古いリンクをたどらない。

依頼文が挙げた例(`begin_reload` と `stop_and_wait` が 2 ずつ、`execute_auto` が 5、`tests.real_git.*` が 1〜3、`TerminaldHandle` の 2 つが 0)は、すべてそのとおりだった。

## 3. リンクの数

| リンク | 前 | 後 | `ambiguous`(前 / 後) |
| --- | ---: | ---: | --- |
| `same_name` | 14,044 | 14,044 | 0 / 0 |
| `file_rename` | 3,293 | 3,293 | 0 / 0 |
| `content_match` | 3,506 | 3,506 | 650 / 650 |
| `body_move` | 127 | 127 | 15 / 15 |
| `inherited` | — | 143 | — / 10 |

`inherited` 143 本の前身の種類は method 126、関数 11、impl 3、型 3 で、122 本は hunk が内側の unit に触れていない(`similarity` 1)。13 のコミットにあり、多いのは 8d8cdd0e(「sessiond」の語の整理、34 本)、e899e99d(エージェントのモジュールの再編、30 本)、3f908482(エージェントのペインの分割、20 本)、4e7fb81e(エージェントのデーモンの改名、19 本)、d7a3046a(module `isolation_announcement` → `real_git`、17 本)である。

`ambiguous` の 10 本は 3 つの形だった。

- **8d8cdd0e の `Drop for SessiondProcess.drop` → `Drop for AgentdProcess.drop`(4 本)。** `crates/horizon-agentd/tests/e2e.rs` と `crates/horizon-terminald/tests/e2e.rs` に同じ impl があり、親の impl の `content_match` が 2 つのファイルの間で交差していた(どちらの前身もどちらの後継にも似ている)。内側の method も、2 × 2 の組でつながった。
- **e30128b4 の `OverviewItem.kind_color` と `PaletteItem.kind_color`(4 本)。** `src/main.rs` から `src/control_surface.rs` への移動で、型 `OverviewItem` と `PaletteItem` の `content_match` が交差していた(`ambiguous`)。型の名前の下に method の限定名があるので、型の交差したリンクから method にも 2 × 2 の組ができた(`similarity` 0.2)。impl の `content_match` は交差していない。
- **a4fd9605 の `FakeHub.agent_attachment` → `FakeSessionHub.agent_attachment`、`FakeHub.terminal_attachment` → `FakeTerminalHub.terminal_attachment`(2 本)。** 親の `FakeHub` が 2 つに分かれた(`ambiguous` の分割)。条件(b)で、各 method は自分の名前がある方の後継にだけつながった。

前身が複数の `inherited` の後継を持つのは 4 組、後継が複数の前身を持つのも 4 組で、どれも上の最初の 2 つの形である。

## 4. HEAD の候補

コミット数が変わった unit は 74 個で、すべて増えた(計 172 コミット)。履歴が空だった 30 個のほかに、44 個が増えた。増えた数の大きいものは、`AgentdState.new` 2 → 15、`AgentdState.send_command` 2 → 8、`ExplorationHost for AgentdExplorationHost.start` 4 → 9、`AgentRoutes.settle_routes` 5 → 10、`Store.clear_projections_for_session` 1 → 5、`FakeTerminalHub.terminal_attachment` 1 → 5 である。`unit_churn` の行は 78 行で値が変わった。

### churn と fan-in × churn

`unit_churn` の上位 10 件(`run_session` 51、`build_definitions` 49、`WorkspaceShell.execute` 45 …… `TerminalSession.spawn` 34)は、順も数も前と同じだった。`unit_fan_in_churn` の上位 10 件(`Event`、`AgentdState`、`ToolCallResult`、`Command`、`ToolCallRequest`、`RigAgentConfig`、`MessageRole`、`WorkspaceShell`、`SessionId` の impl、`CommandId`)も同じだった。`unit_fan_in_churn` は 35 行で値が変わり、上位 50 件では `WriterHandle.flush`(7 → 10 コミット)が 52 位から 36 位に上がり、36〜50 位が 1 つずつ下がった。

### `clone_change_consistency`(既定の `contain`)

上位の作り方は、クローンの側の記録と同じにした(片側のある行から両側がテストの行を除き、同じ側の組の断片をまとめて組の中の最大をとり、片側と両側の降順)。テストは `unit_tests` で判定した。前の SQL の上位 10 件は、クローンの側の記録の `contain` の上位 10 件と同じだった。

| 順位 | 前 | 後 |
| ---: | --- | --- |
| 1 | `TerminalSession.spawn` と `WorkspaceShell`(14 / 34、6) | 同じ |
| 2 | `command_for` と `term_key_code`(17 / 0、0) | `apply_agent_event_to_frame` と `Store.project_event`(11 / 8、18) |
| 3 | `apply_agent_event_to_frame` と `Store.project_event`(10 / 6、17) | `command_for` と `term_key_code` |
| 4〜7 | `fold_until_terminal`、`render_agent_transcript` の組、`open_palette` と `open_session_manager` | 同じ |
| 8 | `tools/tests.rs`(unit に解決しない)と `src/board/model.rs` の `tests`(0 / 6、0) | `AgentRoutes.register_agent` と `TerminalRoutes.register_terminal`(2 / 5、2)、入った |
| 9 | `append_event` と `append_record_uncommitted`(2 / 3、11) | `tools/tests.rs` と `tests` |
| 10 | `open_palette` と `open_view_chooser`(3 / 2、5) | `append_event` と `append_record_uncommitted` |

`open_palette` と `open_view_chooser` は 11 位になった(数は同じ)。数が変わった組は 1,502 組のうち 4 組で、`apply_agent_event_to_frame` と `Store.project_event` の 2 組(断片 3 と 298、10 / 6、17 → 11 / 8、18)、`register_agent` と `register_terminal`(2 / 0、1 → 2 / 5、2)、`Store.messages_for_session` と `Store.approvals_for_session`(0 / 1、3 → 0 / 2、6)だった。どの `inherited` のリンクがこの組の数を増やしたかは確かめておらず、コードも読んでいない。読み判定 2 の 3 番(`AgentdHandle` と `TerminaldHandle`)は、`contain` では側が 2 つの impl なので、method の系譜がつながっても数は変わらない(1 / 0、1。この作り方で前後とも 41 位)。

### co-change

`unit_cochange` は 3,761 → 3,888 行(206 行が変わった)、`cochange_without_reference` は 1,203 → 1,254 行(77 行が変わった)だった。

## 5. 後ろ向き(`production` の範囲)

基準の複製と終わりの複製で、前の SQL と後の SQL のそれぞれで、正解と信号を作った(既定の変数、`clone_sides` は両方 `contain`)。前の SQL の値は、帰属の記録の後の値(クローンの 0.8 / 0.72 / 0.66、0.085、3 コミット以上で 0.3 / 0.2 / 0.19、0.207、churn の P@100 0.72、`fan_in_x_commits` 0.48、co-change 0.43)と一致した。refactored は 1 コミット以上で 778、3 コミット以上で 92、preserve は 30 で、前と後で同じだった。

1 コミット以上:

| 信号 | 順位の付く数(前 → 後) | P@10 | P@50 | P@100 | recall@100 |
| --- | --- | --- | --- | --- | --- |
| `unit_churn` | 2,583 | 0.9 → 0.9 | 0.76 → 0.76 | 0.72 → 0.71 | 0.093 → 0.091 |
| `fan_in_x_commits` | 1,307 | 0.9 → 0.9 | 0.5 → 0.48 | 0.48 → 0.47 | 0.062 → 0.06 |
| `fan_in` | 1,307 | 0.4 → 0.4 | 0.36 → 0.36 | 0.32 → 0.32 | 0.041 → 0.041 |
| `clone_one_sided` | 154 | 0.8 → 0.8 | 0.72 → 0.76 | 0.66 → 0.66 | 0.085 → 0.085 |
| `cochange_without_reference` | 268 → 292 | 0.6 → 0.7 | 0.44 → 0.44 | 0.43 → 0.4 | 0.055 → 0.051 |
| `loc` | 2,583 | 0.8 → 0.8 | 0.78 → 0.78 | 0.75 → 0.75 | 0.096 → 0.096 |

3 コミット以上:

| 信号 | P@10 | P@50 | P@100 | recall@100 |
| --- | --- | --- | --- | --- |
| `unit_churn` | 0.5 → 0.5 | 0.32 → 0.32 | 0.31 → 0.32 | 0.337 → 0.348 |
| `fan_in_x_commits` | 0.6 → 0.6 | 0.24 → 0.24 | 0.15 → 0.15 | 0.163 → 0.163 |
| `clone_one_sided` | 0.3 → 0.3 | 0.2 → 0.22 | 0.19 → 0.18 | 0.207 → 0.196 |
| `cochange_without_reference` | 0.0 → 0.0 | 0.06 → 0.06 | 0.11 → 0.11 | 0.12 → 0.12 |

依頼文の見込み(0.01 以内)を超えたのは、クローンの P@50(1 コミット以上 +0.04、3 コミット以上 +0.02)と recall@100(3 コミット以上 −0.011)、co-change の P@10(+0.1、上位 10 件の 1 件)と P@100(−0.03)、`fan_in_x_commits` の P@50(−0.02)、churn の recall@100(3 コミット以上 +0.011)だった。co-change は順位の付く同一性が 268 → 292 に増えた。どの同一性が入れ替わったかは調べていない。

## 6. 時間、更新、計画、検査

`check.py measure-refresh`(各 2 回、段階ごとに新しいプロセス)を、同じ複製の上で前の SQL と後の SQL で実行した。

| 段階 | 前 | 後 | 後の系譜の層 |
| --- | ---: | ---: | ---: |
| 初回(空のキャッシュから) | 26.1〜27.2 s、6.8〜6.9 GB | 23.6〜26.2 s、6.7〜7.0 GB | 5.77〜5.92 s(前 6.22〜6.75 s) |
| 変更なし | 0.23〜0.26 s | 0.29〜0.31 s | 0.04 s |
| 1 ファイルを編集 | 0.35〜0.37 s | 0.33〜0.41 s | 0.03〜0.04 s |
| 10 ファイルを編集 | 0.39〜0.43 s | 0.37〜0.43 s | 0.03 s |
| 3 ファイルの 1 コミット | 0.78〜0.86 s | 0.78〜3.22 s | 0.42〜0.56 s(前 0.41〜0.44 s) |
| HEAD の amend | 3.27〜3.30 s | 2.92〜3.27 s | 0.48〜0.53 s |

1 コミットの段の後の 3.22 s は 1 回だけで、その回は units の層が 1.73 s だった(系譜の層ではない)。系譜の記録の値(初回 24.5〜24.7 s、系譜の層 5.50〜5.52 s、1 コミットの系譜の層 0.18〜0.27 s)と比べると、今回は前の SQL でも 1 コミットの系譜の層が 0.41〜0.44 s で、その後の増分による差と機械の状態の差を分けていない。キャッシュのファイルは前 621.5 MiB、後 622.0 MiB、キャッシュからの `clone_change_consistency` は前 2.16〜2.17 s、後 2.06〜2.08 s だった。

`check.py explain`(HEAD の複製)は、前の SQL(複製の中の `check.py`)と後の SQL で、すべての行が同じだった(履歴を読むクエリの `READ_AST` 2、`unit_churn`・`clone_change_consistency`・`unit_fan_in_churn` の `READ_TEXT` 11)。

`check.py refresh` を HEAD の複製で実行し、2 回の更新のどちらでも、すべてのビューとクエリがライブと一致した。1 回目(8.09 s)は、直前の `measure-refresh` が前の SQL で残したキャッシュの上の更新で、`lineage_key` が変わったので系譜の層が作り直された(7.05 s)。2 回目は 0.32 s だった。コマンド全体は 1,135.6 s、子のプロセスの RSS の最大は 23.4 GB だった。

`check.py fixture` は、すべての段(fixture、history、lineage、scip、graph、counting、refresh)で通った(全体で 86.8 s)。lineage fixture には 5 つのコミットを足した。`src/meters.rs` に、method 2 つの impl(`Meter`)と、型・impl・method を持つ module(`gauges`)を追加し、`Meter.reading` と `gauges.Dial.needle` をそれぞれ編集し、`Meter` を `Gauge` に改名(型と impl の頭の行だけ)するコミットで `calibrate` を編集して method `zero` を足し、最後に module を `dials` に改名する。確かめたこと:2 つの改名のコミットの `same_name` 以外のすべての継承の関係(`Meter.reading` → `Gauge.reading` が `inherited`、`calibrate` は自分の `content_match`(5/7)、`Gauge.zero` には前身が無い、module の `content_match` から型・impl・method の 3 本の `inherited`)、`src/meters.rs` のすべての unit の `unit_history`(`Gauge.reading` は追加と編集の 2 コミット、`dials.Dial.needle` も 2 コミット、`Gauge.zero` は改名のコミットだけ)、`dials.Dial.needle` の系譜の鎖(深さ 1 で `gauges.Dial.needle`、`inherited`)。既存の確かめはそのまま通り、`cache.sql` を通した行の比較も通った。

## 7. 依頼文が決めていなかったこと

- **規則をコミットごとのリンクにしたこと。** 依頼文は、囲む unit の系譜の各同一性(区間つき)から推測した同一性を区間ごとに受け取る形で書いていた。組んだのは、各コミットの親のリンクから内側の unit のリンクを作り、系譜の歩きに任せる形である(依頼文の「`derived_unit_successions` か `unit_lineage_segments` に置く」の前者)。継承の関係がコミットごとに保存され、キャッシュの追記の扱いがそのまま使える。違いは 2 節の 2 つ(相対名、つながった後の自分のリンク)として現れた。
- **相対名。** 親から下の相対名の全体を保った(`EnclosingRepoGuard.capture` の module の改名では `… .EnclosingRepoGuard.capture`)。囲む unit の判定は、`parent_unit_id` ではなく限定名の接頭辞(`.` で続くこと)にした。`unit_change_sides` は親の番号を出さず、限定名は入れ子をそのまま表すためである。
- **親になるリンク。** `content_match` と `body_move` だけにした。`file_rename` は限定名を変えず、内側の unit も同じ `file_rename` を持つ。`inherited` を親にしないのは、相対名の全体を 1 本の親のリンクから作るので、入れ子の各段がそれで届くためである。
- **条件(a)の範囲。** 前身の側でも確かめた(前身が自分の `content_match`・`body_move`・`file_rename` を持てば継がない)。後継の側は `content_match` と `body_move` の後継であるときに継がない。加えて、前身が同じパスの後の側にある、または後継が同じパスの前の側にあるときも継がない(その同一性は自分のまま続く)。
- **多対多。** 親のリンクが交差していたり分割だったりすると、内側の unit も複数の後継を持つことがある(4 組)。系譜の多対多のオーナーの判断(系譜の記録)に合わせてすべてつなぎ、`ambiguous` を付けた。依頼文はこの場合を決めていない。
- **`similarity`。** 行に触れられていない内側の unit は 1、触れられていれば前後の像の類似度(閾値を下回る値になる。閾値以上なら自分の `content_match` があるので継がない)とした。
- **`clone_sides` の README の書き方。** `overlap` が前の増分で既定だったことと、戻した理由の記録への参照を README に 1 文で残した。

## 8. 記録と食い違った点、気づいた点

- **事前の確かめの数との差(30 個・65 コミット と 27 個・57 コミット)は、2 節の 2 つで説明できた。** 事前の確かめの方法を写した SQL は 27 個・57 コミットを再現した。
- **impl を改名すると、その中で行に触れられない method が多い。** `inherited` の 143 本のうち 122 本は内側の unit に hunk が触れていなかった。系譜が切れていたのは履歴が空の 33 個だけでなく、ほかに 44 個の unit が履歴の一部を失っていた(`AgentdState.new` の 13 コミットなど)。
- **型の `content_match` の交差が、その型の method に伝わった。** e30128b4 の `OverviewItem` と `PaletteItem` は、型のリンクが交差していたため、`kind_color` も 2 × 2 でつながった(impl のリンクは交差していない)。型と impl が同じ限定名を持つので、どちらのリンクからも同じ method が届く。
- **最初の書き方(相関のある副問い合わせ)は、空のキャッシュからの更新で spill の上限を超えた。** fixture では見えなかった。

## 9. 限界

- **判定はしていない。** 上位に入った `register_agent` と `register_terminal` の組を含め、コードは読んでいない。
- **後ろ向きは `production` の範囲だけである。** 動いた P@k について、入れ替わった同一性は調べていない。
- **1 つのリポジトリ**の測定である。
