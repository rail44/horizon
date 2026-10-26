# コードデータベースの数え方の 5 つの規則(2026-10-27)

コードデータベースの SQL 層の 8 つ目の増分である。2 回目の読み判定([読み判定 2](code-db-dogfood-reading-2-2026-10-26.md))の誤報の機構の表と、後ろ向きの確認の 2 回目([後ろ向き 2](code-db-dogfood-retrospective-2-2026-10-26.md))が誤りと示した数え方の規則のうち 5 つを直し、それぞれに fixture の確かめを足し、今の上位の候補と後ろ向きの precision への効き方を測った。SQL と `check.py` だけを変え、Rust は変えていない。観察だけを記し、推奨は書かない。

依頼文によれば、オーナーは 2026-10-26 に、5 つの修正のすべてについて「SQLの修正は全て行ってもらってよいです」と承認した。5 つは [設計文書](../code-database-design.md) の「Re-evaluation after lineage」の節と「Open points」の最初の項目に並ぶものである。前提は、設計文書のその 2 つの節、読み判定 2(機構の表、45 件の候補とその判定。`問題あり` 13、`問題なし` 31、`判断できない` 1)、後ろ向き 2(P@k の表、co-change の P@10 の崩れとその原因)、[系譜の記録](code-db-lineage-2026-10-26.md)、`duckdb-ext/sql/README.md` と `duckdb-ext/sql/` の SQL である。記録の決定は問い直していない。

## 要約

- **5 つの規則を、それぞれ前の数え方に戻す変数つきで組んだ。** 既定は新しい規則で、`attribution := 'all'`、`cochange_link_commits := true`、`clone_before_both := true`、`include_tests := true`、`cycle_edges := 'raw'` の 5 つを与えると、2 回目の記録の数がすべて再現する(HEAD の複製で、行数と 45 件の上位の候補が一致し、基準の複製で後ろ向き 2 の B の列の値が一致した)。
- **今の上位の候補(45 件)のうち、5 つの規則をすべて入れると 19 件が上位から外れた**(上位 10 件、循環は大きい 5 個)。規則ごとには、N1 で 5 件、N2 で 2 件、M1 で 8 件、M5 で 4 件、M6 で 2 個だった。機構の表の見込み(N1 7、N2 6、M1 11、M5 6、M6 4)より、どれも少ない。機構の表の数は「その機構が数や順位を説明する候補」で、外れる数ではない。
- **外れた候補には `問題あり` が 4 件あった。** M1 で `unit_churn` の `session_lifecycle.rs` と `commands.rs` の `WorkspaceShell` impl、M1 で `clone_change_consistency` の `tests.real_git` と `worktree.rs`(`問題あり(小)`)、M5 で `cochange_without_reference` の 2 つの `expect_finished`(`問題あり(小)`)である。前の 1 件は、原因の method(`reconcile`)が代わりに上位に入った。後の 3 件のうち、`commands.rs` の impl と `expect_finished` は、判定の理由がまさに規則の外すもの(impl の中身の組み合わせ、テストの中の写し)だった。どれも変数で前の数え方に戻せる(4 節)。
- **M1 は、定義どおりに組むと、impl の残った数の大半が method の doc コメントと属性の行によるものになった。** 構文木が doc コメントと属性を item の外に置くので、method の doc コメントを変えると impl が変わったことになる。innermost でも `modified` が残る impl 853 件のうち、自分の行の変更にコードの行を含むのは 51 件で、692 件は doc コメントか属性、110 件は空行かコメントだけだった。これは依頼文の定義が決めていない場合で、別の扱いの効き方を測ったうえで、判断を残した(5 節)。
- **循環は 31 個から 24 個になった。** 8 個が消え、23 個が小さくなって残った(1 個は 2 つに分かれた)。`問題あり(小)` の judge と tools の循環は 40 → 25 モジュールで残り、`judge/mod.rs` と `tools/*` を含んでいる。`判断できない` の `board_flow` と `session` の循環は 11 → 9 で残った。
- **後ろ向きでは、`production` の範囲は co-change だけが良くなり(1 コミット以上の P@10 0.4 → 0.6、P@100 0.41 → 0.49。3 コミット以上の P@100 0.05 → 0.14)、ほかの信号は変わらなかった。** `all` の範囲は、正解そのものが innermost で変わり(1 コミット以上 1,722 → 1,662 個、3 コミット以上 222 → 184 個)、ほとんどの信号の precision が下がった。下がった分の多くは正解の変化による(信号を変えない `loc` でも P@100 0.76 → 0.60)。
- **fixture、更新、計画は前と同じに保たれた。** `check.py fixture`(新しい counting fixture を含む)が通り、HEAD の複製で `check.py refresh` の 2 回の更新がすべてのビューとクエリでライブと一致した。`explain` の `READ_AST` の数は変わらなかった。

## 環境と手順

作業は worker の作業ツリーで行った。ブランチは `origin/main` から作られていたので、依頼文が指定した `origin/code-quality-survey` の e30d981a に合わせた(変更の無い状態で `git reset --hard e30d981a`)。測定はこのリポジトリの使い捨ての複製 3 つで行い、作業ツリーのファイルと履歴は測定に使っていない。

- `target/code-db/head`:e30d981a。Horizon のソースは読み判定 2 の 9a3c83e9 と同じである(その後のコミットは `docs/` だけを変えた)。
- `target/code-db/retro/base`:e3c619a0(後ろ向きの基準の木)。
- `target/code-db/retro/end`:46c37290(リファクタリングの範囲の終わり)。

機械はオーナーの対話用デスクトップで、他の作業と共有している(測定中の 1 分の load average は 1〜6)。DuckDB CLI 2.0.0(`~/.local/bin/duckdb2`)、sitting_duck v1.16.0、rust-analyzer、jscpd 5.3.1 を使い、すべて `nice -n 19` で動かした。測定用の SQL とシェル(上位の候補を DuckDB のファイルに書き出すもの、P@k の表を作るもの)は `target/code-db/measure/` に置き、コミットしていない。

```sh
nice -n 19 duckdb-ext/build.sh
uv run --locked --script scripts/refactor-audit/run.py setup
git clone -q --no-checkout . target/code-db/head            # その後 checkout e30d981a
duckdb-ext/sql/run.sh index target/code-db/head             # jscpd 0.8 s、rust-analyzer scip 141.4 s
# 規則ごとの上位の候補(m_label ごとに変数を変える。r2 は 5 つの変数で前の数え方)
duckdb-ext/sql/run.sh sql target/code-db/head target/code-db/measure/top.sql m_out=… m_label=r2 \
    attribution=all cochange_link_commits=true clone_before_both=true include_tests=true cycle_edges=raw
# 後ろ向き(後ろ向き 2 の 9 節と同じ手順に attribution を足したもの)
duckdb-ext/sql/run.sh index target/code-db/retro/base       # rust-analyzer scip 157.6 s
duckdb-ext/sql/jscpd.sh target/code-db/retro/end target/code-db/retro/end/target/code-db/jscpd src crates
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/changes.sql retro_since=e223bc20 attribution=… retro_changes=…
duckdb-ext/sql/run.sh sql END duckdb-ext/sql/retro/lineage.sql retro_since=e223bc20 attribution=… …
duckdb-ext/sql/run.sh sql BASE duckdb-ext/sql/retro/signals.sql [5 つの変数] retro_changes=… retro_lineage_changes=… …
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/head
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/head
```

## 1. 組んだ規則

規則の名前は機構の表のもの(読み判定 2 の 6 節)である。

| 規則 | 組んだもの | 前に戻す変数 |
| --- | --- | --- |
| **N1** 系譜のリンクのコミットを co-change の共有コミットから除く | `unit_history` に `via_link` を足した。その unit の系譜が識別子を前身につないだコミット(`unit_lineage` の `sha`、つまり `file_rename`・`content_match`・`body_move` のコミット)である。`unit_commits` はこのコミットを除くので、`unit_cochange`・`edge_cochange`・`cochange_without_reference` のどれでも、そのコミットはその unit のコミットに数えない。`unit_churn` は数えたまま、そのうちの数を `move_commits` の列で示す | `cochange_link_commits := true` |
| **N2** クローンの片側だけの変更は、両側がそろってから数える | `clone_change_consistency` は、組ごとに 2 つの側の unit の系譜の最初のコミット(`unit_history` の最初)のうち遅い方を求め、それ以後(そのコミットを含む)のコミットだけを片側か両側に数える。組ごとに `since_sha`・`since_time` を返す | `clone_before_both := true` |
| **M1** 変更した行は、その行を含む最も内側の unit だけに付ける | `unit_change_sides` は、各 unit について、その行の範囲から入れ子の unit(`blob_units.parent_unit_ord`)の範囲を除いた行(自分の行)を作り、hunk が自分の行に触れた unit だけを変わったとする。両側(古い側と新しい側)で同じである。`attribution := 'all'` は前の定義(範囲のどこかに触れれば変わった)。`unit_change_sides` は両方の結果を列に持つ(`change_kind` は `attribution` のもの、`change_kind_all` は `all` のもの) | `attribution := 'all'` |
| **M5** テストの unit を co-change の組から除く | `unit_cochange` は、`unit_tests` でテストの unit のコミットを組にする前に除く。`cochange_without_reference` はそれを読むので同じになる。`edge_cochange` はテストの印つきのまま残す | `include_tests := true` |
| **M6** 構造上の辺をまとめてから循環を求める | `modules` に `module_path` を足し、Rust のファイルとモジュールの対応でモジュールの木の中の位置を表した(`lib.rs`・`main.rs`・`mod.rs` はそのディレクトリ、ほかのファイルは `.rs` を除いたパス、インラインのモジュールはファイルの `module_path` に名前を続けたもの)。`module_edges` に 2 つの列を足した。`nested_modules`(同じクレートで一方が他方の祖先か子孫)と `same_type_unit_edges`(両端がともに同じ型の impl の中にある unit の辺の数。各端の最も近い impl を `impl_for` で型に解決する)である。`module_cycle_edges` は、`nested_modules` の辺と、unit の辺がすべて同じ型の辺(`same_type_unit_edges = unit_edges`)を除いてから強連結成分を作る。`module_edges` の行は前と同じで、生の辺はそこに残る | `cycle_edges := 'raw'` |

**系譜の中の M1。** 継承の関係(`derived_unit_successions`)は、`attribution` にかかわらず `all` の結果(`change_kind_all`)を読む。理由は 2 つある。1 つは、unit が移ったかどうかを本文全体(入れ子の unit を含む)で判断する規則だからで、method をすべて外に移した impl は、`all` では本文の変わった `modified` の impl になり、その前の像が別の追加された impl に似ていれば `body_move` になる。innermost では、その impl の自分の行(頭と閉じ括弧)は変わらないので、`modified` にならず、`body_move` にもならない。もう 1 つは、継承の関係は 2 つのキャッシュが保存するので、数えるクエリの `attribution` に依存しない方がよいことである(`refresh.sql` の `lineage_key` に `unit_change_sides` の本文を足したが、`attribution` は入れていない)。系譜に沿って数えるコミット(`unit_history`)は `attribution` に従う。HEAD の複製で、継承の関係は前と同じ 20,970 行だった。

`file_cochange` と `file_cochange_thresholds` は、ファイルの変更をそのまま数え、系譜も unit も読まないので、N1 の影響を受けない。変えていない。

## 2. 測定 A:今の上位の候補

### 数

`r2` は 5 つの変数で前の数え方にしたもの、`fixed` は既定(5 つの規則すべて)、ほかの列は `r2` に 1 つの規則だけを入れたものである。

| | r2 | N1 | N2 | M1 | M5 | M6 | fixed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `unit_changes` の行 | 40,924 | 40,924 | 40,924 | 39,434 | 40,924 | 40,924 | 39,434 |
| `unit_cochange` の組 | 23,125 | 13,692 | 23,125 | 19,212 | 10,221 | 23,125 | 4,163 |
| `cochange_without_reference` の組 | 8,424 | 4,312 | 8,424 | 7,540 | 3,614 | 8,424 | 1,239 |
| `clone_change_consistency` の片側のある行 | 574 | 574 | 532 | 574 | 574 | 574 | 526 |
| 循環(内側の辺、モジュール) | 31(354、166) | 31 | 31 | 31 | 31 | 24(159、94) | 24(159、94) |

`r2` の数(`unit_churn` 9,264 行、`unit_fan_in_churn` 2,915 行、`clone_change_consistency` 1,502 行、片側のある行 574、`cochange_without_reference` 8,424 組、`unit_cochange` 23,125 組、循環 31 個)は読み判定 2 と同じで、上位 10 件と 5 個も 45 件すべてが同じだった。

`unit_changes` の種類ごとの `modified` の行は、`all` → innermost で、impl 1,917 → 853、module 1,297 → 874、type 1,536 → 1,535、function 7,187 → 7,185(入れ子の関数)、method 2,957 → 2,957 だった。`added` と `removed` はどの種類も変わらない(追加・削除される unit は、頭と閉じ括弧が自分の行なので必ず付く)。

### 規則ごとに上位から外れた候補

読み判定 2 の 45 件(各信号の上位 10 件と循環の大きい 5 個)のうち、規則を入れると上位から外れたもの。上位の作り方は読み判定 2 と同じである(`unit_churn` はテストでない unit の上位 10 件、`unit_fan_in_churn` はクエリの順、`clone_change_consistency` は片側のある行から両側がテストの行を除き、同じ 2 つの側の組を 1 つにまとめて(数は組の中の最大)`one_sided` と `both_sides` の降順、`cochange_without_reference` はクエリの順、循環はモジュールの数の降順)。循環は、前の循環の部分集合として残った循環があるかで対応させた。

| 規則 | 外れた数 | 外れた候補(読み判定 2 の番号、判定) | 機構の表の見込み |
| --- | ---: | --- | --- |
| N1 | 5 | co-change 6〜10(すべて `問題なし`) | 7(co-change 2、3、6〜10) |
| N2 | 2 | クローン 5(mock provider の 2 つの shutdown)、8(2 つの `AgentFrameItem`)(どちらも `問題なし`) | 6(クローン 1〜5、8) |
| M1 | 8 | churn 3、5、7、8、fan-in 4、8、クローン 10、co-change 10 | 11(churn 3・5・6・7・8、fan-in 2・4・5・8・9、クローン 10) |
| M5 | 4 | co-change 1、2、3、8 | 6(co-change 1・2・3・8、クローン 9・10) |
| M6 | 2 | 循環 2(`src/workspace/*` と `pane.rs`)、3(`registry.rs` と provider)(どちらも `問題なし`) | 4(循環 1 の `tools/` の部分、2、3、5) |
| すべて | 19 | churn 3・5・7・8、fan-in 4・8、クローン 5・8・10、co-change 1〜3・6〜10、循環 2・3 | — |

見込みと違った分の理由は次のとおりである。

- **N1 の co-change 2・3 は残った。** 2 組とも、共有 7 のうちリンクのコミットは 1 つで、除いても 6 である。上位から外れたのは M5(両方がテストの unit)による。6〜10 番は読み判定 2 の見込みどおり、6 番は共有 3、7〜10 番は共有 2 になった(6 番は `min_shared` 3 を満たすが、結合 100% で共有 4〜5 の組に抜かれる)。
- **N2 は 6 組すべての片側を減らしたが、上位に残ったものが多い。** 1 番 28 / 34 → 14 / 34、2 番 19 / 7 → 7 / 7、3 番 25 / 0 → 11 / 0、4 番 14 / 8 → 5 / 8、5 番 3 / 19 → 3 / 2、8 番 14 / 1 → 片側 1(その 1 は側 b の作成のコミット)。この値は読み判定 2 の表の「共存後の片側」の列と同じだった。1〜4 番は減っても上位 10 の中で並びが変わっただけである。
- **M1 の見込み 11 件のうち 4 件は残った。** churn 6 番の `horizon-workspace` の `Workspace` impl(43 → 36 コミット、7 位)、fan-in 2・5・9 番の `ToolSessionState`・`AgentdState`・`WriterHandle` の impl(39 → 36、34 → 24、22 → 20 コミット)。5 節の、impl に残った doc コメントと属性の行の分である。逆に、見込みに無かった co-change 10 番は、数は変わらず(共有 5、100%)、M1 で数の変わった別の組に抜かれて 11 位になった。
- **M5 はクローンに効かない。** 機構の表はクローン 9・10 番を M5 に数えたが、依頼文の M5 は co-change の組だけを対象にしている。
- **M6 の見込みのうち、循環 1 と 5 は小さくなって残った**(下の循環の表)。

### 外れた `問題あり`

外れた 19 件のうち `問題あり` は 4 件だった。

1. **`session_lifecycle.rs` の `WorkspaceShell` impl(churn 5 番、M1)。** 44 → 24 コミット、24 位。読み判定 2 は、この impl を「`reconcile` と同じ原因(ペインの種類ごとの分岐が `reconcile` と `spawn_agent_resume` にある)」として `問題あり` にした。原因は method の中にあり、規則を入れた後は `WorkspaceShell.reconcile` が 6 位に入った(38 コミット、1 回目の判定で `問題あり`)。規則は、同じ原因を指す候補を impl から method に移した形である。
2. **`commands.rs` の `WorkspaceShell` impl(churn 7 番、M1)。** 42 → 16 コミットで、上位 30 件の外になった。判定の理由は、ボードのセッションの処理が約 300 行、コマンドの dispatcher と同じ impl にあること(`commands.rs:136-448`)で、impl の中身の組み合わせそのものである。innermost の数は impl の自分の行の変更しか数えないので、この種の問題を数では指せない。規則が誤っているのか判定が誤っているのかを、この 1 件から分けることはできない。M1 の前提(入れ子の unit の数は中の変更の和で誤報になる)は、読み判定 2 で同じ機構に数えた `問題なし` の 7 件には当てはまり、この 1 件には当てはまらない。
3. **`setup.rs` の `tests.real_git` と `worktree.rs`(クローン 10 番、`問題あり(小)`、M1)。** 片側 11 / 0 → 6 / 0、11 位。読み判定 2 は、この片側の数を「側 b が解決しないので、側 a のテストモジュールの変更の数そのもの」とし、写しの食い違いは断片を読んで見つけたもので片側の数とは関係が無いとしていた。M1 は、テストモジュールの数を自分の行の変更に減らした。数が欠陥を指していなかったことは前と同じである。
4. **2 つの `expect_finished`(co-change 1 番、`問題あり(小)`、M5)。** 両方がテストの unit なので、M5 がそのまま除いた。判定の理由は、同じ補助関数が 2 つのテストファイルに写されていることで、テストの中の写しである。M5 の前提(テストの unit の組は、テストと対象が一緒に変わるか、1 つの型から書いたテストどうし)には当てはまらない組である。

このほか、`問題あり` の `AgentdHandle` と `TerminaldHandle`(クローン 3 番)は上位に残ったが、片側の数が 25 / 0 → 9 / 0 に減った。M1 だけで 16 / 0、N2 だけで 11 / 0 である。断片が複数の method にまたがるので、側は断片を含む最も小さい unit の impl に解決し、innermost では impl の数に method の変更が入らない。M1 と、クローンの側を囲む unit で数える数え方(機構 M4、今回は直していない)の組み合わせで起きる。

### 循環

31 個の循環のうち、8 個は消え、23 個は小さくなって残った。残った循環は、前の循環の部分集合である。

| 前の循環(最小の要素) | 前のモジュール / 辺 | 後 |
| --- | --- | --- |
| `judge/chat.rs`(1 番、`問題あり(小)`) | 40 / 106 | `judge/handle.rs` 25 / 64(`judge/handle.rs`、`judge/mod.rs`、`policy.rs`、`tools/*` 22 個) |
| `src/agent/view/pane.rs`(2 番、`問題なし`) | 13 / 41 | 2 / 2(`pane.rs` と `src/workspace/mod.rs`) |
| `providers/mock.rs`(3 番、`問題なし`) | 12 / 38 | 3 / 4(`mock.rs`、`rig/mod.rs`、`registry.rs`)と 3 / 5(`rig/session/continuation.rs`、`input.rs`、`state.rs`) |
| `board_flow/dispatch.rs`(4 番、`判断できない`) | 11 / 21 | `board_flow/operations.rs` 9 / 16(`board_flow/operations.rs` と `session/*` 8 個) |
| `rig/clearing.rs`(5 番、`問題なし`) | 9 / 25 | 5 / 9 |
| `transcript/diff.rs` | 8 / 17 | 7 / 13 |
| `horizon-workspace/src/layout.rs` | 7 / 18 | `mode.rs` 3 / 4 |
| `src/terminal/keyboard.rs` | 5 / 10 | `paint.rs` 2 / 2 |
| `horizon-board/src/store/logd.rs`、`horizon-logd/src/writer.rs` | 4 / 7 | 3 / 4、2 / 2 |
| ほかの 3 モジュールの 6 個、2 モジュールの 13 個 | | 13 個が残った(3 モジュールの 5 個と 2 モジュールの 8 個。大きさは同じか小さい) |
| 消えたもの | | `tools/input/bounded.rs`(5)、`duckdb/append.rs`(4)、`contract.rs`(3)、`horizon-agentd/src/hub.rs`、`horizon-cli/src/cli.rs`、`horizon-sandbox/src/lib.rs`、`horizon-workspace/src/persistence.rs`、`src/agent/view/mod.rs`(各 2) |

後の大きい 5 個は、`judge/handle.rs`(25)、`board_flow/operations.rs`(9)、`transcript/diff.rs`(7)、`rig/clearing.rs`(5)、`horizon-agent/src/config.rs`(3。3 モジュールの循環は 8 個あり、名前の順で最初のもの)である。2 番は、`src/workspace/*` の `WorkspaceShell` の impl どうしの辺が同じ型の辺としてまとめられ、読み判定 2 の判定のもう 1 つの理由(コマンドへの結び付け)の `pane.rs` と `mod.rs` の 2 つが残った。1 番の判定が指した辺(judge の中の `start_approval_gate` と tools の間)は残った循環の中にある。

### 規則を入れた後の上位

各信号の上位 10 件。入った候補は判定していない。

- **`unit_churn`**:`run_session`(50)、`build_definitions`(49)、`WorkspaceShell.execute`(45)、`WorkspaceShell.new`(41)、`WorkspaceShell` 型(40)、`WorkspaceShell.reconcile`(38、入った)、`horizon-workspace` の `Workspace` impl(36)、`ToolSessionState` impl(36、入った)、`run_provider_stream`(35、入った)、`Render for WorkspaceShell.render`(34、入った)。11・12 位は `spawn_rig_session`、`TerminalSession.spawn`(34)。`move_commits`(リンクのコミットの数)は、`run_session` 3、`build_definitions` 5、`run_provider_stream` 5、ほかは 1〜2 だった。
- **`unit_fan_in_churn`**:`Event`、`ToolSessionState` impl(39 → 36)、`AgentdState` 型、`Command`、`ToolCallResult`、`RigAgentConfig`、`WriterHandle` impl、`ToolCallRequest`(入った)、`MessageRole`(入った)、`AgentdState` impl(34 → 24)。
- **`clone_change_consistency`**:`spawn` と `WorkspaceShell`(14 / 34)、`command_for` と `term_key_code`(17 / 0)、`apply_agent_event_to_frame` と `project_event`(10 / 6)、`apply_agent_event_to_frame` と `fold_until_terminal`(7 / 7)、`apply_agent_event_to_frame` と `render_agent_transcript`(8 / 5)、`project_event` と `fold_until_terminal`(5 / 8)、`AgentdHandle` と `TerminaldHandle`(9 / 0)、`WorkspaceShell.open_palette` と `open_session_manager`(3 / 4、入った)、`tools/tests.rs`(unit に解決しない)と `src/board/model.rs` の `tests`(0 / 7、入った)、`horizon-workspace` の `persistence.rs` の `tests` と `tests.rs`(7 / 0、入った)。
- **`cochange_without_reference`**:`NamedProviderConfig` と `RawConfig.resolved_providers`、`NamedProviderConfig` と `ResolvedProviderConfig`(読み判定 2 の 4・5 番)、`AgentView.render_composer_hints` と `stop_button`、`ProviderSummary` と `RawNamedProviderConfig`、`ViewKindState` と `ViewKind` の 4 組、`placeholder` と `task_lines`、`placeholder` と `turn_chip`(どれも結合 100%、共有 4〜5)。

## 3. 測定 B:後ろ向き

後ろ向き 2 と同じ手順で、正解を 2 つの数え方で作った。範囲の終わりの複製の `retro/changes.sql` と `retro/lineage.sql` を `attribution` だけ変えて動かした。

| | `all`(後ろ向き 2 と同じ) | innermost |
| --- | ---: | ---: |
| 範囲の unit の変更の行 / コミット / 同一性 | 4,771 / 84 / 3,415 | 4,511 / 84 / 3,354 |
| 系譜の始点(`modified` と `removed`) | 3,086 | 2,826 |
| 正解の refactored、`all`・1 コミット以上 | 1,722 / 6,903(24.9%) | 1,662 / 6,903(24.1%) |
| 同、`all`・3 コミット以上 | 222(3.2%) | 184(2.7%) |
| 同、`production`・1 コミット以上 | 760 / 2,583(29.4%) | 760(29.4%) |
| 同、`production`・3 コミット以上 | 89(3.4%) | 89(3.4%) |
| preserve | 32(本番 30) | 32(本番 30) |

`all` の正解の数と範囲のリンク(460 本、切れ目 24 個)は後ろ向き 2 と同じだった。`production` は関数と method だけなので、正解は変わらない。

信号は `R2`(5 つの変数で前の数え方)と `fixed`(既定)の 2 つで、各欄は「R2 を `all` の正解で / fixed を innermost の正解で(差)」である。`R2` の値は後ろ向き 2 の B の列とすべて一致した。角括弧の中は分解のための 2 つの組み合わせで、[fixed を `all` の正解で、R2 を innermost の正解で] である。`module_cycles` は集合で、欄は集合の大きさと正解に入る数である。

### `all`、1 コミット以上

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 6,903 | 1.00 / 0.90(-0.10)[1.00、0.70] | 0.82 / 0.80(-0.02)[0.84、0.64] | 0.79 / 0.66(-0.13)[0.76、0.66] | 0.05 / 0.04 |
| `loc` | 6,903 | 0.90 / 0.80(-0.10)[0.90、0.80] | 0.80 / 0.64(-0.16)[0.80、0.64] | 0.76 / 0.60(-0.16)[0.76、0.60] | 0.04 / 0.04 |
| `clone_one_sided` | 652 | 0.80 / 0.60(-0.20)[0.60、0.60] | 0.76 / 0.60(-0.16)[0.66、0.72] | 0.81 / 0.68(-0.13)[0.71、0.79] | 0.05 / 0.04 |
| `fan_in_x_commits` | 2,107 | 0.90 / 0.60(-0.30)[0.70、0.80] | 0.68 / 0.58(-0.10)[0.64、0.56] | 0.57 / 0.47(-0.10)[0.54、0.50] | 0.03 / 0.03 |
| `cochange_without_reference` | 1,265 → 391 | 0.50 / 0.70(+0.20)[0.70、0.50] | 0.46 / 0.34(-0.12)[0.34、0.44] | 0.46 / 0.29(-0.17)[0.31、0.45] | 0.03 / 0.02 |
| `fan_in` | 2,107 | 0.30 / 0.30(±0)[0.30、0.30] | 0.40 / 0.40(±0)[0.40、0.40] | 0.34 / 0.31(-0.03)[0.34、0.31] | 0.02 / 0.02 |
| `module_cycles`(集合) | 1,551 → 852 | | | 601 / 296 個 [307、582] | |

### `all`、3 コミット以上

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 6,903 | 0.80 / 0.60(-0.20)[0.70、0.50] | 0.38 / 0.34(-0.04)[0.40、0.24] | 0.35 / 0.25(-0.10)[0.31、0.21] | 0.16 / 0.14 |
| `loc` | 6,903 | 0.60 / 0.10(-0.50)[0.60、0.10] | 0.44 / 0.14(-0.30)[0.44、0.14] | 0.38 / 0.17(-0.21)[0.38、0.17] | 0.17 / 0.09 |
| `clone_one_sided` | 652 | 0.40 / 0.20(-0.20)[0.20、0.40] | 0.30 / 0.22(-0.08)[0.24、0.28] | 0.24 / 0.18(-0.06)[0.22、0.22] | 0.11 / 0.10 |
| `fan_in_x_commits` | 2,107 | 0.50 / 0.40(-0.10)[0.50、0.40] | 0.38 / 0.24(-0.14)[0.36、0.24] | 0.27 / 0.18(-0.09)[0.25、0.18] | 0.12 / 0.10 |
| `cochange_without_reference` | 1,265 → 391 | 0.00 / 0.00(±0)[0.00、0.00] | 0.00 / 0.06(+0.06)[0.06、0.00] | 0.03 / 0.06(+0.03)[0.06、0.03] | 0.01 / 0.03 |
| `fan_in` | 2,107 | 0.20 / 0.20(±0)[0.20、0.20] | 0.14 / 0.14(±0)[0.14、0.14] | 0.13 / 0.10(-0.03)[0.13、0.10] | 0.06 / 0.05 |
| `module_cycles`(集合) | 1,551 → 852 | | | 77 / 46 個 [54、65] | |

### `production`、1 コミット以上

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.90 / 0.90(±0) | 0.76 / 0.76(±0) | 0.71 / 0.71(±0) | 0.09 / 0.09 |
| `loc` | 2,583 | 0.80 / 0.80(±0) | 0.78 / 0.78(±0) | 0.75 / 0.75(±0) | 0.10 / 0.10 |
| `clone_one_sided` | 154 | 0.80 / 0.80(±0) | 0.76 / 0.74(-0.02) | 0.68 / 0.65(-0.03) | 0.09 / 0.09 |
| `fan_in_x_commits` | 1,307 | 0.80 / 0.80(±0) | 0.52 / 0.52(±0) | 0.43 / 0.43(±0) | 0.06 / 0.06 |
| `cochange_without_reference` | 493 → 256 | 0.40 / 0.60(+0.20) | 0.36 / 0.46(+0.10) | 0.41 / 0.49(+0.08) | 0.05 / 0.06 |
| `fan_in` | 1,307 | 0.20 / 0.20(±0) | 0.32 / 0.32(±0) | 0.29 / 0.29(±0) | 0.04 / 0.04 |
| `module_cycles`(集合) | 1,125 → 580 | | | 471 / 232 個 | |

### `production`、3 コミット以上

| 信号 | 順位の付いた数 | P@10 | P@50 | P@100 | recall@100 |
| --- | ---: | ---: | ---: | ---: | ---: |
| `unit_churn` | 2,583 | 0.50 / 0.50(±0) | 0.34 / 0.34(±0) | 0.30 / 0.30(±0) | 0.34 / 0.34 |
| `loc` | 2,583 | 0.50 / 0.50(±0) | 0.36 / 0.36(±0) | 0.29 / 0.29(±0) | 0.33 / 0.33 |
| `clone_one_sided` | 154 | 0.40 / 0.30(-0.10) | 0.18 / 0.18(±0) | 0.18 / 0.19(+0.01) | 0.20 / 0.21 |
| `fan_in_x_commits` | 1,307 | 0.60 / 0.60(±0) | 0.24 / 0.24(±0) | 0.16 / 0.16(±0) | 0.18 / 0.18 |
| `cochange_without_reference` | 493 → 256 | 0.00 / 0.00(±0) | 0.02 / 0.12(+0.10) | 0.05 / 0.14(+0.09) | 0.06 / 0.16 |
| `fan_in` | 1,307 | 0.20 / 0.20(±0) | 0.08 / 0.08(±0) | 0.05 / 0.05(±0) | 0.06 / 0.06 |
| `module_cycles`(集合) | 1,125 → 580 | | | 57 / 40 個 | |

`production` の正解は 2 つの数え方で同じなので、`production` の分解の値は省いた(fixed を `all` の正解で数えた値は、innermost の正解での値と同じだった)。

### 変わったこと、変わらなかったこと

- **`production` では、変わったのは co-change と クローンだけだった。** co-change は、N1 と M5 で組が 493 → 256 に減り、1 コミット以上の P@10 が 0.4 → 0.6、P@100 が 0.41 → 0.49、3 コミット以上の P@100 が 0.05 → 0.14 になった。後ろ向き 2 で系譜を入れて下がった P@10(名前で数えた第 1 回の 0.9 → 0.4)は、0.6 まで戻った。クローンは N2 と M1 で 0.01〜0.10 動いた。`unit_churn`、`fan_in_x_commits`、`fan_in`、`loc` は、関数と method の数がほとんど変わらないので同じだった。
- **`all` の範囲の下がり方の多くは、正解の変化による。** 信号を変えない `loc` が、innermost の正解では 1 コミット以上の P@100 0.76 → 0.60、3 コミット以上 0.38 → 0.17 に下がった。innermost では、method だけが変わった impl やモジュールが「範囲で変更された」に入らなくなり(1 コミット以上で 60 個、3 コミット以上で 38 個減った)、`loc` の上位にある大きな impl やモジュールが正解から外れた。分解の値では、`unit_churn` の 1 コミット以上の P@100 は、信号だけを変えると 0.79 → 0.76、正解だけを変えると 0.79 → 0.66 である。
- **`all` の co-change は、P@10 が上がり、P@50・P@100 が下がった。** 順位の付いた数が 1,265 → 391 に減った。上位 100 の入れ替わりの内訳は数えていない。分解の値では、P@100 の下がり方(0.46 → 0.29)は信号の変化(0.46 → 0.31)でほぼ決まる。
- **`module_cycles` の集合は 1,551 → 852 個(本番 1,125 → 580)になった。** 正解に入る割合は、`all`・1 コミット以上で 0.39 → 0.35(296 / 852)、本番・1 コミット以上で 0.42 → 0.40(232 / 580)だった。

## 4. 測定 C:fixture、更新、計画

### fixture

`check.py fixture` は、既存の 6 つの fixture と新しい counting fixture のすべてで通った。既存の fixture は次のように変えた。

- history fixture:`unit_changes` を 2 つの数え方で確かめる。innermost では、method の中の変更で impl が変わったことにならない(`Square`、`Shape for Square`、`Shape for Rect` の 5 行が消える)。`all` では前と同じ行。`unit_change_sides` の `change_kind_all` が `attribution` によらないこと。`Shape for Square` impl の churn は (3, 2) → (1, 1)(自分の行が変わったのは追加のときだけ)。`clone_change_consistency` の 2 組の `since_sha` は最初のコミット。
- graph fixture:co-change の行を `attribution := 'all'` で数える(前の値のまま)。`module_edges` の 2 つの列(インラインのモジュールとそのファイル、`src/cycle_b.rs` とクレートの根 `src/lib.rs` が `nested_modules`)。

counting fixture(`check.py counting-fixture`)は、10 コミットの小さなクレートを作って rust-analyzer で索引し、各規則を前に戻す変数と比べる。

| 規則 | 場合 | 既定 | 前の数え方 |
| --- | --- | --- | --- |
| N1 | 一緒に追加・変更された 2 つの関数を、同じコミットで新しいファイルに移す(`content_match` 2 本) | 移動のコミットは `via_link`、`unit_churn` 3 コミットのうち `move_commits` 1、`unit_cochange` の共有 2 | 共有 3 |
| N2 | 1 つ目の側の 2 コミットの後で、写しの 2 つ目の側を作る | 片側 1 / 1、`since_sha` は写しのコミット | 3 / 1 |
| M1 | impl の method の本体を変え、次に impl の頭を変える | `unit_changes` は method、次に impl | impl は 2 回とも |
| M5 | 2 つの統合テストを 3 回一緒に変える | `unit_cochange` と `cochange_without_reference` に組が無い | 組がある(共有 3) |
| M6 | 親のファイルと子のファイル、同じ型の impl を持つ 2 つのファイルが呼び合う。別に兄弟の 2 つのモジュールが呼び合う | 兄弟の循環だけ | 循環 3 個 |

最後に同じ行を `cache.sql` の上で確かめる。

### 更新と計画

HEAD の複製で、キャッシュを消してからの初回の更新は、`run.sh query … unit_churn LIMIT=3` の全体で 26.6 s、`meta.seconds` の合計で 23.5 s だった(`blob_nodes` 11.1 s、`lineage` 6.4 s、`blob_units` 1.7 s ほか。load average 5)。系譜の記録は 24.5〜24.7 s(`lineage` 5.5 s)、読み判定 2 は 42.6 s(load average 26)だった。何も変わっていない更新は 0.27 s(記録は 0.26〜0.64 s)。継承の関係は 1,618 コミットを導出して 20,970 行で、記録の行数と同じだった(コミットは記録の 1,615 より 3 つ多い)。

`check.py refresh` を HEAD の複製で 1 回実行し、2 回の更新のどちらでも、すべてのビュー(系譜の 5 つを含む)とクエリがライブと一致した。更新は 0.47 s と 0.32 s で、コマンド全体はおよそ 13 分だった。

`check.py explain` の `READ_AST` の数は、前の SQL(同じ複製、e30d981a の `duckdb-ext/sql`)と同じだった。構文のビュー(`nodes`、`units`、`tags`)は 1、履歴を読むクエリは 2(今のファイルと blob)。増えたのは `READ_TEXT` と `READ_SCIP` で、`unit_cochange` と `cochange_without_reference` の `READ_TEXT` が 18 → 21(`unit_tests` が `tags` を読む分)、`module_graph` が `READ_TEXT` 13 → 21・`READ_SCIP` 1 → 2、`module_cycles` が `READ_TEXT` 41 → 57・`READ_SCIP` 2 → 3(`module_edges` が `impl_for` を読む分)だった。ライブの計画だけで、キャッシュの上ではどれも読まない。

## 5. うまくいかなかったこと

- **M1 の定義どおりの実装では、impl の数の多くが残った。** 依頼文の M1 は「囲む unit は、自分の中にあり、どの内側の unit にも無い行(頭、フィールド、属性)だけで数える」である。tree-sitter の構文木では、item の属性と doc コメントは item の兄弟の節で、item の行の範囲の外にある。そのため、method の doc コメントや `#[…]` の行、method の間の空行は、method ではなく impl の自分の行になる。method を足すと、ふつうは doc コメントと区切りの空行も足すので、impl も変わったことになる。innermost でも `modified` が残った impl 853 件と module 874 件について、自分の行の変更を本文で分けると次のとおりだった(新旧の両側。HEAD の複製の全履歴)。

  | | コードの行を含む | doc コメントか属性を含む(ほかは空行かコメント) | 空行かコメントだけ |
  | --- | ---: | ---: | ---: |
  | impl(853) | 51 | 692 | 110 |
  | module(874) | 329 | 525 | 20 |

  依頼文の括弧の中の「属性」が、囲む unit 自身の属性(構文木では外にあるので、もともと範囲に入らない)を指すのか、内側の item の属性を指すのかは、定義から決まらない。実装は定義の文言どおり(内側の unit の範囲だけを除く)にして既定にし、判断を残した。比べるために、「自分の行の変更がコードの行を含まない impl とモジュールの `modified` を数えない」という扱いを、測定のセッションだけで `unit_changes` を差し替えて測った(コミットしていない)。この扱いでは、機構の表が M1 に数えた 11 件はすべて上位から外れ(churn 3・5・6・7・8、fan-in 2・4・5・8・9、クローン 10)、`unit_churn` と `unit_fan_in_churn` の上位 10 件から impl が無くなった。一方で、`問題あり` の `AgentdState` impl(fan-in 5 番。同じ原因の型は 2 位に残る)と、`AgentdHandle` と `TerminaldHandle`(クローン 3 番)も外れた。後者は、断片を含む impl が側になり、method の変更が数えられなくなるためである。この扱いは本文の分類(空行、`//`、`#[`)を使うので、言語の知識を `lang/` の外に持ち込むことにもなる。
- **M1 はクローンの側の数を減らす。** `clone_change_consistency` は、断片を含む最も小さい unit を側にするので、複数の method にまたがる断片の側は impl になる。innermost では、その impl の数に method の変更が入らない。`AgentdHandle` と `TerminaldHandle` は 25 / 0 → 16 / 0(M1 だけ)になった。機構 M4(片側の数を断片で数える)は今回の 5 つに入っていない。
- **M5 と M1 は、`問題あり` を 1 件ずつ、判定の理由そのものの形で外した**(2 節の 2 番と 4 番)。どちらも変数で前に戻せる。

## 6. 依頼文が決めていなかったこと

- **系譜の中の M1。** 継承の関係は `all` の結果を読む(1 節)。
- **リンクのコミットの範囲。** `unit_lineage` の `sha` のすべて(どの深さのリンクも)を、その unit のリンクのコミットとした。`file_rename` で、改名と同じコミットで unit の本文も変わった場合も、依頼文が `file_rename` を挙げているので除く。`body_move` の残り殻の、系譜を始め直したコミットは、残り殻のリンクのコミットではない(残り殻に前身は無い)ので、残り殻の co-change には残る。本体を受け取った側では除かれるので、残り殻と受け取った側の組はそのコミットを共有しない。
- **N2 の「以後」。** 遅い側の最初のコミットを含めた。そのコミットは、遅い側にとって片側の変更(作成)になる。読み判定 2 の「共存後の片側」の列も同じ数え方だった(8 番の b の 1 は作成のコミット)。側が unit に解決しない場合と、範囲に履歴の無い unit の場合は、その側は境を決めない。両側とも決めなければ、すべてのコミットを数える。
- **M1 の囲む unit の行**:5 節。
- **M5 のテストの判別。** `unit_tests` の `is_test` だけを使った(後ろ向きの `production` の範囲のような、パスによる補いはしていない)。読み判定 2 で、`#[cfg(test)] mod tests;` の `tests.rs` の関数にも印が付くことは確かめられている。
- **M6 の祖先。** どの深さの祖先と子孫の辺も除く。クレートの根(`lib.rs`、`main.rs`)はクレートのすべてのモジュールの祖先なので、根との辺はすべて除かれ、根はどの循環にも入らない。`module_path` は `#[path]` を読まず、`src/bin/` の下のバイナリはライブラリの根の下に置かれる。
- **M6 の同じ型。** 両端がともに同じ型の impl の中にある unit の辺とした(依頼文の「impls of the same type」)。型の定義そのもの(フィールドなど)への辺は含めていない。循環 2 番の `src/workspace/*` は、この定義で 13 → 2 モジュールになった。
- **生の辺の残し方。** `module_edges_raw` は作らず、`module_edges` に 2 つの印の列を足し、行は変えなかった。`module_graph` のクエリは前と同じ行を返す。
- **変数の名前**:1 節の表のとおり。`include_tests` は既存の変数の名前とぶつからない。
- **graph fixture。** co-change の期待値を `attribution := 'all'` で保ち、既定の規則は counting fixture で確かめた。

## 7. 記録と食い違った点、気づいた点

- **機構の表の数と、外れた数。** 表の数は「その機構が数や順位を説明する候補」なので、外れた数と同じものではない。M5 にクローンの 2 件が数えられていたが、依頼文の M5 は co-change だけを対象にしている。N1 の co-change 2・3 番はリンクのコミットが 1 つずつで、除いても上位に残る。
- **N2 の値は、読み判定 2 の「共存後の片側」の列と同じだった**(片側の向きを揃えると、10 組すべて)。
- **初回の更新の時間は、系譜の記録(24.5〜24.7 s)に近い 23.5 s だった。** 読み判定 2 の 42.6 s は load average 26 のときの値である。
- **`explain` の `READ_TEXT` の数**は、系譜の記録の「後」の数(`unit_cochange` 18 など)と、前の SQL で数えた値が一致した。

## 8. 限界

- **今の候補の判定は読み判定 2 のものを引いた。** 規則を入れた後に入った候補は判定していない。外れた `問題あり` についての 2 節の記述は、読み判定 2 の判定の理由と、数の変化から書いたもので、コードを読み直してはいない。
- **上位の作り方は読み判定 2 の記述から組み直した。** クローンの組のまとめ方(数は組の中の最大、側の名前の順で向きを揃える)は記述どおりにしたつもりで、`r2` の 45 件は読み判定 2 の表と一致した。
- **後ろ向きの正解は「範囲で変更された unit」のままである。** innermost の正解は、method だけが変わった impl を正解から外すので、`all` の範囲の値は正解の定義の変化を強く受ける。
- **M1 の別の扱い(5 節)は測っただけで、組んでいない。** 本文の分類は行の先頭の文字だけで行い、ブロックコメントの途中の行やマクロの中は区別していない。
- **1 つのリポジトリの、上位 10 件ずつ**(循環は 5 個)である。
