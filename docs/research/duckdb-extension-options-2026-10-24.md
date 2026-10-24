# DuckDB 拡張の実装余地 2026(2026-10-24 調査)

オーナーの依頼は「duckdbの拡張として作る余地のあるコンポーネントもありそうなので、そちら側の調査を並行して行っておきましょうか」である。この依頼は、Python で書く言語非依存コードデータベースのスパイクを並行して実装している最中に出された。スパイクの設計は、tree-sitter の構文木 `nodes`、関数単位 `units`、`tags`、git の `commits`/`file_changes`/`hunks`、コミットごとの `unit_changes`、jscpd の `clone_pairs`、任意で SCIP の `occurrences`/`symbols` を DuckDB の表にし、SQL で構造と履歴を結合するものである。このレポートはスパイクの結果を見ずに書いた。問いは三つある。(1) 各部品について、既存の DuckDB 拡張で代替または流用できるものはどれか。(2) 自作するなら DuckDB 拡張として作る余地のある部品はどれか。また 2026 年 10 月時点の拡張の作り方(C++ テンプレート / C API v1 / C API v2 / Rust)と、DuckDB 2.0 への互換性・配布・保守の負担はどうか。(3) CodeQL・Glean・Kythe・Joern・SCIP・Sourcegraph などの先行例はこれらの事実をどうモデル化しており、スキーマ設計について何が分かっているか。

方法は、Opus の調査エージェント 3 体による並行の Web 調査である。担当は「拡張の作り方と配布」「既存拡張の一覧」「先行例のデータモデル」の三つに分けた。検証の表記は次のとおりである。

- ★:調査者が一次資料のページを自分で読んだ。
- ★raw:調査者が生のファイルやスキーマをダウンロードして中身を確かめた。
- 無印:検索スニペットか二次的な要約に由来する。

先行例担当の調査者は、要約つきの取得ツールで読んだページにも ★ を付けている。その場合の文言は言い換えである。リポジトリの状態(スター数、最終 push、archived かどうか)はすべて 2026-10-24 に GitHub API で確かめた。

DuckDB v2.0.0 のタグは 2026-10-21 に付いた。community extensions のレジストリは 2026-10-15 に既定のビルドを 2.0 系へ切り替えた(PR #3079)。したがって、ある拡張に「2.0 のバイナリがない」場合、それは「まだ再ビルドされていない」だけかもしれない。生の調査ノートはジョブの一時ディレクトリ(`/home/satoshi/.claude/jobs/da8abac5/tmp/research_notes/DuckDB 拡張の実装余地 2026/`)にあり、揮発する。恒久的な記録はこのレポートである。

この節以降の「推論」は、資料の主張ではなく、調査者か執筆者の推論を示す。

## 主要な知見

- **tree-sitter 層には成熟した community 拡張 sitting_duck(v1.16.0、C++、Apache-2.0)がある。** その `read_ast` は、1 ノード 1 行で DFS preorder の `node_id`、`parent_id`、`depth`、`descendant_count`、行と byte の範囲を返す。28 言語(自己申告)を扱い、v1.5.6 と v2.0.0 の linux_amd64 バイナリが実在する(★ 2026-10-24 のプローブ)。ただし、CSS セレクタ系の `ast_select_from` は 2.0 で約 24 倍遅い。原因は上流のプランナ回帰 duckdb/duckdb#26036 で、メンテナが自分で測った(★ descriptor)。素の `read_ast` が 2.0 で遅くなるかを測った人はいない。
- **git 層の duck_tails(v1.8.0、C++、libgit2)には、2026-10-24 時点で 2.0.0 のバイナリがない。** v1.4.5 と v1.5.6 のバイナリはある(★ プローブ)。descriptor は 2.0 でビルドとテストが通ったと述べている(1270 assertions)。CI が失敗したのか、まだ走っていないだけなのかは確かめていない。duck_tails の `git_diff_tree` が返すのはファイルの状態だけで、行数も hunk もない(★ ソース)。**コミットごとの `hunks` を返す DuckDB 拡張は見つからなかった。**
- **SCIP を読む DuckDB 拡張も、jscpd を読む拡張も見つからなかった。** どちらもコアの `read_json` で読める(推論)。SCIP の `expt-convert` は SQLite を出力するが、occurrence は 200 件ごとのチャンクに分けた zstd 圧縮の protobuf BLOB として格納される(★ ソース)。protobuf を解く protoduck には v1.5.5 のバイナリしかない(★ プローブ)。
- **拡張の ABI は 3 クラスになった。** C++(`CPP`)と C API v2(`C_STRUCT_UNSTABLE`)のバイナリは、ビルドした DuckDB の版でしか読み込めない。C API v1(`C_STRUCT`)のバイナリは、宣言した下限の版以降で読み込める。公式の Rust テンプレートは 2026-10-23 に C API v2(`duckdb-neo`)へ移った(★ Makefile)。その結果、テンプレートどおりに作った Rust 拡張は C++ と同じく版に固定される。版をまたいで同じバイナリを使える Rust の経路は、duckdb-rs の v1 `vtab` API だけになる(推論)。
- **保守の負担は具体的な形で観測できる。** 2.0 のリリースから 48 時間以内に、community extensions に「Bump X for DuckDB v2.0」の PR が集中した(★)。duck_tails は、2.0 で削除された `TableFunction::named_parameters` を互換ヘッダで吸収している。その descriptor には「Declare-vs-Extend ordering throws at LOAD time, not build time」と書かれている(★)。現在は 1.4 / 1.5 / 2.0 の 3 系列が同時にサポートされている(★ 2.0 告知)。
- **Python には表関数 UDF がない。** 2.0 の Python で定義できるのはスカラー UDF だけで、Arrow 版は最大 2048 行のバッチで呼ばれる(★ docs)。表関数 UDF の PR(duckdb-python #99)は 2025-10-28 に未マージのまま閉じられた(★)。
- **調べた先行例の中に、コミットをまたぐ同一性(同じ関数・同じクローンが別のコミットでも同じものだという判定)を保存された識別子として解いているものはない。** CodeQL と Kythe は 1 スナップショットの中だけで識別子を一意にしている(★raw / ★)。Software Heritage と World of Code は内容ハッシュで識別している(無印)。関数の履歴は追跡器による照合問題として扱われている。報告されている精度は約 90%(CodeShovel)から 99.5%(CodeTracker)だが、対象はほぼ Java で、HistoryFinder(ICSE 2026)の訂正版オラクルでは CodeShovel の recall が大きく下がる(いずれも無印)。
- **SCIP の occurrence を、Sourcegraph も scip `expt-convert` も 1 行 1 occurrence に正規化していない。** どちらも BLOB として持っている(★raw / ★)。関係演算で結合するための形ではない。

## 部品ごとの対応表

各セルの出典は、後続の節の該当箇所である。「推論」と書いたセルは資料の主張ではない。

| 部品 | 既存拡張で得られるもの(名前・関数・版の対応) | 既存拡張で得られないもの | 自作拡張にした場合に効く点 | Python 側のままで足りる点 |
|---|---|---|---|---|
| `nodes` | sitting_duck `read_ast(..., source := 'full')`。返す列は `node_id`(ファイルごとに 0 始まりの preorder)、`parent_id`、`depth`、`sibling_index`、`children_count`、`descendant_count`、`type`、`start/end_line`、`start/end_column`(1 始まり)、`start/end_byte`(0 始まり・半開)。並列のグロブ読み込みに対応する。バイナリは 1.5.6 と 2.0.0 にあり、1.4.5 にはない(1.4.4 にはある)(★) | 公開されたスループット値(★ docs に無し)。2.0 で素の `read_ast` が遅くなるかの測定。Haskell/Julia/Scala/F# のドキュメントはあるが grammar のサブモジュールがなく、実際にビルドされているか不明(★) | 2048 行チャンクでストリームできるので、全 AST を一度にメモリに載せずに済む。sitting_duck 自身がこの方式を採っている(★ architecture docs) | `read_ast` で取り込むか、py-tree-sitter から Arrow で入れる。どちらでも全件を materialize するなら、ABI の負担なしに大半の利点が得られる(推論) |
| `units` | sitting_duck の `is_function_definition(semantic_type)`、`qualified_name`、`ast_function_metrics`(cyclomatic・max_depth・lines)、`ast_function_scope`(★) | 名前束縛と意味分類の精度。#276 で `ast_definitions()` が同じ名前を 2〜3 回返す、#147 で `:is-referenced` がすべての定義に一致する、など未解決の issue が多い(★)。関数の境界の定義はスパイク側の言語ごとの規則と一致する保証がない(推論) | 言語ごとの関数判定を C++ や Rust のコードとして書ける。ただしその分の保守は自分で負う(推論) | `nodes` の上の SQL ビュー、または Python 側の言語ごとの規則で作れる(推論) |
| `tags` | sitting_duck の `ast_definitions` と `ast_find_references`(スコープチェーンを考慮)(★)。Query.Farm vgi-code の `ts_query(source, language, query)` は `.scm` の S 式クエリを 9 言語で実行できるが、DuckDB 拡張ではなく、Arrow IPC で通信する別プロセスの worker である(★) | 同じプロセス内で tree-sitter の `tags.scm` 規約をそのまま実行する拡張(★ sitting_duck の functions.md を grep して確認) | `.scm` クエリを表関数としてプロセス内で実行できるようになる(推論) | py-tree-sitter の `Query` で `tags.scm` を実行し、Arrow で入れる(推論) |
| `commits` / `file_changes` | duck_tails `git_log`。列は hash・author・committer・日時・message・parent_count・tree_hash。ファイル単位の状態は `git_diff_tree` が返す(status、old_path)。バイナリは 1.4.5 と 1.5.6 にあり、**2.0.0 にはない**(★)。proudust/duckdb-git(Rust、未署名、v0.6.0、≥1.5.5)の `git_log` は、`file_changes` STRUCT[] に path・add_lines・del_lines を入れて返す(★ README)。shellfs(2.0 のバイナリあり)を使えば `git log --numstat` をパイプで読める(★ README。構文は未読) | 2.0 で署名済みのまま、行数つきの `file_changes` を返すもの(推論) | rename 検出やマージの扱いを、表関数の名前付き引数として持たせられる(推論) | `git log --numstat` を CSV にして読む。公式ガイドはこの方式を採り、4 バイトの区切り文字を使っている(★) |
| `hunks` | ない。近いものは duck_tails の `read_git_diff`(`+`/`-` を前置した行の diff テキスト)と `text_diff_lines` だが、読んだ文書の範囲では新旧の行番号を返さない(★) | コミット × ファイルごとの `@@` 範囲(★ 一覧担当の結論) | 存在しない部品なので、拡張として作る余地が最も明確にある。libgit2 か gix から直接ストリームできる(推論) | `git log -p --unified=0` の `@@` ヘッダを Python で解析する。または shellfs から読んで SQL で解析する(推論) |
| `unit_changes` | 部品として持つ拡張はない。duck_tails の `git://path@rev` と `read_ast` を組み合わせると、チェックアウトせずにリビジョンごとの AST が得られる。sitting_duck は issue #282 で、この URI を受け付けることを確認している(★)。ただし 2.0 では duck_tails のバイナリがない | hunk と unit の対応付け。コミットをまたぐ unit の同一性(先行例の節) | 必要な (commit, file) の組だけを遅延して parse できる。projection や LIMIT の pushdown がそのまま効く(推論) | `hunks` と、版ごとの `units` の範囲との区間結合として SQL で書ける(推論) |
| `clone_pairs` | jscpd 専用の拡張はない(★ GitHub 検索 0 件)。コアの `read_json` で JSON レポートを読める(推論) | クローンの系譜(バージョンをまたぐ対応付け) | 小さい。jscpd の外部実行は拡張にしても残る(推論) | `read_json` か Python で取り込める(推論) |
| SCIP `occurrences` / `symbols` | SCIP 専用の拡張はない(★ 検索 0 件)。`scip print --json` の出力をコアの `read_json` で読める(推論)。`scip expt-convert` の SQLite はコアの `sqlite_scanner` で読める。sqlite_scanner のバイナリは 1.4.5 / 1.5.6 / 2.0.0 のすべてにある(★)。documents・global_symbols・mentions・defn_enclosing_ranges は SQL で直接扱える(★ ソース) | occurrence の BLOB の展開。zstd を展開して protobuf を復号する手段が、2.0 にはない。protoduck のバイナリは v1.5.5 だけで、コアに zstd 展開のスカラーがあるかは未確認 | SCIP protobuf の直接の reader。occurrence を 1 行ずつストリームできる(推論) | Python の protobuf バインディングで復号し、Arrow で入れる(推論) |
| 結果に対するグラフ問い合わせ | コアの再帰 CTE(`USING KEY`、v1.3 以降)。2.0 で実行器が改修され、到達可能性のベンチマークで 42.6 倍速くなった(★ 公式ブログ)。DuckPGQ v1.0.0 には 2.0.0 のバイナリがある(2026-10-22)。ただし接続ごとに `SET active_grammar_extensions = ['duckpgq']` が要り、`GRAPH_TABLE` を使うビューはファイル型の DB では作れない(★)。onager(40 以上のアルゴリズム)と petgraph_ext にも 2.0.0 のバイナリがある(★)。duckgql のバイナリは v1.5.5 だけ(★) | DuckPGQ の 2.0 での性能値(★ duckpgq.org に数値なし) | 小さい(推論) | fan-in × churn のような問いは、通常の結合と集約で書ける(推論) |

## 拡張の作り方と配布

### 経路と ABI クラス

| 経路 | リポジトリ(最終 push) | ABI クラス | 版の扱い | 状態 |
|---|---|---|---|---|
| C++ API | `duckdb/extension-template`(2026-10-22)★ | `CPP` | DuckDB の正確な版と platform に固定される(★ 2.0 docs) | 既存のコード解析拡張(sitting_duck、duck_tails)はこの経路で作られている(★) |
| C API(安定部分 = v1) | `duckdb/extension-template-c`(2026-10-22)★ | `C_STRUCT` | 宣言した下限の版以降で読み込める(★ extension-ci-tools) | README は「(Experimental)」と書き、community extensions への対応を「(Coming soon)」としている。DuckDB のビルドは不要(★) |
| C API(不安定部分 = v2) | 同じテンプレートで `USE_UNSTABLE_C_API=1` と `CAPI_VERSION=2` を指定する ★ | `C_STRUCT_UNSTABLE` | 「This pins the produced binaries to the exact DuckDB version」(★ base.Makefile) | 2.0 で新設された。2.0 告知は、安定と宣言する前に「test it in the field」すると述べている(★) |
| Rust(公式テンプレート) | `duckdb/extension-template-rs`(2026-10-23、PR #55「Neo template」)★ | `C_STRUCT_UNSTABLE` | Makefile のコメントは「Binaries load only in the exact TARGET_DUCKDB_VERSION, not in newer DuckDB releases.」(★) | 依存は `duckdb-neo = 0.20000.0` と `duckdb-loadable-macros = 1.20000.0`(feature `neo`)。crate 構成は core・native(cdylib)・wasm(staticlib)。ビルドに Python3/venv・Make・Git が要る(★) |
| Rust(duckdb-rs v1 `vtab`) | `duckdb` crate の `vtab` モジュール ★ | `C_STRUCT`(footer で指定) | 1.x を下限にすれば版をまたげる(推論) | テンプレートの既定の経路ではなくなった |
| Go など | `duckdb/extension-template-go` は 404 ★ | — | — | 公式テンプレートなし |

duckdb-rs v1.20000.0 は 2026-10-22 に出た。この版で C-API-V2 ブランチがマージされ、`duckdb-neo` crate が加わった(#855、#910、#913、★ release notes)。`duckdb-neo` の extensions.md には次のことが書かれている(★)。

- 実験的な機能である。
- 拡張は `DUCKDB_RELEASE_COMMIT` に固定される。
- panic は内部エラーとして報告される。そのため、失敗した `LOAD` の後は、その DB に対するすべてのクエリがエラーになる。
- 自前のプログラムに静的にリンクする方法は「TBD」である。

duckdb-rs の issue #849(2026-09-07)の題名は「Add notice of unstable V2 C API」である(★)。

第三者の移行調査(RGenomicsETL/duckhts #254、2026-09-23 起票、9-27 クローズ、★)の内容は次のとおりである。

- 表関数の v1 呼び出し(`duckdb_register_table_function` など)は「since v0.3.3」で安定している。スカラーと集約は v1.1.0 から安定している。ファイルシステムは v1.5.6 で安定化され、1.4 LTS では使えない。
- 「nothing is currently unstable in the v1 spec」とする記述がある(検索要約経由、無印)。
- 2.0 のローダーは `C_STRUCT` の v1 拡張を受け付ける。ただしこれは「ABI acceptance, not a tested successful LOAD or semantic compatibility」であり、「V1 support duration after 2.0 is undetermined」である。
- v2 にはないもの:table appender、function set、カタログの description(duckdb #25962)、カスタムファイルシステム(PR #25916 は未マージ)。
- v2 の signature は「change without deprecation」で変わる。
- 規模:DuckHTS は v1 の関数を 134 個、呼び出し箇所 7,473 で使っている。v2 には関数が 557 ある。

C API が最初に安定化されたのは DuckDB PR #14992「Bump Extension C API to stable」だが、その版と日付は確かめていない。

### 2.0 への互換性とサポート期間

2.0.0 の告知(2026-10-21、★)の内容は次のとおりである。

- サポート期間:2.0 は LTS で、少なくとも 2027 年 10 月まで。1.4 は 2026-11-17 まで、1.5 は 2027 年 3 月まで延長された。
- C++17 のコンパイラが必須になった。
- 拡張が文法そのものに hook できるようになった。
- 利用者がホストする trusted repository は「still a work in progress and its syntax may change」である。
- 2.0 で書いた DB ファイルは、それより古い版では読めない(一覧担当 ★)。
- osx_amd64 向けの community extensions はビルドされなくなり、DuckDB 2.1 で osx_amd64 のサポート自体が終わる(★ 2.0 distribution docs)。

C++ 側のソース API も壊れた。duck_diff の PR #3351 は「The previous ref (68ace70) targets v1.5.6 and does not compile against v2.0」と述べ、`extension-ci-tools` の `v2.0-cyanoptera` に合わせて修正している(★)。descriptor には系列ごとの ref が書かれる。`ref` が 2.0、`variegata` が 1.5、`andium` が 1.4 である(★)。

dc の PR #3338 では、メンテナが 3 系列すべてを 1 つのコミットに固定した。そのうえで「The automatic Andium workflow is disabled」として、1.4.5 と 1.5.6 への手動デプロイを依頼している。検証は 123 テスト × 3 ランタイム、「twelve version/platform combinations」である(★)。

duckbridge の PR #3163 では、`C_STRUCT_UNSTABLE` で詰めたバイナリが 2.0 で読み込めなかった(「built specifically for DuckDB version 'v1.5.6'」)。`USE_UNSTABLE_C_API=0` と `TARGET_DUCKDB_VERSION=v1.5.6` で `C_STRUCT` の footer にして作り直すと、「one binary load on 1.5.6 and 2.x」になり、33 項目の検査が両方で通った(★)。footer の ABI クラスを取り違えるだけで、版をまたぐ互換性が失われる実例である。

community extensions のリポジトリには、拡張のディレクトリが 387 ある。直近 30 件の PR(2026-10-23/24)はほとんどが「Bump X for DuckDB v2.0」である(★)。開発ページには「community extensions aim to be built and distributed only for the latest stable DuckDB release」と書かれている。古い版の拡張は「frozen in time」になる(★。ただしページの内容は 2.0 より前のものかもしれない)。

### 非公開拡張の読み込み

署名のない拡張は、CLI なら `-unsigned`、クライアント API なら `allow_unsigned_extensions` を指定して、`LOAD '/abs/path/x.duckdb_extension'` で読み込む(★ 2.0 docs)。`allow_extensions_metadata_mismatch` はデバッグ用で、`LOAD` では unsigned での読み込みと併用したときにしか効かない。

自前のリポジトリは `CREATE EXTENSION REPOSITORY` と公開鍵で登録する。公開鍵は作成時に固定される。置き場所は HTTP(S)・S3・`vX.Y.Z/<platform>/` 構成のローカルディレクトリのいずれかである。版や platform が合わないものを読み込むと、DuckDB がクラッシュするか未定義動作になり得る(★)。footer の版と platform はホストの CLI と一致させる必要がある(★ duckdb-neo extensions.md)。

ホストごとに埋め込まれている libduckdb は別物である。PyPI の `duckdb` の各版、CLI、libduckdb をリンクした Rust バイナリがそれぞれ別の libduckdb を持つので、C++ と v2 の拡張はホストの版ごとに別の成果物が要る(推論)。Horizon の `crates/horizon-agent` は DuckDB 2.0 の共有ライブラリを動的にリンクしているので、ここでも同じ一致が要る(推論)。

duckdb-rs 側では、bundled ビルドへの拡張の読み込みが 2.0 の前後でまだ修正されていた(#918 は 2026-10-23 クローズ、#883 は 2026-09-30、★)。

### 表関数の能力

duckdb-rs の `vtab`(v1)の `VTab` trait は bind・init・func(`DataChunkHandle` を埋める)で構成される。他に次の機能がある(★ function.rs)。

- projection pushdown:`InitInfo::get_column_indices()`
- 並列度と local init:`set_max_threads`、`set_local_init`
- 統計:`set_cardinality`
- 引数:名前付き引数、`get_config_option`(PR #912、2026-10-20)

filter pushdown の accessor は見つからなかった。

v2 C API の表関数は、2026-09-28 に閉じられた duckdb #25632 まで、1 スレッドでしかスキャンされなかった。報告では 8.4M 行で、max_threads 16 でも 1 タスク(24.0 ms)だった。`preserve_insertion_order=false` にすると 16 タスク(6.4 ms)になった。batch index がない source の順序保持プランが `SOURCE_ORDERED` になるのが原因で、batch index の対応と非順序出力のフラグが追加された(PR #26230、★)。

拡張の表関数と、Python からの INSERT や Arrow での取り込みを比べたベンチマークは見つからなかった。

## 既存拡張の一覧

バイナリの有無は、2026-10-24 に `https://community-extensions.duckdb.org/<ver>/linux_amd64/<name>.duckdb_extension.gz`(コアは `extensions.duckdb.org`)へ HTTP HEAD を送って確かめた。200 はその版のバイナリが公開されていること、404 はないことを示す。プローブしたのは linux_amd64 だけである。版の列が descriptor・GitHub の自己申告であるのに対し、バイナリの列は実在する成果物である。

| 名前 | 種別・言語 | 自己申告の版 | バイナリ 1.4.5 / 1.5.6 / 2.0.0 | 最終コミット・規模 | 対応する部品 |
|---|---|---|---|---|---|
| sitting_duck | community, C++ | v1.16.0(2026-10-14) | 404(1.4.4 は 200)/ 200 / 200 | 2026-10-19、35★、open 41 | `nodes`、`units`、`tags` の一部 |
| duck_tails | community, C++ + libgit2(vcpkg)。wasm は除外 | レジストリ v1.8.0、GitHub release v1.7.0(2026-09-17) | 200 / 200 / **404** | 2026-10-19、29★。ライセンスは descriptor が MIT、GitHub が Apache-2.0 | `commits`、ファイル状態、blame、`git://path@rev` |
| proudust/duckdb-git | third-party、未署名、Rust | v0.6.0(2026-08-23)、≥1.5.5 | レジストリ外 | 0★ | `commits` と numstat の `file_changes` |
| protoduck | community, Rust | v0.1.2 | 404 / 404(1.5.5 は 200)/ 404 | 2026-09-17、1★ | SCIP の BLOB 復号 |
| 0xcaff/duckdb_protobuf | third-party、未署名、Rust | v0.0.7(2024-09-11)、README は「duckdb 1.1.0」を指定 | レジストリ外 | 2026-09-26、30★、ライセンス宣言なし | 区切り付き protobuf ファイル |
| duckpgq | community(CWI), C++ | v1.0.0、GitHub release なし | 404(andium の ref はある)/ 200 / 200(2026-10-22) | 2026-10-21、505★、open 33 | グラフ問い合わせ |
| duckgql | community, C++17 | v0.2.0 | 404 / 404(1.5.5 は 200)/ 404 | 2026-10-04、54★ | ISO GQL の部分集合 |
| onager | community, Rust + C++ | レジストリ 0.5.0 | 404 / 200 / 200 | 2026-10-23、152★ | グラフアルゴリズム |
| petgraph_ext | community, Rust | v0.5.1 | 404 / 200 / 200 | 2026-10-10、6★。ライセンスは GitHub が宣言なし、descriptor が MIT | グラフアルゴリズム |
| shellfs | community(Query.Farm), C++ | '2025120401' | 404 / 200 / 200 | 2026-10-23、96★ | `git log`、`jscpd`、`scip print --json` のパイプ |
| read_lines | community, C++ | 0.4.3 | 200 / 200 / 200 | 2026-10-19、6★ | 行範囲の切り出し |
| gh | community(experimental), C++ | v0.0.1 | 404 / 200 / 404 | 2026-04-29 | コード解析ではない |
| nanoarrow / webbed / tantivy / zipfs / duck_hunt / markdown | community | 各 descriptor | 1.5.6・2.0.0 とも 200(webbed は 1.4.5 も 200) | — | 周辺(Arrow IPC、XML、BM25 など) |
| prql / parser_tools / hostfs / fsquery / tarfs | community | — | 2.0.0 はすべて 404。tarfs はプローブしたどの版にもない | prql は 2026-05-28、hostfs は 2025-10-01 が最終 | 無関係か、更新が止まっている |
| vgi-code | Query.Farm の VGI worker(DuckDB 拡張ではない), Rust | v0.1.1(2026-07-21) | 該当なし(別プロセス) | 2026-10-23、1★ | `.scm` の `ts_query`、9 言語 |
| コアの json / sqlite_scanner / httpfs / fts / vss / excel | core | — | 200 / 200 / 200 | — | jscpd・SCIP の JSON、SQLite の取り込み |

以上は一覧担当の ★ である。

### sitting_duck の詳細

sitting_duck v1.16.0 は、1 つのソースツリーを v1.5.6 と 2.0 の両方でビルドしている。v1.5.6 では全テストが通った(9027 assertions、0 failures)。レジストリの 2.0 のビルドは `skip_tests: true` で走るので、descriptor 自身が「COMPILE-compat evidence only」と書いている。リポジトリ側の 2.0 のカバレッジでは、154 passed / 0 failed / 5 skipped だった(★ descriptor)。

2.0 での遅さの測定値は次のとおりである。測ったのはメンテナで、レジストリの 2.0 のコミットを使っている(★ descriptor)。メンテナはこの遅さを「PLANNING, independent of input size and selector complexity」と説明し、原因を duckdb/duckdb#26036 としている。

| 処理 | v1.5.6 | 2.0 | 比 |
|---|---|---|---|
| `ast_select_from` の平均 | 0.468 s | 11.273 s | 約 24 倍 |
| `EXPLAIN` | 0.581 s | 72.211 s | 約 124 倍 |

関連して、issue #160 は「ast_select_from spends ~6 s planning per call」、#164 は 78k ノードの表で呼び出しグラフのセレクタがメモリを使い切り、エラーではなくプロセスが落ちることを報告している(★)。

v1.16.0 では、`descendant_count`・`children_count`・`depth` の値の折り返しが修正された。それまではそれぞれ uint16・uint16・uint8 の写しを経由していたため、ノードが 65,536 を超えるファイルでは部分木が誤っていた(★ descriptor)。

対応言語数の記述は資料によって揃っていない。descriptor は 28 言語とし、README は 27 言語、または 26 の grammar と DuckDB adapter としている(★)。grammar は git のサブモジュールとして入っており、CI は生成済みの `parser.c` を使う。v1.10.0 から `register_language()` で実行時に grammar を読み込めるが、既定では無効である(★)。

### git 層の詳細

| 対象の表 | 現在の取得元 |
|---|---|
| `commits` | duck_tails か proudust/duckdb-git、または `git log` の CSV |
| 行数つきの `file_changes` | proudust/duckdb-git、または shellfs や CSV 経由の `git log --numstat` |
| 状態だけの `file_changes` | duck_tails の `git_diff_tree`(added/deleted/modified/renamed/copied/typechange) |
| `hunks` | 拡張なし |

この対応表は一覧担当の推論、各関数の存在は ★ による。

duck_tails の `git_blame` は rename を考慮し、1 行ごとに orig_commit_hash・orig_path・orig_line_number を返す。`git_blame_hunks` は hunk ごとに start_line と line_count を返す(★)。

DuckDB 以外の先行例の多くは止まっている。

- gitbase:最終コミット 2019-10-29
- mergestat-lite:最終 release v0.6.2(2024-03-13)
- Code Maat:最終 release v1.0.4(2023-02-20)

例外は AmrDeveloper/GQL(GitQL)で、0.43.0 を 2026-03-09 に出した。ただし独自のエンジンで動く(いずれも ★)。

### DuckDB でコード解析をした先行例

構造と履歴を DuckDB で結合した公開プロジェクトは見つからなかった。DuckDB をストアに使った MSR/ICSE のツール論文も見つからなかった(一覧担当 ★ 検索)。最も近いのは、同じ作者による sitting_duck と duck_tails の組である。二つは `git://…@rev` の URI で結合されている(★ issue #282)。

## 先行例のデータモデル

| システム | 事実の単位 | 木・範囲の表し方 | 識別子 | 保存 |
|---|---|---|---|---|
| CodeQL | 型付きの関係(`.dbscheme`。言語ごとに生成され、公開された構文仕様はない) | 木は `ast_node_parent(unique node, parent, parent_index)` と `#keyset[parent, parent_index]`。位置は別表 `locations_default(id, file, beginLine, beginColumn, endLine, endColumn)`。1 始まりで終端を含む | TRAP の `#N=@"key"`。文字列のグローバルキーを import 時に整数へ intern する。キーには列の型を前置することが勧められている | DB ごとに 1 スナップショット |
| Glean | predicate(SQL の表にほぼ相当)と fact(行にほぼ相当)。スキーマもクエリも Angle で書く | — | fact の id。所有者は unit の文字列か、その `\|\|`/`&&` の組み合わせ(uset) | RocksDB。不変の DB を積み重ねる |
| Kythe | node・edge・fact のグラフ。1NF の entry | anchor ノードが `loc/start`(0 始まり)と `loc/end`(終端を含まない)の **byte** オフセットを持つ | VName(language, corpus, root, path, signature)。signature は入力の版をまたいで安定である必要がない | ソート済みの entry ストリーム。SQL 版は Tickets・Nodes・Edges の 3 表 |
| Joern | CPG(有向・ラベル付き・属性付きのマルチグラフ) | AST・CFG・手続き内データフローを 1 つのグラフに統合 | ノード | Neo4j/OrientDB/JanusGraph から、自前の OverflowDB、さらに列指向の flatgraph(ヒープ内)へ移った |
| SCIP | 文書ごとの `Occurrence` | 範囲(`typed_range`)、`symbol_roles` のビット集合、`enclosing_range` | 人が読める symbol 文字列 | Protobuf。Sourcegraph は BLOB として保存 |
| sitting_duck | 1 ノード 1 行 | preorder の `node_id` と `descendant_count`。行は 1 始まり、byte は 0 始まりの半開 | ファイルごとの連番 | DuckDB の表関数 |

### CodeQL

CodeQL の glossary は、スキーマの構文について「There is currently no public-facing specification for the syntax of schemas」と述べている(★)。

Ruby の dbscheme(1,553 行、★raw)には、tree-sitter 由来のスキーマの形がそのまま現れている。

- ノードの種類ごとに表がある(`ruby_method_def` など)。
- 繰り返されるフィールドごとに、`#keyset[parent, index]` の付いた表がある。
- トークンは `ruby_tokeninfo(id, kind, value)` に入り、`kind` の値で種類を区別する。
- 約 100 個の選択肢を持つ union 型 `@ruby_ast_node` がある。

Rust の dbscheme(3,768 行、生成物、★raw)は、任意属性や 1 対 1 の属性を nullable な列にせず、`function_names(id, name)` のような狭い別表にしている。

TRAP の識別子は 2 種類ある(★raw trap.rs)。`fresh_id()` が出すファイル内で匿名の `#123=*` と、`global_id(key)` が出す `#7=@"key"` である。trap.rs は後者の用途を「incrementally importable TRAP files」と説明している。tree-sitter の経路では、位置はファイルごとにハッシュマップで重複を除いた fresh id になる。

位置の変換規則は extractor のコメントに書かれている(★raw extractor/mod.rs)。tree-sitter の row/column は 0 始まりで終端を含まないが、CodeQL は 1 から数え、終端を含む。そのため、終端の列が 0(行頭)の範囲は前の行の行末に移し、空の範囲は `end_column = start_column - 1` に切り詰める。

`codeql dataset import` で既存の dataset に追記できるのは、dbscheme が一致し、「ID pool has been preserved from the initial import」の場合だけである(無印)。

GitHub の推奨ハードウェアは、100K LOC 未満で 8 GB・2 コア、1M LOC 超で 64 GB 以上・8 コア、SSD の空きが 14 GB 以上である(無印)。DB の LOC あたりの大きさや評価時間について、公式の数値は見つからなかった。

### Glean

Meta の記事(2024-12-19、★)には次のことが書かれている。

- クエリが効率的になるのは、predicate のフィールドの prefix を指定したときである。
- 目標は「O(changes) rather than O(repository)」だが、実際の計算量は O(fanout) になる。C++ のヘッダが変わると、それを include するファイルも索引し直す必要があるためである。
- 積み重ねた不変の DB は、下の層に情報を足したり、下の層の情報を隠したりできる。
- 派生 predicate は SQL のビューのように働き、言語に依存しない抽象層にも使われる。
- 1 回の `FunctionDeclaration` の検索は「about a millisecond」で返る。データの大きさは示されていない。

incrementality の docs(★)によると、所有者の伝播は時間も空間も O(facts) である。「Typically we see between 10-100x more facts than sets」とあり、unit を関数単位にすると集合の数が増える。積み重ねた DB の上での派生 predicate の増分計算は未実装である。

スキーマ変更で互換になるのは、既定値を持てる型のフィールドの追加・削除、sum の選択肢の追加・削除、list と set の切り替えである。フィールドの型の変更は非互換になる(★)。

Glean が SCIP を取り込んだとき、SCIP は LSIF より「8x smaller, and can be processed 3x faster」だった(★ Announcing SCIP)。

### Kythe

anchor の例は `int 錨;` で、範囲は 4..7 になる。つまり範囲は文字ではなく byte で数える。0..0 の anchor はファイル全体を指す(★ schema)。

entry は Source・Kind・Target・Fact・Value の順でソートされる。indexer は順方向の edge だけを出し、逆方向の edge は serving のときに合成する。Write は「cannot delete any data from the store」で、ストアからデータを消せない(★ storage)。

### Joern

flatgraph の例は Linux のネットワークドライバで、9M ノード、88M のノード属性、84M エッジ、19M のエッジ属性を、約 3.7 GB のヒープと、約 300 MB の永続化で持つ(★ flatgraph.joern.io)。flatgraph の非機能は、ACID、WAL、リモートアクセス、クラスタリング、「no swapping to disk」である。

docs に挙がっている frontend の中に Rust はない(★)。flatgraph と Neo4j を比べたベンチマークの数値は読めなかった。

### SCIP と Sourcegraph

`symbol_roles` のビットは、Definition 0x1、Import 0x2、WriteAccess 0x4、ReadAccess 0x8、Generated 0x10、Test 0x20、ForwardDefinition 0x40 である。`enclosing_range` は「nearest non-trivial enclosing AST node」を指す(★ scip.proto)。ベンチマークでは、LSP 風の `Range{Position}` メッセージは効率が悪く、packed int32 にすると全体のペイロードが約 50% 減った(★)。

LSIF が置き換えられた理由は二つ挙げられている(★ Announcing SCIP)。一つは、不透明な ID 番号を多用し、「globally incrementing IDs make it difficult … to update an existing index」であること。もう一つは大きさで、LSIF は gzip 圧縮で約 4 倍、非圧縮で約 5 倍大きい。

Sourcegraph の Postgres の表は、2024-09 に archive された公開スナップショットに基づく(★raw)。

| 表 | 役割 |
|---|---|
| `codeintel_scip_documents(payload_hash UNIQUE, raw_scip_payload bytea)` | 同じ文書データを持つ索引どうしで重複を除く |
| `codeintel_scip_document_lookup` | (upload_id, document_path) で一意 |
| `codeintel_scip_symbols` | (upload_id, symbol_id, document_lookup_id) ごとに definition・reference・implementation・type_definition の範囲を、それぞれ「an encoded set of ranges」として持つ |
| `codeintel_scip_symbol_names` | symbol 名の prefix tree |

scip の `expt-convert`(★raw convert.go)は、occurrence を `chunkSizeHint = 200` ごとに `proto.Marshal(&scip.Document{Occurrences: ...})` し、zstd で書く。

github/stack-graphs は 2025-09-09 に archive された(無印、Wayback 経由)。手元の `rust-analyzer 1.99.1` には `scip` サブコマンドがあるが、help は「do not provide any stability guarantees」と注記している(★raw)。

### sitting_duck の木の表し方

sitting_duck は木を preorder で走査する。部分木への所属は `d.node_id > a.node_id AND d.node_id <= a.node_id + a.descendant_count` で判定し、これが「powers `:has()`, combinators, and scope resolution without recursive CTEs」とされている。祖先をたどるコストは O(depth) である。`semantic_type` は 1 byte の `[ss kk tt ll]` で、言語をまたぐ述語に使われる。行は 2048 行のチャンクで DuckDB に流され、ベンチマークは公開されていない(★ architecture docs)。

### 関数とクローンの履歴

関数の履歴を追跡する道具の精度は、次のように報告されている(いずれも無印、ほぼ Java)。

| 道具 | 精度 | 実行時間 | 評価の条件 |
|---|---|---|---|
| CodeShovel(ICSE 2021) | メソッド履歴の約 90% を完全に正しく復元 | 中央値約 2 s | OSS 20 リポジトリのオラクル |
| CodeTracker(ESEC/FSE 2022) | precision・recall とも 99.5% | 平均 3.6 s | 1,280 コードブロック |
| HistoryFinder(ICSE 2026) | 訂正版オラクルで CodeShovel の recall が「drops significantly」。git ベースの道具は最速だが精度は大きく劣る | — | 40 リポジトリの 400 メソッド |

クローンの系譜(Kim, Sazawal, Notkin & Murphy 2005)は、バージョンをまたいで追跡したクローンクラスとして定義され、一貫した変更・一貫しない変更などの変化パターンを持つ。二次要約(Bettenburg et al. WCRE 2009)によると、クローン群の最大 72% が 8 コミット以内に消え、最大 38% は常に一貫して変更された。Krinke は、クローン群への変更の約半分が一貫しない変更だと報告している。これらの数値は原典と照合していない(無印・二次)。stored clone pair の関係スキーマを記述した文献は見つからなかった。

## スキーマと問い合わせに対して先行例が言えること

この節は観察であり、推奨ではない。

- **木の表し方は 2 通りある。** CodeQL は親ポインタと兄弟インデックスで木を表し、再帰は Datalog が受け持つ(★raw)。sitting_duck は preorder の id と `descendant_count` による区間で表し、再帰なしの範囲結合で部分木を判定する(★)。区間の表し方が成り立つのは、ファイルの版ごとに parse し直して新しい行にする場合だけである。編集すると id がずれる(推論)。この区間の表し方では件数の列の型が効く。sitting_duck では uint16 の写しが原因で、65,536 ノードを超えるファイルの部分木が誤っていた(★)。
- **位置の規約は 3 系統あり、互いに互換でない。** tree-sitter・Kythe・sitting_duck の byte 範囲は 0 始まりで終端を含まない(★)。CodeQL は 1 始まりで終端を含む(★raw)。sitting_duck の列番号は 1 始まりである(★)。SCIP は line/character で、文字の符号化方式を宣言する(★)。終端を含まない範囲から含む範囲へ変換するとき、CodeQL の extractor は、行頭(列 0)で終わる範囲を前の行に移し、空の範囲を切り詰めている(★raw)。`hunks`(git の行範囲)と `units`(tree-sitter の範囲)を区間結合するとき、この変換はどちらかの側で明示的に行う必要がある(推論)。
- **位置を別表にする設計がある。** CodeQL は node → loc id → (file, 行, 列) と正規化し、ファイルごとに intern している(★raw)。Sourcegraph は document path の繰り返しを減らすために lookup id を置いている(★raw)。
- **コミットをまたぐ同一性は、どの先行例も解いていない。** CodeQL と Kythe の識別子は 1 DB の中だけで一意で、Kythe の signature は版をまたいで安定である必要がない(★)。大規模な基盤は内容ハッシュで識別している(無印)。関数の履歴とクローンの系譜は、どちらも照合の工程として扱われている(無印)。`unit_changes` で「同じ unit」をどう定義するかは、スキーマ側で決めることになる(推論)。Glean の unit 所有者は、どの fact が見えるかを決める仕組みであって、版をまたぐ同一性の仕組みではない(★)。
- **occurrence を BLOB で持つか行で持つかで、先行例は BLOB に寄っている。** Sourcegraph と scip `expt-convert` は occurrence を BLOB で持ち、点検索(go-to-def、find-refs)を前提にしている(★raw)。occurrence を関係演算で結合する前提の保存形は、調べた先行例にはなかった。`enclosing_range` は、AST と結合せずに参照を囲む関数へ帰属させるための事実だが、rust-analyzer・scip-go・scip-typescript がこれを埋めるかは確かめていない。
- **Glean ではフィールドの順序が索引の選択になる。** prefix を指定したときに効率的なので、predicate のフィールドの順序が事実上の索引の選択になる(推論、★ 記事)。unit を関数単位にすると所有者の集合が増える(★)。
- **再帰 CTE と DuckPGQ には、それぞれ数値と注意点がある。** 2.0 の `USING KEY … UNION` は、新しいキーか変化したキーだけを次へ渡し、キーの状態が変わらなくなった時点で止まる。公式ブログ(2026-08-25、★)の測定値は次のとおりである。

  | ベンチマーク | v1.5.5 | 2.0 preview | 比 |
  |---|---|---|---|
  | 到達可能性(1M エッジ / 100k ノード) | 4.051 s | 0.095 s | 42.6 倍 |
  | LDBC SF100 の経路探索 | 19.319 s | 2.948 s | 6.55 倍 |
  | 再帰ベンチマーク 63 件の幾何平均 | — | — | 5.5% の改善 |

  DuckPGQ は自らを研究プロジェクトと称している。2.0 では接続ごとに文法を有効にする必要があり、`GRAPH_TABLE` を使うビューはファイル型の DB では commit 時に拒否される。SIGSEGV の issue(#304、#319)が開いたままである(★)。2.0 での性能値はない。
- **クエリが計算する信号については、兄弟レポート「コード品質解析手法の動向 2026」が整理している。** 欠陥予測ではプロセスメトリクスがプロダクトメトリクスを上回る(Majumder et al. 2022, ★(abstract))。クローンの害の予測子として支持があるのは、共変更、遅延伝播、意図しない不整合である(Juergens et al. 2009 ★、Mondal et al. 無印)。アーキテクチャのアンチパターンで寄与が大きいのは、高い fan-in と高い共変更を組み合わせた Unstable Interface である(Mo et al. 2021, 無印)。これらはいずれも、`unit_changes` と `clone_pairs` をコミットをまたいで結合する問い合わせ、つまり上の同一性の問題に依存する(推論)。共変更結合は閾値に強く依存し、rename を追跡しないとファイルを移動するたびに系譜が切れる(同レポート、無印と推論)。

## 未検証・未調査

各調査者の検索予算は限られていた。「見つからなかった」は「存在しない」の証明ではない。

**拡張と配布**

- C 拡張 API が最初に安定化された DuckDB の版(PR #14992 は未読)。
- duckdb.org の「C API extensions」のページは取得しておらず、v1 と v2 の対応範囲(replacement scan、copy function、カスタム型、カタログへのアクセス)は第三者の DuckHTS #254 にしか拠っていない。
- C API での progress reporting と filter pushdown。
- 2.0 で SET の `custom_extension_repository` が `CREATE EXTENSION REPOSITORY` と並んで残っているか。
- community extensions の審査方針・署名方針の本文と、CI のワークフローファイル。
- 版の更新で壊れた件数の集計(個別の PR しか見ていない)。
- 拡張の表関数と、Python からの INSERT や Arrow での取り込みを比べたベンチマーク。
- Rust 拡張や C API 拡張の行数・ビルド時間を示した DuckCon の講演やブログ。
- `read_text`/`read_blob`/`glob` の詳細と、Arrow のゼロコピーの具体。

**既存拡張**

- `read_ast` のスループット(files/s、rows/s、MB)。
- 素の `read_ast` が 2.0 で遅くなるか。
- sitting_duck の対応言語数の不一致と、Haskell/Julia/Scala/F# が実際にビルドされているか。
- duck_tails の 2.0 のビルドが失敗したのか、まだ走っていないだけなのか(レジストリの CI ログは未確認)。
- proudust/duckdb-git に v2.0.0 向けの release asset があるか。
- コアの DuckDB に、SCIP の SQLite の BLOB を zstd 展開できるスカラーがあるか。
- `scip print --json` の出力の大きさと、大きな索引での性能。
- protoduck の 2.0 対応(descriptor に `ref_next` がない)。
- DuckPGQ の 2.0 での性能値(CIDR/VLDB の論文は未読)と、match rewrite が実行できる経路モードの範囲。
- onager の 2.0 での対応範囲とメモリモデル。
- shellfs のパイプ構文(query.farm の文書は未取得)。
- DuckDB をストアにした MSR 論文、DuckCon のコード解析の講演。

**先行例**

- CodeQL の DB の LOC あたりの byte 数・TRAP の大きさ・評価時間の公式な数値(ECOOP 2016 の本文は未読)。TRAP の文法には公開された仕様がない。
- Glean の fact 数と byte 数、Kythe の serving 表の大きさと性能、Sourcegraph の表の大きさと occurrence あたりの byte 数。
- Joern v4.0.0 の日付、flatgraph と Neo4j の比較値。
- SCIP の indexer(rust-analyzer、scip-go、scip-typescript)が `enclosing_range` を埋めるか。scip-code への移管の理由と日付。
- SQL に格納した AST の公開測定(KLOC あたりの行数、byte 範囲の包含結合の性能)と、コードの AST で nested set と path enumeration を比べた研究。
- World of Code(EMSE 2021, arXiv 2010.16196)と Software Heritage の SANER 2020 の PDF は、取得しても読めないバイナリだった。WoC の map 名と大きさ、SWH のエッジあたりのビット数は確かめていない。
- クローンの系譜の数値(72%、38%、約半分)は原典と照合していない。jscpd の出力スキーマと、系譜の道具がペアや群をどう保存するかは調べていない。FinerGit の数値は見つからなかった。
