# コードデータベースのクローンの側の規則(2026-10-27)

追記:次の増分で、`clone_sides` の既定は `contain` に戻した(`overlap` は変数で残る)。[系譜の継承の規則の記録](code-db-inherited-lineage-2026-10-27.md)を参照。以下の本文の「既定は `'overlap'`」は、この記録の時点のものである。

コードデータベースの SQL 層の 11 個目の増分である。[帰属の規則の記録](code-db-attribution-2026-10-27.md)(以下「帰属の記録」)と [設計文書](../code-database-design.md) の「Open points」の最初の項目が残した機構 M4(クローンの片側だけの変更を、断片ではなく断片を含む unit で数える)について、`clone_change_consistency` の側の決め方を変え、fixture の確かめを足し、今の上位の候補と後ろ向きの precision への効き方を測った。SQL と `check.py` だけを変え、Rust は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、オーナーは 2026-10-27 に、次の形について「はい。お願いします。」と承認した。

- クローンの各側(ファイルと開始行・終了行)の unit は、断片の行と自分の行が重なる最も内側の unit の集合とする。自分の行は M1 の意味のもの、つまり `[lead_start_line, end_line]` から直接の内側の unit の同じ範囲を除いたものである。3 つの method にまたがる断片はその 3 つの method に対応し、impl は、断片が impl 自身の行(頭や method の間の行)に重なるときだけ入る。どの unit にも入らない断片は、前と同じく何にも対応しない。
- そのうえで、`unit_history`(系譜)を通し、N2 の境(両側の集合が存在するようになってから)の後で、A の unit のどれかに触れ B のどれにも触れないコミットを `only_a`、その逆を `only_b`、両方の集合に触れたものを `both` とする。
- 列 `units_a`、`units_b`(数)と `shared_units`(両方の集合にある unit)を足し、`same_unit` は「2 つの集合が同じ」という意味にする(意味の変更を文書に書く)。前の決め方は変数で残す(`clone_sides := 'contain'` で前の振る舞い、既定は `'overlap'`)。

前提は、帰属の記録、[読み判定 2](code-db-dogfood-reading-2-2026-10-26.md) のクローンの候補 10 件とその判定、`duckdb-ext/sql/` の SQL と README である。記録の決定は問い直していない。

## 要約

- **規則を `clone_change_consistency` に組み、前の決め方を `clone_sides := 'contain'` に残した。** HEAD の複製で、`contain` の結果は前の SQL の結果と、前からある 20 列のすべてと行の順まで一致した(1,502 行)。
- **読み判定 2 のクローンの 10 件のうち、数が変わったのは `AgentdHandle` と `TerminaldHandle`(3 番)だけだった。** ほかの 9 件は、断片が 1 つの unit の中にあるので、側の集合が前の unit 1 つのままか(8 件)、数が変わらなかった(10 番。側 b が unit 2 つに解決したが、境の後にその unit の変更が無かった)。3 番は、5 つの断片の片側の最大が 1 / 0(両側 1)から、断片ごとに 1 / 0 〜 3 / 0 になり、上位 10 件に戻らなかった(最も上の断片の組で 34 位)。
- **3 番が戻らなかった理由の 1 つは、系譜の切れ目だった。** 判定の理由の `begin_reload` と `stop_and_wait` のうち、`AgentdHandle.begin_reload`、`AgentdHandle.stop_and_wait`、`TerminaldHandle.stop_and_wait` の `unit_history` は 0 コミットだった。impl の改名(`SessiondHandle` → `AgentdHandle`)で method の同一性が変わったが、method の行に hunk が触れなかったので、`unit_changes` にその method の追加も削除も無く、系譜がつながらない。今の unit 9,264 個のうち 33 個(method 15、関数 11)が履歴 0 だった。
- **上位 10 件に新しく 3 件が入り、3 件が出た。** 入ったのは、`NamedColor` と `CommandId`(5 / 16、`問題なし`)、`explore.rs` と `web/mod.rs` の 2 つの `panic_message`(17 / 0、`問題なし`)、`runtime/agent.rs` と `runtime/terminal.rs` の接続のループ(9 / 0、両側 5、`問題あり`)である。出たのは、`tools/tests.rs` と `src/board/model.rs` の `tests`(側 a が解決し両側がテストになって候補から除かれた)、`append_event` と `append_record_uncommitted`(12 位)、`open_palette` と `open_view_chooser`(13 位)である。`問題あり` の候補で外れたものは無い。
- **側の unit の数は、`contain` で 0 が 771 側・1 が 2,233 側、`overlap` で 0 が 29・1 が 1,656・2 が 570・3 以上が 749 側になった。** 片側のある組は 520 → 873、`same_unit` が変わった組は 254(真 → 偽 240)、片側の数が変わった組は 392 だった。
- **断片の境の 1 行で入る unit が多い。** 側と unit の対応 5,433 個のうち 670 個は重なりが 1 行で、そのうち 533 個は impl・module・型だった。新しく入った `panic_message` の組の片側 17 は、断片の最初の行(前の関数 `fold_until_terminal` の閉じ括弧)で入った `fold_until_terminal` の履歴そのものだった。
- **後ろ向き(`production`)では、クローンの信号の precision が下がった。** 1 コミット以上で P@10 / P@50 / P@100 が 0.8 / 0.72 / 0.66 → 0.3 / 0.38 / 0.38、recall@100 が 0.085 → 0.049、3 コミット以上で 0.3 / 0.2 / 0.19 → 0.1 / 0.12 / 0.10、0.207 → 0.109 だった。順位の付く同一性が 154 → 379 に増え、上位 100 のうち 62 が新しく入った同一性(正解 10)で、前からの 38 は正解 28 だった。`contain` の値は帰属の記録の後の値と一致した。
- **時間、更新、計画。** キャッシュからのクエリの時間は、前の SQL 1.57〜1.76 s、`contain` 1.45〜1.81 s、`overlap` 1.48〜1.77 s(各 5 回)で、差は振れの中だった。`check.py fixture` が通り、HEAD の複製で `check.py refresh` の 2 回の更新がすべてのビューとクエリでライブと一致した。`explain` の数は前の SQL と同じだった(`READ_AST` 2、`READ_TEXT` 11)。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の 0a3f935d に合わせた(変更の無い状態で `git reset --hard`)。測定はこのリポジトリの使い捨ての複製 3 つで行い、作業ツリーのファイルと履歴は測定に使っていない。

- `target/code-db/head`:0a3f935d。Horizon のソース(`src`、`crates`、`preview-plugin`)は、帰属の記録の e30d981a と同じである。
- `target/code-db/retro/base`:e3c619a0(後ろ向きの基準の木)。
- `target/code-db/retro/end`:46c37290(リファクタリングの範囲の終わり)。

3 つとも、この作業ツリーで作り直した(`git clone` と `checkout`、索引と jscpd の報告、DuckDB の拡張のビルド、キャッシュ)。前の SQL は、0a3f935d の `queries/clone_change_consistency.sql` を写したもので、同じキャッシュの上で動かした(この増分はキャッシュのスキーマを変えていない)。機械はオーナーの対話用デスクトップで、他の作業と共有している(測定中の 1 分の load average は 1〜4)。DuckDB CLI 2.0.0(`~/.local/bin/duckdb2`)、rust-analyzer、jscpd 5.3.1 を使い、すべて `nice -n 19` で動かした。測定用の SQL とシェルは `target/code-db/measure/` に置き、コミットしていない。時間は `run.sh` などを包んだ Python の `resource.getrusage(RUSAGE_CHILDREN)` で取った 1 回ずつの値である。

```sh
nice -n 19 duckdb-ext/build.sh
uv run --locked --script scripts/refactor-audit/run.py setup
git clone -q --no-checkout . target/code-db/head            # その後 checkout 0a3f935d(base、end も同様)
duckdb-ext/sql/run.sh index target/code-db/head             # jscpd 0.6 s、rust-analyzer scip 133.4 s
duckdb-ext/sql/run.sh index target/code-db/retro/base       # jscpd 0.4 s、rust-analyzer scip 145.1 s
duckdb-ext/sql/jscpd.sh target/code-db/retro/end target/code-db/retro/end/target/code-db/jscpd src crates   # 0.5 s
# 前の SQL、contain、overlap の結果と文脈の表を DuckDB のファイルに書き出す(初回のキャッシュの構築を含めて 27.3 s、最大 RSS 8.3 GB)
duckdb-ext/sql/run.sh sql target/code-db/head target/code-db/measure/export.sql m_out=…
# 後ろ向き
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 retro_changes=…        # 18.6 s、6.7 GB
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 …                       # 1.3 s、0.9 GB
duckdb-ext/sql/run.sh query BASE unit_churn LIMIT=1                                                          # 基準のキャッシュの構築 17.0 s、6.9 GB
duckdb-ext/sql/run.sh sql BASE duckdb-ext/sql/retro/signals.sql … retro_scope=production retro_min_commits=1|3 clone_sides=contain|overlap   # 6.6〜7.5 s、4.8〜6.8 GB
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/head
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/head   # 前の SQL は複製の中の check.py で
```

## 1. 組んだもの

`queries/clone_change_consistency.sql` の側の決め方を `clone_sides` で切り替える。

- **`overlap`(既定)。** 側のファイルの unit について、自分の行を区間として作る。区間の作り方は `views/unit_changes.sql` の innermost の `own_spans` と同じで、unit の `[lead_start_line, end_line]` から直接の内側の unit の同じ範囲を除いた隙間である。断片の行 `[start_line, end_line]` と重なる区間を持つ unit が、その側の集合になる。空行は除いていない(依頼文の定義が `[lead_start_line, end_line]` から内側の unit を除いたものだったため。5 節)。
- **`contain`。** 前の決め方(断片全体を含む最も小さい unit。複数の unit にまたがれば無し)。

数え方は前のクエリの形をそのまま使う。側ごとの unit の行(`resolved`)が 1 行から複数行になり、コミットごとに「側 a の unit のどれかに触れたか」「側 b の unit のどれかに触れたか」を `bool_or` で求める。両方の集合にある unit に触れたコミットは両側になる。

足した列は `units_a`、`units_b`、`shared_units`(数)と、`side_units_a`、`side_units_b`(`{kind, qualified_name}` の並び)である。前からの列の位置は変えず、後ろに足した。`kind_a`/`unit_a`(`kind_b`/`unit_b`)は、集合のうち断片と重なる自分の行が最も多い unit の名前にした(同数なら小さい unit、それも同じなら後の `unit_id`。`contain` では前と同じ 1 つの unit)。`same_unit` は「2 つの集合が同じ」で、側 a に unit が無いときは偽、側 b だけに無いときは NULL にした。これは前の式(`a.unit_id IS NOT NULL AND a.unit_id = b.unit_id`)の値と同じで、`contain` で前の結果を保つためである。

N2 の境(`since_sha`)は、両側の集合のすべての unit の最初のコミットのうち最も遅いものにした(5 節)。

側のファイルの unit は、物化した CTE で 1 回だけ読む。最初の版は `units` を 3 か所で読み、ライブの計画の `READ_TEXT` が 11 → 15 になったので、そう直した(4 節)。

`retro/signals.sql` のクローンの信号は、`unit_a`/`unit_b` の代わりに `side_units_a`/`side_units_b` のすべての unit を、その組の順位で並べるようにした。`contain` では集合は前の unit 1 つなので、前と同じ行になる(3 節で確かめた)。

README には、側の規則、変数、`same_unit` の意味の変更、fixture の記述を足した。設計文書は変えていない。

## 2. 測定 A:HEAD の候補

上位の作り方は帰属の記録と同じである。`clone_change_consistency` の片側のある行から、両側がテストの行を除き、同じ側の組の断片を 1 つにまとめ(数は組の中の最大)、片側と両側の降順に並べる。`overlap` では、側の組を「ファイルと unit の集合」の組とした。集合のすべての unit がテストのとき、その側をテストとした。

### 読み判定 2 の 10 件

| # | 組 | 判定(読み判定 2) | `contain`(片側 a / b、両側、順位) | `overlap`(同) |
| --- | --- | --- | --- | --- |
| 1 | `TerminalSession.spawn` と `WorkspaceShell`(型) | 問題あり | 14 / 34、6、1 位 | 同じ、1 位 |
| 2 | `apply_agent_event_to_frame` と `fold_until_terminal` | 問題なし | 7 / 7、10、4 位 | 同じ、6 位 |
| 3 | `AgentdHandle` と `TerminaldHandle` | 問題あり | 1 / 0、1、39 位 | 断片ごとに 4 組に分かれ、3 / 0・1(34 位)、2 / 1・1(35、36 位)、1 / 0・1(101 位) |
| 4 | `Store.project_event` と `fold_until_terminal` | 問題なし | 5 / 8、9、6 位 | 同じ、8 位 |
| 5 | mock provider の `start_session` と `respond_to_user_message` | 問題なし | 2 / 3、3、11 位 | 同じ、14 位 |
| 6 | `command_for` と `term_key_code` | 問題なし | 17 / 0、0、2 位 | 同じ、4 位 |
| 7 | `apply_agent_event_to_frame` と `Store.project_event` | 問題なし | 10 / 6、17、3 位 | 同じ、5 位 |
| 8 | 2 つの `AgentFrameItem` | 問題なし | 0 / 1、0、43 位 | 同じ、104 位 |
| 9 | `apply_agent_event_to_frame` と `render_agent_transcript` | 問題あり(小) | 8 / 5、21、5 位 | 同じ、7 位 |
| 10 | `setup.rs` の `tests.real_git` と `worktree.rs` | 問題あり(小) | 4 / 0、0、14 位 | 同じ、22 位 |

1・2・4〜9 番の断片は 1 つの unit の中にあり、`overlap` でも側は同じ unit 1 つだった。順位が下がったのは、新しく入った組に抜かれたためである。10 番の側 b(`worktree.rs:56-`、本番の `run_git` とテストの補助関数の間)は `contain` では解決しなかったが、`overlap` では `run_git` と `scrub_git_env` になった。境(側の unit の最初のコミットの最も遅いもの)の後に側 b の変更が無く、数は変わらなかった。側 a は `tests.real_git` のモジュールと、その中の `git`・`run_git` の 2 つの関数になった。

3 番は、`contain` では 5 つの断片のすべてが 2 つの impl に解決していた。`overlap` では、断片ごとに method の集合になり、どれにも method の間の行で impl が入った。断片ごとの数は次のとおりである(片側 a / b、両側)。

| 断片(a / b の行) | 側 a の集合 | `contain` | `overlap` |
| --- | --- | --- | --- |
| 465-486 / 652-673(exact)、464-486 / 651-673(normalized) | `begin_reload`、`stop_and_wait`、`reload_provider_config`、impl | 1 / 0、1 | 1 / 0、1 |
| 313-331 / 544-562 | `register_agent`、`attach_session`、impl | 1 / 0、1 | 3 / 0、1 |
| 357-371 / 603-616 | `session_list`、`list_providers`、`responder`、impl | 1 / 0、1 | 2 / 1、1 |
| 371-385 / 605-617 | `list_providers`、`list_provider_models`、impl | 1 / 0、1 | 2 / 1、1 |

判定の理由の断片(`begin_reload` と `stop_and_wait`、`src/runtime/mod.rs:472-486` と 659-673)は 1 / 0 のままだった。これは、側の集合の多くの unit に履歴が無いためである。`unit_history` は、`AgentdHandle.begin_reload`、`AgentdHandle.stop_and_wait`、`TerminaldHandle.stop_and_wait` で 0 コミット、`TerminaldHandle.begin_reload` で 2 コミット(a4fd9605、8d8cdd0e)だった。`SessiondHandle.begin_reload` の履歴(67cffe45 の追加、a4fd9605)は、impl の改名で `AgentdHandle.begin_reload` につながっていない。改名のコミットは impl の頭の行だけを変え、method の行に hunk が触れないので、innermost でも `all` でも `unit_changes` にこの method の行が無く、継承の関係(`content_match` は削除と追加の組を見る)ができない。今の unit で `unit_history` が 0 のものは 33 個(method 15、関数 11、impl 3、型 3、module 1)だった。

### 上位 10 件の前と後

| 順位 | `contain` | `overlap` |
| ---: | --- | --- |
| 1 | `spawn` と `WorkspaceShell`(14 / 34、6) | 同じ |
| 2 | `command_for` と `term_key_code`(17 / 0、0) | `NamedColor` と `CommandId`(5 / 16、0)、入った |
| 3 | `apply_agent_event_to_frame` と `project_event`(10 / 6、17) | `explore.rs` の {`fold_until_terminal`、`panic_message`} と `web/mod.rs` の {`panic_message`、`web_runtime`}(17 / 0、0)、入った |
| 4 | `apply_agent_event_to_frame` と `fold_until_terminal`(7 / 7、10) | `command_for` と `term_key_code` |
| 5 | `apply_agent_event_to_frame` と `render_agent_transcript`(8 / 5、21) | `apply_agent_event_to_frame` と `project_event` |
| 6 | `project_event` と `fold_until_terminal`(5 / 8、9) | `apply_agent_event_to_frame` と `fold_until_terminal` |
| 7 | `open_session_manager` と `open_palette`(4 / 3、5) | `apply_agent_event_to_frame` と `render_agent_transcript` |
| 8 | `tools/tests.rs`(unit に解決しない)と `src/board/model.rs` の `tests`(0 / 6、0) | `project_event` と `fold_until_terminal` |
| 9 | `append_record_uncommitted` と `append_event`(2 / 3、11) | `runtime/agent.rs` の {`spawn`、`recover_generation_mismatch`、`Op`} と `runtime/terminal.rs` の {`spawn`、`recover_stale_terminald`、`Op`}(9 / 0、5)、入った |
| 10 | `open_view_chooser` と `open_palette`(3 / 2、5) | `open_session_manager` と `open_palette` |

`contain` の上位 10 件は帰属の記録の後の上位と同じだった。出た 3 件のうち、`tools/tests.rs` の組は、側 a(`tools/tests.rs:187-`)が `overlap` でテストの関数に解決し、両側がテストになったので候補の作り方で除かれた。`append` の組は 12 位、`open_view_chooser` の組は 13 位になった(数は同じ)。

### 入った 3 件の判定

- **`NamedColor` と `CommandId` — 問題なし。** normalized の一致で、端末の色の名前の列挙(`crates/horizon-terminal-core/src/types/color.rs:74-104`)と、ワークスペースのコマンドの列挙(`crates/horizon-workspace/src/commands.rs:1-79`)の、フィールドの無い variant の並びである。領域が違い、一方を変えるともう一方も変えるという関係が無い。読み判定 2 の 6 番(`command_for` と `term_key_code`)と同じ、見かけの一致である。片側 5 / 16 は、それぞれの列挙に variant を足したコミットである。`contain` では、どちらの断片も型の属性の行(`#[derive(…)]`、前置き)から始まり、`start_line`(`pub enum` の行)より前なので、型に含まれず解決しなかった。`overlap` は前置きの行を自分の行に数えるので解決した。
- **2 つの `panic_message` — 問題なし。** パニックの payload を文字列にする 7 行の関数が、`crates/horizon-agent/src/tools/explore.rs:688-694` と `crates/horizon-agent/src/tools/web/mod.rs:240-246` に、今は字句まで同じにある。写しの費用は小さく、変える理由のある関数でもない。片側 17 / 0 は、断片の最初の行(`explore.rs:686`、前の関数 `fold_until_terminal` の閉じ括弧)で入った `fold_until_terminal` の 17 コミットそのもので、`panic_message` の写しとは関係が無い。側 b にも、同じく閉じ括弧の 1 行(`web/mod.rs:238`)で `web_runtime` が入っている。
- **`runtime/agent.rs` と `runtime/terminal.rs` の接続のループ — 問題あり。** `spawn` の本体(`src/runtime/agent.rs:122-261`、`src/runtime/terminal.rs:87-227`)は、デーモンへの接続、`PreHelloTransport` の指数の後退、`SILENCE_MISMATCH_THRESHOLD` までの沈黙の数え上げ、世代の不一致の回復を呼ぶ 2 つの腕からなる約 140 行のループで、名前とログの文言とコメントを置き換えると同じだった(`diff` で残るのは、ログの「initialize」と「hello」、コメント 2 か所、回復の関数の名前と文書と 1 つのエラーの文言だけ)。後退の上限、沈黙の閾値、回復の予算という同じ方針を 2 か所に持ち、方針を変えると 2 か所を直す。b414e425(2026-08-01「Share the daemon plumbing both runtimes duplicated」)は 2 つのランタイムの重複の一部を共有に移したが、このループは残った。片側 9 / 0 のうち 5 つは、断片の最初の行(`Op` の最後の variant の行)で入った `Op` に variant を足したコミット(1e391cfd、8e057dd7、828ea425、cc9a48b6、6f42b611)で、残りの 4 つ(718e77d9、8d8cdd0e、9763a937、8c52fe5a)は、エージェント側の回復の文言のプロトコルの定数名(718e77d9 の差分で確かめた)、`sessiond` の語の整理、ACP v2 への移行だった(718e77d9 以外は題名で見た)。両側 5 のうち 4 つ(a4fd9605、b293051a、b414e425、8fbe878c)はループを 2 か所とも直したコミットである。読んだかぎり、片側の数は伝播の漏れを示していない。判定はループの重複そのものによる。帰属の記録の後の `contain` では、同じ 2 つの `spawn` は断片 249・250(3 / 0、両側 4)として 16 位にあった。`overlap` では、ループの後ろ半分と `spawn_test_stream` にまたがる断片の組(6 / 0、両側 1)も 11 位に入った。

### 側の unit の数と、変わった組

| 側の unit の数 | `contain` | `overlap` |
| ---: | ---: | ---: |
| 0 | 771 | 29 |
| 1 | 2,233 | 1,656 |
| 2 | 0 | 570 |
| 3 以上 | 0 | 749 |

側は 1,502 組 × 2 = 3,004 個である。`overlap` の集合の大きさの最大は 10、`shared_units` が 1 以上の組は 557 だった。集合に入った unit の種類は、関数 4,020、module 512、method 499、impl 203、型 199 だった。`contain` で側が解決していた 1,121 個(側 a)のうち 1,119 個で、その unit が `overlap` の集合にも入っていた。

組の分類の変化(1,502 組):

| | 組 |
| --- | ---: |
| `same_unit` が変わった | 254(真 → 偽 240、NULL → 値 12、値 → NULL 2) |
| 片側の数(`only_a` か `only_b`)が変わった | 392 |
| 片側が 0 から 1 以上になった | 355 |
| 片側が 1 以上から 0 になった | 2 |
| 片側のある組 | 520 → 873 |
| 片側の合計 / 両側の合計 | 1,392 → 2,224 / 4,158 → 4,635 |
| `since_sha` が変わった | 535 |
| どちらかの側が解決しない組 | 393 → 16 |

`same_unit` が真から偽になった 240 組は、`contain` で 2 つの断片が同じ module(211 組。テストのモジュールが多い)か impl(29 組)に解決していた組で、`overlap` では断片ごとに別の関数や method になった。240 組のうち 238 組で、囲む module か impl が空行などの 1 行で両方の集合に入り、`shared_units` が 1 以上だった。片側の数は 82 組で 1 以上になった。片側が 0 になった 2 組は、`palette.rs` と `view_chooser.rs` の `ListDelegate` の impl の組で、`contain` では impl の自分の行の変更が片側ずつ 1 だったのが、`overlap` では method の集合にした後、両側に触れたコミット 1 つになった。両方の数え方で片側がある 518 組では、数が増えたのが 22、減ったのが 8、同じが 488 だった。

断片の境の行について。側と unit の対応 5,433 個を、断片と重なる自分の行の数で分けると、1 行が 670(impl・module・型が 533)、2 行が 612(166)、3〜5 行が 933(94)、6 行以上が 3,218(121)だった。unit が 2 つ以上の側 1,319 個のうち 595 個は、1 行だけで入った unit を持つ。impl・module・型の 1 行は、空行や閉じ括弧、method の間の行で、関数や method の 1 行は、断片の端が前後の item の閉じ括弧か頭の行にかかったものである(2 つの `panic_message` の組がその例)。jscpd の断片は字句の窓で切られるので、端が前後の item に 1 行かかることがある。

境の決め方による違い(5 節)。両側の集合の最初のコミットの最も遅いもの(組んだもの)の代わりに、側ごとに集合の最初のコミットの最も早いものを取り、その 2 つの遅い方を境にすると(測っただけで、組んでいない)、3 番の 371-385 / 605-617 の断片は 2 / 1 から 3 / 5(両側 3)になる。それでも上位 10 件には入らない(その上位の 10 位は片側 12)。その上位 10 件には、`src/lib.rs` のモジュールの宣言(`pub mod …;`)の並びの組が 2 つ(3 位、11 位)と、`judge/mod.rs` と `tools/bash/mod.rs` の承認の種類の型と impl の組(10 位)が入り、接続のループの組は 12 位になった。

## 3. 測定 B:後ろ向き(`production` の範囲)

基準の複製を作り直し、帰属の記録と同じ手順で、正解と信号を作った(既定の変数。正解は innermost)。正解は `clone_sides` に依存しない。refactored は 1 コミット以上で 778、3 コミット以上で 92、preserve は 30 で、帰属の記録の後の値と同じだった。`contain` の値は、帰属の記録の後の値と一致した。

| クローンの信号 | 順位の付く数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 コミット以上、`contain` | 154 | 0.8 | 0.72 | 0.66 | 0.085 |
| 1 コミット以上、`overlap` | 379 | 0.3 | 0.38 | 0.38 | 0.049 |
| 3 コミット以上、`contain` | 154 | 0.3 | 0.2 | 0.19 | 0.207 |
| 3 コミット以上、`overlap` | 379 | 0.1 | 0.12 | 0.10 | 0.109 |

ほかの信号(churn、fan-in、co-change、loc)は `clone_sides` に依存せず、両方で同じ値だった(1 コミット以上の P@100 は churn 0.72、`fan_in_x_commits` 0.48、co-change 0.43、loc 0.75)。

`overlap` の上位を分けると次のとおりである(1 コミット以上の正解)。

| 上位 k | `contain` でも順位があった同一性(正解) | 新しく順位が付いた同一性(正解) |
| ---: | --- | --- |
| 10 | 4(3) | 6(0) |
| 50 | 22(15) | 28(4) |
| 100 | 38(28) | 62(10) |

`contain` の上位 100 の同一性は、`overlap` でもすべて順位があり、順位は中央値で 92 下がった。新しく入った同一性の多くは、`contain` では impl に解決していた断片の method である。`production` の範囲は関数と method だけを数えるので、impl に解決していた断片は、`contain` ではこの範囲に何も足さなかった。たとえば `overlap` の 5〜19 位は、`board_pane/list.rs`、`palette.rs`、`session_manager.rs`、`view_chooser.rs` の `ListDelegate` の impl の `render_empty`、`render_item`、`set_selected_index`、`perform_search`(どれも片側 13)で、範囲の正解に入っていなかった。

## 4. 測定 C:fixture、時間、更新、計画

`check.py fixture` は、既存の fixture のすべてと、広げた 2 つの fixture で通った(全体で 80.6 s)。

- history fixture:既存の組 1(`tests.square_area` と `rect.rs` の 4〜9 行)の側 b は、`contain` では解決せず(片側 1 / 0、両側 0)、`overlap` では型 `Rect`(自分の行 2 行)、impl の頭(1 行)、method `Shape for Rect.area`(2 行)の 3 つになり、片側 0 / 1、両側 1 になる。両方の値を確かめる。
- counting fixture:`src/left.rs` と `src/right.rs` に、同じ impl(method 2 つ。2 つ目の method の前は doc コメントで、間に impl 自身の行は無い)を置き、2 つの組を作った。組 1 は 1 つ目の method の本体から 2 つ目の method の本体まで、組 2 は impl の頭から 1 つ目の method の本体まで。2 つのコミット(左の 1 つ目の method の本体を変える、右の impl の頭を変える)を足し、`overlap` では組 1 の側が method 2 つ(片側 1 / 0、両側 1)、組 2 の側が method と impl(片側 1 / 1、両側 1)、`contain` では両方の組の側が impl(片側 0 / 1、両側 1)になることを、集合と数で確かめる。`cache.sql` を通した行も、両方の値で確かめる。

キャッシュからのクエリの時間(同じセッションで各 5 回、`real`):前の SQL 1.57〜1.76 s、`contain` 1.45〜1.81 s、`overlap` 1.48〜1.77 s。

`check.py refresh` を HEAD の複製で実行し、2 回の更新(0.35 s、0.31 s)のどちらでも、すべてのビューとクエリ(`clone_change_consistency` を含む)がライブと一致した。コマンド全体は 787.3 s、子のプロセスの RSS の最大は 24.5 GB だった。

`check.py explain`(HEAD の複製)は、前の SQL(複製の中の 0a3f935d の `check.py` と SQL)と後の SQL で、すべての行が同じだった。`clone_change_consistency` は `READ_AST` 2、`READ_TEXT` 11、`READ_JSON` 3。最初の版は `units` を 3 か所で読み、`READ_TEXT` が 15 になったので、側のファイルの unit を物化した CTE で 1 回だけ読むように直した。直した前と後で、`contain` と `overlap` の結果の行の hash の和は同じだった。

## 5. 依頼文が決めていなかったこと

- **自分の行に空行を含めるか。** 依頼文の定義(`[lead_start_line, end_line]` から直接の内側の unit を除いたもの)どおりに、空行を含めた。帰属の innermost は hunk の空行を除くが、自分の行の区間そのものは空行を含む。そのため、method の間の空行に重なる断片は impl を集合に入れる(2 節の 3 番では、どの断片の集合にも impl が入った)。impl の履歴は innermost では少ないので、数への影響は小さい。
- **N2 の境。** 依頼文の「両側の集合が存在するようになってから」を、両側の集合のすべての unit が存在するようになってから、と読み、すべての unit の最初のコミットの最も遅いものにした。前の SQL の `since` の式を、側ごとの unit の行が複数になっても変えずに使う形でもある。側ごとに最も早い unit を取る読み方の値を 2 節の終わりに記した。
- **`kind_a`/`unit_a` の意味。** 集合の代表として、断片と重なる自分の行が最も多い unit にした。
- **`same_unit` の NULL。** 前の式は、側 a だけが解決したとき NULL、側 a が解決しないとき偽だった。`contain` で前の結果を保つため、`overlap` でも同じにした。
- **集合の列の形。** `units_a`、`units_b`、`shared_units` は数にし、集合そのものは `side_units_a`、`side_units_b` の列に置いた。後ろ向きのクローンの信号は、この列のすべての unit を順位付けるように変えた。
- **共有の unit に触れたコミット。** 両方の集合に入っている unit に触れたコミットは両側とした。
- **上位の作り方。** `overlap` では側の組を集合の組でまとめた。集合のすべての unit がテストのとき、その側をテストとした。

## 6. 記録と食い違った点、気づいた点

- **読み判定 2 は、上位 10 件のすべてを M4 の機構で説明していた。** この規則(断片の行と重なる unit の集合)では、10 件のうち 9 件の側は前と同じ unit で、数は変わらなかった。断片が 1 つの関数の中にあれば、囲む unit で数えることと、重なる unit で数えることは同じになる。読み判定 2 が書いた直し方(各コミットで断片の行の本文が変わったかを見る)とは別の規則で、関数の中の断片の外の変更は、この規則でも数に入る。
- **3 番(`AgentdHandle` と `TerminaldHandle`)の 9 / 0 は、数え方の記録の時点の impl の数で、method の数ではなかった。** 帰属の記録は、断片が複数の method にまたがって impl に解決することを 1 / 0 への下がり方の理由としていた。method の集合で数えても、判定の理由の断片は 1 / 0 のままだった。理由の 1 つは系譜の切れ目である(2 節)。
- **impl を改名すると、行に hunk の触れない method の系譜が切れる。** `lineage.sql` の限界(同じコミットだけ、重い書き換え)には書かれていない形である。今の unit の 33 個が履歴 0 だった。
- **`contain` は、型や関数の前置き(属性、doc コメント)の行から始まる断片を解決しなかった。** `start_line` は前置きを含まないためで、`NamedColor` と `CommandId` の組がその例だった。

## 7. 限界

- **判定は、入った 3 件だけ、コードを読んで行った。** ほかは読み判定 2 の判定を引いた。
- **後ろ向きは `production` の範囲だけである。**
- **境の別の読み方は HEAD の上位だけで測り、後ろ向きでは測っていない。**
- **1 つのリポジトリの、上位 10 件**である。
