# gitoxide(gix)による `git log` 再現の測定(2026-10-24)

オーナーの問いは「他に、新し目のgit実装やrustによるライブラリってありませんでしたっけ」であり、続く依頼は「はい。では測定をお願いします。」である。測る対象は gitoxide(`gix`、pure Rust)である。

背景は次のとおりである。`origin/code-quality-survey` の 23368b0e にある DuckDB 拡張 `git_history`(`duckdb-ext/git-history/`)は、`git_hunks`・`git_file_changes`・`git_commits` の 3 つの表関数の行を、この repo 上で `git log` と完全に一致させている。前任の worker は、libgit2 だけでは git と一致しないことを見つけ、git v2.55.0 の C の 4 か所を Rust に移植した。(1) `-U0` の diff の前に行う `trim_common_tail`(`xdiff-interface.c`)、(2) `-M` の rename 検出(`diffcore-rename.c`・`diffcore-delta.c`)、(3) git のバイナリ判定(`diff` 属性、なければ先頭 8000 バイトの NUL)、(4) revision walk の順序(`revision.c`)である。git は GPL-2.0 なので、gix を使えばこれらの移植が不要になるかを確かめる。本記録は測定であり、推奨は書かない。既存の拡張には手を入れていない。

## 主要な観察

- **commit の順序は一致した。** `repo.rev_walk([HEAD]).sorting(ByCommitTime(NewestFirst))` は、`HEAD` と 9 つの `since..HEAD` のすべてで `git log --format=%H` と同じ列を返した。同じ秒の隣接 commit が 56 組あるが、それも含めて一致した。topo の `DateOrder` も一致した。
- **file changes は全行一致した。** rename の組み合わせ・status・numstat の 8,999 行(merge を含めると 13,163 行)のすべてが一致した。rename の行は 234 行ある。
- **`-U0` の hunk は 72 ファイル(65 commit)で違った。** git にだけある行が 142 行、gix にだけある行が 127 行である(全 44,039 行)。同じ diff を `-U1` と `-U3` で比べると、35,060 行と 28,191 行のすべてが一致した。git は context が 0 のときだけ `trim_common_tail` を行う。違った 72 ファイルは、いずれも両側に 1024 バイト以上の共通の末尾がある。
- **fixture では、type change(ファイル → symlink)の commit だけが違った。** git は削除と作成の 2 つの hunk を出し、gix は内容同士の 1 つの hunk を出す。バイナリ、rename、merge などの他の場合は一致した。
- **速さ。** probe は hunks に 0.85〜0.99 s、file changes に 0.81〜1.08 s かかった。拡張は Python DuckDB から 2.49〜2.89 s と 2.86〜3.45 s、git CLI は 0.65 s と 0.70 s だった。

## 1. 対象と版

| 項目 | 値 |
| --- | --- |
| 対象の repo | この repo の 23368b0e(`origin/code-quality-survey` の先端)。commit 1,588、merge 343(README の測定は ed63e944 の 1,586 commit で、そこから 2 commit 進んでいる) |
| git | 2.55.0(GPL-2.0) |
| libgit2(既存拡張) | 1.9.7(`libgit2-sys` 0.18.8、GPL-2.0 with linking exception) |
| `gix` | 0.90.0(crates.io の最新、MIT OR Apache-2.0)。features は `default-features = false` に `blob-diff`、`revision`、`sha1`、`max-performance` を加えた |
| `gix-diff` | 0.70.0(MIT OR Apache-2.0) |
| `gix-traverse` | 0.64.0(MIT OR Apache-2.0) |
| `gix-imara-diff` | 0.3.2(**Apache-2.0 のみ**) |
| `gix-revwalk` / `gix-object` | 0.38.0 / 0.66.1(MIT OR Apache-2.0) |
| Rust | 1.99.1 |
| 機械 | AMD Ryzen AI 9 HX 370(24 スレッド)、Linux 7.2.9(Void) |

probe の依存の木(normal、161 crate)に GPL のものはない。MIT OR Apache-2.0 以外のライセンスとして、MIT、Unlicense OR MIT、Zlib、Apache-2.0(`gix-imara-diff` と `unicode-bom`)、MPL-2.0(`uluru`。`max-performance` の pack cache が引き込む)がある(`cargo tree -p gix-probe -e normal --prefix none -f "{p} {l}"`)。C のコードはリンクしない。`ldd` に出るのは libc と libgcc_s だけである。

## 2. probe の作り

`duckdb-ext/gix-probe/` は `duckdb-ext` workspace のメンバーで、ライブラリとバイナリからなる。`git-history/` は変更していない。`gix-probe REPO hunks|file_changes|commits [--since REV] [--include-merges] [--sorting MODE] [--slider MODE] …` で、拡張の `dump` example と同じ列の TSV を出す。使った gix の API は次のとおりである。git のコードの移植も、`git-history/src/` からの複写もしていない。

- **commit**:`repo.rev_walk([head]).sorting(..).with_hidden(since).all()`。`gix::revision::walk::Sorting` の `BreadthFirst`、`ByCommitTime(NewestFirst)`、`ByCommitTime(OldestFirst)` を試した。`ByCommitTimeCutoff` は `since` とは意味が違うので試していない。topo は `gix::traverse::commit::topo::Builder::from_iters(&repo.objects, [head], since).sorting(DateOrder | TopoOrder)` を使った。列は `author().time()?.seconds`、`author().name`、`message()?.summary()`、`parent_ids()` から作る。
- **tree diff**:`gix::diff::tree_with_rewrites(..)` に `Options { location: Some(Path), rewrites: Some(Rewrites { copies: None, percentage: Some(0.5), limit: 1000, track_empty }) }` を渡す。`track_empty` は `false`(gix の既定)と `true` の両方を測った。基準の tree は、root では空の tree、merge では第 1 親である(merge は `--include-merges` のときだけ diff する)。tree の entry は捨てる。status の文字は `ChangeRef` の種類から決める。`Addition` → A、`Deletion` → D、`Rewrite` → R(`copy` なら C)、`Modification` → M である。ただし、entry の種類(file・symlink・gitlink)が変わったときは T とする。
- **行の diff**:`repo.diff_resource_cache_for_tree_diff()`(`pipeline::Mode::ToGit`、worktree なし)に `set_resource` で両側を載せ、`prepare_diff()` の `Operation` を見る。`InternalDiff` なら `Diff::compute(Myers, &prep.interned_input())` を計算する(gix の `diff.algorithm` の既定も myers である)。slider の後処理は 3 通り測った。`indent` は `postprocess_lines`(`diff_with_slider_heuristics` と同じ `IndentHeuristic`)、`none` は `postprocess_no_heuristic`、`raw` は後処理なしである。`@@` の番号は `Diff::hunks()` の範囲から作る。開始は 1 始まりで、空の側は直前の行を指す。これは README が述べる git の番号の付け方である。比較のため、`UnifiedDiff` を `ContextSize::symmetrical(0)` で使い、その header をそのまま使う版(`--unified-header`)も測った。numstat には、後処理する前の diff の `count_additions()`・`count_removals()` を使う。これは gix の `Platform::line_counts()` と同じ計算である。gix の `Diff` では、行数が context の数に左右されない。
- **バイナリ**:`prepare_diff()` が `SourceOrDestinationIsBinary` を返したら、hunk を出さず、numstat を NULL にする。
- **type change と submodule**:gix にはそれを直接扱う手段がなかった(5 節)。type change は gix が出すとおり、1 つの `Modification` として diff する。submodule の側は `set_resource` が `InvalidMode` で拒むので、hunk なし・NULL とする。

比較には `duckdb-ext/scripts/gix_parity.py` を使った。git 側の行は前任者の `cli_hunks.py` と `cli_file_changes.py` で作り、両者を multiset として比べる(`parity.py` の `EXCEPT ALL` と同じ)。コマンドは次のとおりである。

```sh
cd duckdb-ext && nice -n 19 cargo build --release -j 4 -p gix-probe
cd .. && python3 duckdb-ext/scripts/gix_parity.py . duckdb-ext/target/release/gix-probe duckdb-ext/target/gix-parity hunks context file_changes tail commits merges
DUMP=$PWD/duckdb-ext/target/release/examples/dump python3 duckdb-ext/scripts/gix_parity.py . duckdb-ext/target/release/gix-probe duckdb-ext/target/gix-parity timing
```

対照として、拡張の `dump` example の hunks と file changes を同じ先端で git の出力と突き合わせた。`sort` して `cmp` したところ、どちらも一致した。git 側の基準そのものは正しい。

## 3. hunks(`git log -p -U0 -M --diff-merges=off`)

git の行は 44,039 行である(`cli_hunks.py .`)。

| gix の設定 | gix の行数 | git にだけある行 | gix にだけある行 |
| --- | --- | --- | --- |
| (a) slider あり(`postprocess_lines`) | 44,024 | 142 | 127 |
| (b) slider なし(`postprocess_no_heuristic`) | 44,024 | 1,363 | 1,348 |
| (b') 後処理なし(`Diff::compute` のみ) | 45,225 | 3,961 | 5,147 |
| (a) に `track_empty = true` | 44,024 | 142 | 127 |
| (a) の header を `UnifiedDiff`(context 0)から取る | 44,024 | 16,154 | 16,139 |

最後の行の違いは番号の付け方だけである。`UnifiedDiff` は、空の側にも `start + 1` を書く。たとえば git の `@@ -0,0 +1,10 @@` は `@@ -1,0 +1,10 @@` になる。本記録の他の数字は、すべて `Diff::hunks()` から git の番号で作った行のものである。

### 違いの形(slider あり)

違う行を `(sha, path)` ごとにまとめ、形で分類した(`gix_parity.py` の `classify`)。

| 形 | ファイル数 | git の行 | gix の行 | 判定の規則 |
| --- | --- | --- | --- | --- |
| shifted | 25 | 25 | 25 | 行数が同じで、両側の開始が同じだけずれている |
| split-or-merged | 28 | 75 | 74 | 削除行と追加行の合計は同じだが、hunk の切れ目が違う |
| different-lines | 19 | 42 | 28 | 削除行か追加行の合計が違う(diff そのものが違う) |
| 計 | 72(65 commit) | 142 | 127 | |

slider なし(b)では、shifted が 454 ファイル・2,420 行に増えた。split-or-merged は 28 ファイル・151 行、different-lines は 19 ファイル・140 行で、ほとんど変わらなかった。後処理なし(b')では、shifted が 1,214 ファイル、split-or-merged が 744 ファイル、different-lines が 19 ファイルだった。

### 原因の切り分け

git の `xdi_diff` は、context が 0 で function context もないときにだけ `trim_common_tail` を行う(README と `git-history/src/history.rs` の記述による)。そこで、git が末尾を削らない `-U1` と `-U3` で同じ比較をした。git 側は `git log -p -U<n> -M` の header を body の行数で数えて作り、gix 側は `UnifiedDiff` を `ContextSize::symmetrical(n)` で使った。空の側の番号だけ git に合わせた(`gix_parity.py … context`、probe の `--context n`)。

| 比較 | git | gix | 違い |
| --- | --- | --- | --- |
| `-U1`、slider あり | 35,060 | 35,060 | 0 / 0 |
| `-U3`、slider あり | 28,191 | 28,191 | 0 / 0 |
| `-U1`、`--diff-merges=first-parent` / `--include-merges` | 51,320 | 51,320 | 0 / 0 |

さらに、`-U0` で違った 72 ファイルの両側の blob を `git cat-file` で取り出し、末尾の共通部分のバイト数を測った(`gix_parity.py … tail`)。72 ファイルすべてで 1024 バイト以上だった。`trim_common_tail` が切り落とすのは 1024 バイト単位の塊なので、72 ファイルはどれも切り落としの対象になる。context が 1 以上なら全行が一致し、違いは末尾が切られる条件を満たすファイルにだけ出る。この 2 つの観察から、`-U0` の違いは 3 つの形のすべてを含めて `trim_common_tail` の有無に由来すると判断した。逆向きの確認(gix の入力の末尾を git の規則で削ってから diff すると一致するか)はしていない。それは git のコードを probe に移植することになるからである。

形ごとの例を 3 つずつ示す(先頭は commit の短縮 SHA)。

shifted:

| commit / path | git | gix |
| --- | --- | --- |
| 04a5165f86 `crates/horizon-logd/tests/e2e.rs` | `@@ -286,0 +287,100 @@` | `@@ -287,0 +288,100 @@` |
| 196598f1fb `crates/horizon-agent/src/tools/catalog.rs` | `@@ -252,0 +253,67 @@` | `@@ -253,0 +254,67 @@` |
| 2e6a5f32d5 `config.example.toml` | `@@ -107,30 +106,0 @@` | `@@ -106,30 +105,0 @@` |

split-or-merged:

| commit / path | git | gix |
| --- | --- | --- |
| 0247b3f333 `src/sessiond/tests.rs` | `@@ -337,11 +337,11 @@` `@@ -348,0 +349,27 @@` | `@@ -337,11 +337,20 @@` `@@ -348,0 +358,18 @@` |
| 1e1fe08614 `src/agent/view.rs` | `@@ -523,0 +547,12 @@` `@@ -525,0 +561,1 @@` | `@@ -523,0 +547,5 @@` `@@ -525,0 +554,8 @@` |
| 266370be2d `crates/horizon-sessiond/tests/e2e.rs` | `@@ -606,11 +617,9 @@` `@@ -618,8 +626,0 @@` | `@@ -606,8 +617,9 @@` `@@ -615,11 +626,0 @@` |

different-lines:

| commit / path | git | gix |
| --- | --- | --- |
| 1e7fd1eaa8 `crates/horizon-agentd/src/main.rs` | `@@ -107,2 +108,15 @@` `@@ -110,1 +124,1 @@` | `@@ -107,4 +108,17 @@` |
| 4c709af3ea `crates/horizon-sessiond/src/session/setup.rs` | `@@ -136,2 +136,1 @@` `@@ -139,5 +138,1 @@` | `@@ -136,8 +136,3 @@` |
| 7596236a4e `src/control_surface/view/actions.rs` | `@@ -4,7 +10,0 @@` `@@ -12,1 +12,1 @@` | `@@ -4,9 +11,2 @@` |

shifted では、同じ挿入の位置が 1 行下にずれている。それ以外の 2 つの形では、切れ目の決まり方か、間にある不変の行の扱いが違う。gix は git が分ける 2 つの hunk を、間の行も変更扱いにして 1 つにまとめている。

## 4. file changes(`git log --raw --numstat -M --diff-merges=off`)

`cli_file_changes.py .` と `gix-probe . file_changes` を比べた。

| 設定 | git | gix | 違い |
| --- | --- | --- | --- |
| `track_empty = false` | 8,999 | 8,999 | 0 / 0 |
| `track_empty = true` | 8,999 | 8,999 | 0 / 0 |

- rename の組み合わせ:違いのある commit はない。R の行は 234 行あり、そのうち 52 行は内容の変わらない rename(0/0)である。README によれば、libgit2 の `find_similar` では 6 commit で組み合わせが git と違った。gix は、その 6 commit を含む全 commit で git と同じ組を作った(6 commit がどれかは記録されていないので、個別には確かめていない)。
- status の文字:違いはない。この repo の行は A 1,157、D 324、M 7,284、R 234 で、T はない。
- バイナリ(NULL の数):git も gix も 0 行である。この repo の履歴には、git がバイナリと判定するファイルがない。
- numstat:違いはない。

`gix-diff` の rename 検出は、ソースの冒頭で「git とは多くの点で違う」と自ら述べている(`rewrites/tracker.rs` の Deviation の節)。読んだ範囲での違いは次のとおりである。basename の一致だけで組む段がない(同名は、完全一致の候補の中での優先と、類似度が同点のときの決め手にしか使わない)。類似度は git の span hash ではなく、行 diff で消えたバイト数から計算する(`(old_len - removed_bytes) / max(old_len, new_len)`)。組み合わせは行列全体の最大を取るのではなく、追加側の順に最良の削除側を決めていく。バイナリの組は類似度を計算しない。この repo での一致は観測であって、実装の同等性を示すものではない。

## 5. commit の順序(`git log [since..]HEAD --format=%H`)

`since` には、`git log --format=%H` の列の位置 `round(k × 1588 / 10)`(k = 1〜9)にある 9 つの commit を使った。前任者の 9 つの値は README にもテストにも残っていなかったので、ブリーフの指示どおり新しく選んだ。`gix_parity.py … commits` の結果は次のとおりである。数字は git と違う位置の数である。

| 範囲(件数) | breadth-first | commit-time | commit-time(commit-graph なし) | commit-time-oldest | topo-date | topo |
| --- | --- | --- | --- | --- | --- | --- |
| HEAD(1,588) | 1,014 | 一致 | 一致 | 1,580 | 一致 | 504 |
| 73e2244648..(163) | 95 | 一致 | 一致 | 155 | 一致 | 80 |
| daef3b533a..(318) | 248 | 一致 | 一致 | 310 | 一致 | 188 |
| dcb709a4dd..(476) | 405 | 一致 | 一致 | 468 | 一致 | 253 |
| 5760532b8e..(636) | 532 | 一致 | 一致 | 625 | 一致 | 289 |
| 953cc8f522..(797) | 622 | 一致 | 一致 | 789 | 一致 | 326 |
| f1cb4864d5..(953) | 761 | 一致 | 一致 | 945 | 一致 | 416 |
| 51adc8c5a0..(1,113) | 884 | 一致 | 一致 | 1,105 | 一致 | 448 |
| 99b9c4b92e..(1,270) | 964 | 一致 | 一致 | 1,261 | 一致 | 486 |
| 5c2ca1500b..(1,429) | 1,012 | 一致 | 一致 | 1,421 | 一致 | 504 |

どの設定でも、commit の集合は git と同じだった。commit-time と topo-date は、git の順序とも一致した。`git log --format=%ct HEAD | uniq -c` で数えると、同じ committer 時刻の commit が隣り合う組が 56 組あり、その並びも一致した。この repo には分割された commit-graph があり、gix の単純な walk はそれを既定で使う。`use_commit_graph(false)` でも結果は同じだった。commit の列(`author_time`、`author_name`、`subject`、`parent_count`、`is_merge`)も、`git log --format=%H%x09%at%x09%an%x09%s%x09%P` と multiset として一致した。`summary()` は、複数行の件名を空白でつなぐ `%s` の挙動も再現した(fixture の "First line wraps here")。

## 6. `include_merges`(`--diff-merges=first-parent`)

| 表 | git | gix | git にだけある行 | gix にだけある行 |
| --- | --- | --- | --- | --- |
| hunks(slider あり) | 64,354 | 64,328 | 226 | 200 |
| file changes(`track_empty` の両方) | 13,163 | 13,163 | 0 | 0 |

hunks の違いは 117 ファイルにある。形で分けると shifted 43、split-or-merged 42、different-lines 32 である。117 ファイルのすべてに 1024 バイト以上の共通の末尾があり、`-U1` では 51,320 行が全部一致した(3 節の表)。

## 7. 時間

ここでの数字はすべて、新しいプロセスで 3 回ずつ測ったものである(`gix_parity.py … timing` は子プロセスの壁時計と `ru_maxrss` を測る)。拡張は `uv run --with duckdb==2.0.0 python duckdb-ext/scripts/perf.py <ext> <repo> <fn> 3` で、1 プロセスの中で 3 回測った。機械は他の作業と共用している。

| 処理 | 行 | 時間 | ピーク RSS |
| --- | --- | --- | --- |
| gix-probe hunks | 44,024 | 0.99 / 0.85 / 0.89 s | 142 MiB |
| gix-probe file_changes | 8,999 | 1.08 / 0.81 / 0.84 s | 140 MiB |
| gix-probe commits | 1,588 | 0.01 s ×3 | 13 MiB |
| 拡張 `git_hunks`(Python DuckDB) | 44,039 | 2.89 / 2.49 / 2.57 s | 173 MiB(LOAD 後 68 MiB) |
| 拡張 `git_file_changes` | 8,999 | 2.91 / 2.86 / 3.45 s | 144 MiB(LOAD 後 69 MiB) |
| 拡張 `git_commits` | 1,588 | 0.02 / 0.03 / 0.03 s | 84 MiB(LOAD 後 69 MiB) |
| 拡張の crate の `dump` example hunks | 44,039 | 2.64 / 2.60 / 2.60 s | 74 MiB |
| 同 file_changes | 8,999 | 2.99 / 2.81 / 2.90 s | 72 MiB |
| 同 commits | 1,588 | 0.02 s ×3 | 13 MiB |
| `git log -p -U0 -M …`(出力は捨てる) | | 0.66 / 0.66 / 0.65 s | 119 MiB |
| `git log --raw --numstat -M …` | | 0.70 s ×3 | 119 MiB |
| `git log --format=%H` | | 0.01 s ×3 | 13 MiB |
| `cli_hunks.py`(git と Python の解析) | 44,039 | 0.72 / 0.74 / 0.75 s | 120 MiB |
| `cli_file_changes.py` | 8,999 | 0.82 / 0.79 / 0.77 s | 119 MiB |

probe は 1 スレッドで commit を順に処理する。object cache を 64 MiB に設定しており(`object_cache_size_if_unset`)、RSS にはその分が含まれる。probe の時間には、TSV を組み立てる時間も含まれる。

## 8. fixture

`git-history/tests/cli_parity.rs` の fixture の作り方と CLI の解析を、`duckdb-ext/gix-probe/tests/fixture_parity.rs` に複写した(`git-history` 側は変えていない)。そのうえで、同じ 4 つの範囲(`since` の有無 × `include_merges` の有無)で比べた。テストは観測した状態を assert しており、`cargo test -p gix-probe` は通る。

| 場合 | hunks | file changes | commits |
| --- | --- | --- | --- |
| root commit(空の tree との diff) | 一致 | 一致 | 一致 |
| 2 つの hunk、挿入だけの hunk | 一致 | 一致 | 一致 |
| 削除だけの hunk | 一致 | 一致 | 一致 |
| 内容の変わらない rename(R、0/0) | 一致 | 一致 | 一致 |
| 内容の変わる rename | 一致 | 一致 | 一致 |
| バイナリの変更(hunk なし、NULL) | 一致 | 一致 | 一致 |
| ファイル → symlink(T) | **違う** | 一致(status T) | 一致 |
| merge(off / first-parent) | 一致 | 一致 | 一致 |
| `since`、`%s` の折り返し | — | — | 一致 |

type change の commit の `kind.txt` について、git は `@@ -1,1 +0,0 @@` と `@@ -0,0 +1,1 @@` の 2 行を出し、gix は `@@ -1,1 +1,1 @@` の 1 行を出す。この repo の履歴には T がないので、0 行の違いである。

## 9. 4 つの移植部分の判定

| 移植した振る舞い | 判定 |
| --- | --- |
| (1) `-U0` 前の `trim_common_tail` | **gix differs: 269 行(git にだけ 142、gix にだけ 127)/ 72 ファイル / 65 commit。merge を含めると 426 行 / 117 ファイル。** 原因は、gix が context 0 の diff の前に末尾を削らないことである。context 1 と 3 では全行一致した。gix に切り替える設定はなかった。調べた範囲は `Diff::compute`、`postprocess_lines` / `postprocess_no_heuristic` / `postprocess_with_heuristic`、`diff_with_slider_heuristics`、`UnifiedDiff` と `ContextSize`、`blob::Platform::prepare_diff` と `platform::Options` である。imara の Myers が両端の共通部分を除くのは行単位の前処理で、git の 1024 バイト単位の切り落としとは別物である。git の結果に合わせるには、gix に渡す前に入力を git の規則で削る必要があり、それは git のコードの移植になる |
| (2) `-M` rename 検出 | **gix reproduces it as configured**(`Rewrites { copies: None, percentage: Some(0.5), limit: 1000, track_empty: false }`。`true` でも同じ)。この repo で 8,999 行(merge を含めると 13,163 行)、R 234 行、違いは 0 である。fixture も一致した。ただし、アルゴリズムは git と違う(4 節)。一致は観測であり、別の履歴で違いが出る可能性は残る |
| (3) バイナリ判定 | **gix reproduces it as configured**(fixture の 1 件で確認。この repo にはバイナリがない)。gix の規則は `gix-diff` の `blob/pipeline.rs` にある。diff driver の `binary` 設定があればそれに従う。なければ、`diff` 属性が unset(`-diff`、`binary` マクロ)のときにバイナリとする。どちらでもなければ、先頭 8000 バイトに NUL があるかで決める(`is_binary_buf`)。加えて、`large_file_threshold_bytes`(`core.bigFileThreshold`)を超える blob もバイナリとする。ソースを読んで分かった違いが 1 つある。`diff` 属性が set(`diff`)のとき、git はテキストと決め打つが、gix は NUL の検査に回す。これは測っていない |
| (4) revision walk の順序 | **gix reproduces it with option `Sorting::ByCommitTime(CommitTimeOrder::NewestFirst)`**(`since` は `with_hidden`)。`HEAD` と 9 つの `since` のすべてで、同じ秒の 56 組を含めて一致した。topo の `DateOrder` も一致した。`BreadthFirst`、`OldestFirst`、`TopoOrder` は一致しない |

README の慣例に関わる 2 点の判定は次のとおりである。

- **type change を削除と作成に分ける**:gix に直接の手段はない。`tree_with_rewrites` は `Modification` を 1 つ出し、`blob::Platform` は file と symlink の内容を 1 つの diff にする。fixture の 1 commit で違い、この repo では 0 行である。
- **submodule の側の `Subproject commit <sha>` 行**:gix に手段はない。`blob::Platform::set_resource` は、`EntryKind::Commit` を `set_resource::Error::InvalidMode` で拒む。probe ではその変更を hunk なし・numstat NULL とした。この repo にも fixture にも submodule はないので、違いは測っていない。

## 10. 確かめられなかったこと

- `track_empty`(空ファイルの rename)に git の既定と同じ値があるかどうか。この repo にも fixture にも、空の blob の rename は現れなかった。
- `diff` 属性が set のとき、バイナリ判定がどう違うか(9 節の (3))。
- submodule を含む履歴での挙動。
- 前任者が libgit2 で見つけた、rename の組が違う 6 commit と順序が違う 8 commit が、それぞれどれだったか。記録がないので、それらを個別には照合していない。上の比較は全 commit を対象にしている。
- `-U0` の違いが `trim_common_tail` に由来することは、`-U1`/`-U3` で一致すること、および末尾の長さの条件から判断した。gix の入力に git の切り落としを施して一致させる確認はしていない。
- 他の repo での挙動。測ったのはこの repo と fixture だけである。

## 11. ブリーフや README との食い違い

- README の数字は ed63e944(1,586 commit、hunk 44,009 行、file changes 8,969 行)のものである。本記録の先端は 23368b0e で、1,588 commit、44,039 行、8,999 行である。拡張の `dump` の出力は、この先端でも git と一致した。
- README には「libgit2 で末尾を削らないと 44,009 行中 142 行が違った」とある。gix で git にだけある行も 142 行だが、先端も比べている行数も違う。同じ行かどうかは照合していない。
- ブリーフは、前任者の 9 つの `since` 値が README かテストにあるとしていたが、見つからなかった。そのため 5 節で新しく選んだ。

## 12. `unit_changes` への影響(依頼側セッションの追加測定、2026-10-24)

オーナーの問い「どう実害が出るズレなのかがわからないのと、gixの挙動としてバグという意味?」に答えるため、hunk が使われる唯一の場所であるスパイクの `unit_changes`(hunk の行範囲と unit の行範囲の交差)を、git の hunk と gix の hunk のそれぞれから作って比べた。スクリプトは `duckdb-ext/scripts/unit_changes_impact.py`(スパイクの `history.py` の `commits_and_changes`・`UnitCache`・`unit_changes` をそのまま使う)。入力は本記録の §3 と同じ `gix_parity.py` が `target/code-db/gix-out/` に書いた `git_hunks.tsv` と `gix_hunks_indent.tsv` で、`*.rs` に絞った。対象は 0df26a53 の作業ツリー(1,589 コミット)。

```sh
uv run --with tree-sitter==0.25.2 --with tree-sitter-rust==0.24.2 --with tree-sitter-go==0.25.0 --with tree-sitter-typescript==0.23.2 \
  python -I duckdb-ext/scripts/unit_changes_impact.py . target/code-db/gix-out/git_hunks.tsv target/code-db/gix-out/gix_hunks_indent.tsv
```

| | git の hunk から | gix の hunk から |
| --- | --- | --- |
| hunk 行(`*.rs`) | 34,703 | 34,694 |
| `unit_changes` 行 | 40,513 | 40,506 |
| 片側にしかない行 | 7(6 コミット、7 unit) | 0 |

7 行はすべて `modified` で、既存の unit の直後に新しい unit を追加したコミットに集中する。例: 901edb40f6 の `crates/horizon-agent/src/contract.rs` では、`impl ToolCallRequest { fn identity … }` の直後に `impl ApprovalRequest { fn identity … }` が追加された。git の hunk は `@@ -928,0 +901,9 @@` で、挿入 9 行の先頭 3 行が `        }`、`    }`、`}`、つまり既存の `identity` と impl の閉じ括弧を「新規」と読み、新しい塊の閉じ括弧を「既存」と読んでいる。この範囲は `ToolCallRequest.identity` の末尾と交差するため、スパイクは変更のない `impl ToolCallRequest` と `ToolCallRequest.identity` を「変更」と記録する。gix の hunk は `@@ -932,0 +905,9 @@` で `impl ApprovalRequest {` から始まり、新しい unit だけを記録する。残る 6 件(43efd46a66、9bda1b0109、b3406dfd63、c325cd098d、fed32039eb の各テスト関数、および同じ 901edb40f6 の impl 側)も同じ構図である。

このリポジトリでは、`-U0` の配置差による誤った帰属は git 側に 7 件、gix 側に 0 件だった。どちらの道具も hunk を unit の境界に揃えることは保証していないので、別のリポジトリで逆向きに出ることは排除できない。「git と行単位で一致させる」という目標は、依頼側が検証の手段として置いたもので、監査の要件ではなかった。

この測定を見て、オーナーは 2026-10-24 に「ではgixで進めましょう」と決めた。
