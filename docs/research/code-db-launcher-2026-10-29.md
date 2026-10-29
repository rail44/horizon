# コードデータベースの起動コマンド `code-db`(2026-10-29)

`duckdb-ext/sql/run.sh` を Rust のコマンド `code-db` に置き換えた増分の記録である。起動側の責任(解析するリポジトリの設定ファイルを読むこと、DuckDB の資源の上限を決めること、拡張と SQL を順に読み込むこと、索引を作るツールを起動すること)を 1 つのプログラムに集め、DuckDB には値だけを渡す。設定ファイルを除いて、挙動は変えていない。

## オーナーの決定(2026-10-29)

- 「人間の判断であったりプロジェクトの方向性で左右されるものは…設定やデータのような変更の効くライフサイクルの短い部分で担うべき」。プロジェクトに依存する判断は、ビューやマクロではなく設定ファイルに置く。
- 設定ファイルは、解析する側のリポジトリの直下の `.code-db.toml` とする。無ければ今の挙動のままとする。
- そのファイルを DuckDB(sitting_duck の TOML パーサや SQL)で読む案は退けられた:「duckdb拡張の動作についての設定ファイルという立ち位置になるわけですが、これを読むのにduckdbを用いることへの違和感が強い」。起動側が読む。
- オーナーは選択肢 B を選んだ:「Bで」。`duckdb-ext/` に置く小さな Rust のコマンドが `run.sh` を置き換え、`toml` クレートでファイルを読み、`host-check` と同じように `duckdb` クレートで DuckDB を開き、値を変数と表として渡してから SQL を読む。上限の計算(`/proc/meminfo`)も同じ理由でこのコマンドに移す(DuckDB が自分の上限を計算しない)。
- この増分に含めないもの:SCIP からモジュールの境界やテストを導くこと、`.git-blame-ignore-revs` を読むこと、`layers` に要るもの以外の新しい信号、マージの両方の親との差分。

## 作ったもの

### クレートと依存

`duckdb-ext/code-db/` に、独自のワークスペースのクレート `code-db`(バイナリ名 `code-db`)を置いた。`host-check/` と同じく、拡張のワークスペースに入れると cargo が `libduckdb-sys` の feature を統合し、`loadable-extension-v2` が libduckdb のリンクを止めてしまうためである。`duckdb = "=1.20000.0"`(`default-features = false`)を、AGENTS.md の Build setup のとおり機械に入れた libduckdb 2.0.0 にリンクする(`ldd` で `~/opt/duckdb/current/libduckdb.so` を確かめた)。

依存は `duckdb` のほかに、`serde`(derive)と `toml` 0.8(設定ファイル)、`libc`(`statvfs`、スピル先のファイルシステムの空き)、`unicode-width` 0.2(markdown の列幅)である。開発用に `tempfile`。どれもルートの `Cargo.lock` にある版である。`duckdb` クレートは `default-features = false` でも arrow を引き込む(`host-check` と同じ)。

`duckdb-ext/build.sh` が拡張の後に `cargo build --release --locked` で作り、`duckdb-ext/code-db/target/release/code-db` に置く。SQL ファイルと拡張は、ビルドしたチェックアウトのもの(`CARGO_MANIFEST_DIR` の 2 つ上)を読む。

| ファイル | 役割 |
| --- | --- |
| `src/main.rs` | 引数、サブコマンド、読み込みの順、DuckDB に渡す前置き(変数と表) |
| `src/settings.rs` | `.code-db.toml` の型と検査 |
| `src/limits.rs` | 上限の導出、`/proc/meminfo` の読み取り、DuckDB と同じ大きさの書式 |
| `src/db.rs` | `duckdb::ffi`(C API)による 1 つの接続、文ごとの実行と結果の文字列化 |
| `src/script.rs` | 文の分割、ドットコマンド(`.print` と `.read`) |
| `src/render.rs` | DuckDB CLI の markdown モードと同じ表の出力 |
| `src/index.rs` | `index`:jscpd と rust-analyzer をメモリ上限のもとで起動する |
| `tests/end_to_end.rs` | フィクスチャのリポジトリでの通しのテスト |

### サブコマンド

| サブコマンド | 内容 |
| --- | --- |
| `index REPO [tool_memory_max=SIZE]` | `run.sh index` と同じ。jscpd(`jscpd.sh`)と `rust-analyzer scip` を、`MemAvailable` の 80%(または `tool_memory_max`)の上限のもとで起動する。`systemd-run --user` が使えればそれ、無ければ `prlimit --as`、どちらも無ければ上限なし(警告を出す) |
| `query REPO QUERY [NAME=VALUE ...]` | 設定ファイル、上限、読み込みの後に `queries/QUERY.sql` を実行して出力する。`FORMAT`、`LIMIT` は `run.sh` と同じ |
| `sql REPO FILE.sql [NAME=VALUE ...]` | 同じ読み込みの後に FILE.sql を読む |
| `shell REPO [NAME=VALUE ...]` | 同じ読み込みを DuckDB CLI の `-init` ファイルにして CLI を起動する |
| `limits REPO [--json] [memory_limit=… threads=… temp_limit=…]` | 読み込みが設定する上限を出力する。`--json` は機械向け(`memory_limit`、`threads`、`temp_limit`、`temp_directory`、`line`) |

### SQL とシェルから移ったもの

- **上限の計算**。`00_load.sql` から `read_text('/proc/meminfo')` による計算、マクロ `setting_bytes`、変数 `limits_*` と `temp_free_bytes` を除いた。`code-db` が `/proc/meminfo` の `MemAvailable`、DuckDB の既定値(`current_setting` で DuckDB に尋ねる)、`statvfs` によるスピル先の空きから同じ規則で値を決め、変数 `memory_limit`、`threads`、`temp_limit`、`temp_directory` と、要約の行 `limits` として渡す。`00_load.sql` はそれを適用するだけで、未設定の変数については DuckDB の既定値のままにする(`RESET` の後 `coalesce(変数, current_setting(…))` を `SET` する)。最後の文は変数 `limits` を返し、無ければ実際の設定値からなる行を返す。
- **読み込みの順と前提の確認**。拡張の有無、jscpd のレポートの有無、索引が要るクエリの判定(「Needs views/scip.sql / graph.sql」)、ビューのファイルの順(`views/scip.sql` と `views/graph.sql` は索引があるときだけ)、`refresh.sql`。
- **変数の受け渡し**。`NAME=VALUE`(数・`true`・`false`・`NULL` はそのまま、ほかは文字列)と `NAME:=EXPR` の規則は `run.sh` と同じにした。
- **ツールの起動**。メモリ上限の選択、失敗の報告(終了状態かシグナルと上限)、前の `index.scip` を残すこと。
- 拡張の `LOAD` は `00_load.sql` に残した(下の「決めたこと」4)。

### 設定ファイル

依頼文の形のとおりである。`[scope] exclude` が変数 `exclude_patterns` に、`[tests] paths` と `extra_attributes` が新しい変数 `extra_test_paths` と `extra_test_attributes` に、`[[layers]]` が表 `config_layers(name, module_pattern)` と `config_layer_deps(name, may_depend_on)` になる。SQL の規則の変更は `lang/rust.sql` の 2 箇所だけで、テストの属性名の一覧(`test`、`rstest`、`test_case`、`bench`)に `extra_test_attributes` を、ファイルのパスの規則に `extra_test_paths`(GLOB 演算子のパターン)を足した。どちらも既定は空である。

層については、`views/graph.sql` に `module_layer_matches`、`module_layers`、マクロ `graph_layer_violation(src_layer, dst_layer)`、`module_layer_violations` を足し、クエリ `queries/layer_violations.sql` を加え、`module_cycles` クエリの出力の最後に列 `violating_edges` を足した。何も消していない。

## 決めたこと(依頼文で決まっていなかったもの)

1. **置き場所と起動の仕方**。`duckdb-ext/code-db/` の独自のワークスペースとし、`build.sh` がリリースビルドする。SQL と拡張の場所はビルド時のパスから決める(環境変数による上書きは作っていない)。
2. **どこで実行するか**。既定の `FORMAT`(markdown)では、読み込みもクエリも `code-db` のプロセスの中で `duckdb` クレートを通して実行し、結果は `code-db` が CLI の markdown モードと同じ形で書く。ほかの `FORMAT`(`box`、`csv`、`json` など)と `shell` は、同じ読み込みを CLI のスクリプトにして DuckDB CLI に渡す。`duckdb` クレートには CLI の各モードの描画も対話の機能も無いためである。CLI に渡すときも、上限・設定・変数は `code-db` が決めた値をスクリプトの先頭に書く。`shell` の `-init` ファイルは `REPO/target/code-db/shell-init-PID.sql` に書き、終了時に消す。
3. **C API を直接使う**。`duckdb` クレートの `Connection` は生の接続を出さず、DuckDB の変数は接続ごとなので、`duckdb::ffi` で 1 つの接続を開いてすべてを流す。値の文字列化は、C API の `duckdb_value_varchar` が入れ子の型(リスト、構造体、マップ)に空文字列を返すため、SELECT 文を `SELECT CAST(COLUMNS(*) AS VARCHAR) FROM (…)` で包んで DuckDB 自身に文字列化させ、列名と型は包む前の文の準備から取る。包んだ文が準備できないとき(DuckDB が SELECT に書き換える `PRAGMA version` など)は、書いたままの文を実行する。SELECT 以外で入れ子の列を返す文はエラーにする(空で出すより止める)。EXPLAIN は CLI と同じく計画の文字列だけを出す。CLI の markdown の列幅は、表示幅(東アジアの全角は 2、タブは 8 の倍数まで)で数え、値に改行があれば最後の行の幅になる。これらは CLI の出力と比べて合わせた。
4. **拡張の LOAD は `00_load.sql` に残した**。`check.py` や手で CLI を使う利用者が同じファイルを読むためである。`code-db` は拡張のファイルがあることを確かめ、作業ディレクトリを Horizon のチェックアウトの直下にする(`LOAD` は相対パスのリテラルしか取らない)。
5. **文の分割**。CLI の `.read` に合わせるため、引用符・コメント・`$$` の外の `;` で分ける自前の分割器を置き、各片が DuckDB の `duckdb_extract_statements` で 1 文になることを確かめる(ずれたらエラー)。ドットコマンドは、ここの SQL ファイルが使う `.print` と `.read` だけを扱い、ほかはエラーにする。
6. **上限の導出の細部**。DuckDB の既定値(`memory_limit`、`threads`、`max_temp_directory_size`)は、`code-db` が開いたばかりの DuckDB に `current_setting` で尋ねる。以前の式の「この DuckDB がすでに持っている量(`duckdb_memory()`)」の項は、新しいプロセスでは 0 なので落とした。空きは `df` でなく `statvfs` で読む。空きが読めないときの要約行の文言は「temp_free_bytes unset」から「free space unknown」に変えた(その変数が無くなったため)。`memory_limit`、`threads`、`temp_limit` の上書きは `NAME=VALUE` だけを受け、`code-db` が解釈できない大きさはエラーにする(以前は DuckDB が受け付けた値をそのまま使った)。
7. **未設定のときの `temp_directory`**。`00_load.sql` は変数が無ければ DuckDB の既定(作業ディレクトリの `.tmp`)のままにする。`code-db` は常に `REPO/target/code-db/tmp` を渡すので、`code-db` 経由の挙動は変わらない。手で CLI から読む場合だけ、スピル先が変わる。
8. **`check.py` の上限**。接続ごとに `code-db limits REPO --json` を呼び(`CODE_DB` で場所を変えられる)、`--set` の `memory_limit`、`threads`、`temp_limit` をその引数にする。同じ接続で `00_load.sql` を読み直しても、上限は計算し直さない(以前は読み直すたびに計算した)。
9. **設定ファイルの検査**。依頼文の「すべての項目に `reason` が必須、未知のキーと `reason` の欠落はキーと行を示すエラー」に加え、層の名前の重複と、宣言されていない層を指す `may_depend_on` もエラーにした。依頼文の例の最初の層(`board`)には `reason` が無いが、「すべての項目」に従って層にも `reason` を必須とした。エラーの文は `toml` クレートのもの(行と列、該当箇所の抜き出し、「missing field `reason`」「unknown field `…`」)である。
10. **設定の値と命令行の変数**。設定ファイルの値は空でないときだけ変数に入れ、その後に命令行の `NAME=VALUE` を設定する。命令行が設定ファイルを上書きする。
11. **テストのパスの照合**。`tests.paths` はリポジトリからの相対パスに GLOB 演算子で当てる(`*` は `/` をまたぐ)。`exclude_patterns` と同じ規則であり、`**/x` はリポジトリの直下の `x` には当たらない。
12. **層の照合と違反の定義**。モジュール(`modules.module`:ファイルのパス、`mod` ブロックは `<path>::<名前>`)に GLOB 演算子で当てる。`src/x.rs*` と書けばファイルとその中の `mod` ブロックの両方に当たる。どの層にも入らないモジュールは、出る辺も入る辺も制約しない。同じ層の中の辺は違反にしない。`may_depend_on` は推移的に扱わない。`layer_violations` は `module_edges` のすべての辺(親子のモジュールの辺、同じ型の impl の辺、テストの辺を含む)を対象とし、`violating_edges` は循環の辺(`module_cycle_edges`、親子と同じ型の辺をまとめた後のもの)のうち違反の数である。
13. **2 つの層に当たるモジュール**。`code-db` が読み込みの直後に `module_layer_matches` を引いて、モジュールとその層の一覧を示して止まる。CLI に渡す経路では、スクリプトの中で `module_layers` を読み、ビューの `error()` で止まる。ビュー `module_layers` はほかのクライアントでもこの場合にエラーになる。
14. **永続キャッシュの鍵**。`refresh.sql` の derive key は導出に使うビューとマクロの定義の文字列であり、変数の値を含まないため、テストの規則を変えてもキャッシュのタグが古いまま残る。`extra_test_paths` と `extra_test_attributes` の値を(空でないときだけ)鍵に加えた。依頼文が規則の変更を `lang/rust.sql` に限ったのに対し、これはキャッシュの鍵の変更である。
15. **列の位置**。`violating_edges` は `module_cycles` の出力の最後の列にした(既存の列の位置を変えないため)。循環ごとの数なので、同じ循環のすべての行に同じ値が入る。`layer_violations` の列は、両端の層、両端のモジュール、`module_edges` の数の列、`nested_modules`、その辺が入っている循環(無ければ NULL)である。
16. **jscpd**。`jscpd.sh` はそのまま残し、`code-db index` から起動する。`scope.exclude` は jscpd の対象には効かない(今までどおり `JSCPD_PATHS` か既定のディレクトリ)。
17. **メモリ上限の仕組み**。`setrlimit` を直接使わず、`run.sh` と同じく `systemd-run` と `prlimit` のコマンドを使う。
18. **`LIMIT` の包み方**。`run.sh` の `sed 's/;[[:space:]]*$//'` と同じく、各行の末尾の `;` を除いてから包む。

## 測ったこと

すべて、このワークツリーの `target/code-db/head`(HEAD 7e84967d の使い捨てのクローン)で行った。`run.sh index` で索引と jscpd のレポートを作り(rust-analyzer 190.4 s、上限は `prlimit --as` で 32.5 GB)、その後クローンの `target/debug`(2.7 GB)を消した。

### `run.sh` との出力の一致

SQL を変える前に、`queries/` のすべてのクエリ(15 本)を既定の変数・markdown で `run.sh` と `code-db`(デバッグビルド)で実行し、標準出力をバイト単位で比べた。

| クエリ | 結果 |
| --- | --- |
| `clone_change_consistency`、`clone_site_tags`、`clone_site_tags_summary`、`cochange_without_reference`、`file_cochange`、`file_cochange_thresholds`、`module_cycles`、`module_graph`、`unit_churn`、`unit_cochange`、`unit_fan_in_churn` | 11 本とも一致(最大は `unit_churn` の 3,476,330 バイト、9,295 行) |
| `compare_cycles`、`compare_graph`、`compare_units`、`unit_dependencies` | 既定の変数では両方とも同じメッセージで止まる(標準出力は空)。標準エラーは `code-db: ` の接頭辞だけが違う |

追加で次の 7 件も比べ、すべて一致した:`unit_dependencies`(`TerminalSession.spawn` を指定)、`compare_units compare_before=HEAD~30`(6,806,382 バイト)、`unit_churn LIMIT=7`、`module_cycles FORMAT=box LIMIT=12`、`file_cochange FORMAT=csv min_coupling_pct=50`、`cochange_without_reference 'exclude_patterns:=[…]' LIMIT=20`、`.print`・複数の文・空の結果・リストと構造体の値・`|` を含む値を持つ `sql` ファイル。`FORMAT=box` と `csv` は CLI に渡す経路、ほかは `code-db` の中の描画である。

SQL を変えた後にもう一度 15 本と新しい `layer_violations` を実行し、`run.sh` の出力と比べた。14 本は一致し、`module_cycles` は最後の列(`violating_edges`、すべて 0)を除くと一致した。`layer_violations` は設定ファイルが無いので 0 行である。この後で `run.sh` を消した。その後、CLI と比べて描画を 3 点直した(複数行の値の列幅、EXPLAIN、`PRAGMA version` のような文。「決めたこと」3)。直した後のバイナリで 15 本と追加の 7 件をもう一度実行し、同じ結果だった(追加の `module_cycles FORMAT=box` は新しい列の分だけ違う)。

時間は、標準エラーの上限の行を除いた 1 回ずつの壁時計で、`run.sh` が 2.4〜5.6 s、`code-db` が 2.3〜6.0 s だった(最初のクエリはキャッシュの作成を含み、`run.sh` 33.1 s、SQL を変えた後の `code-db` 14.5 s)。

### `board_flow` の 2 つの読み(`code-db-dogfood-reading-3-2026-10-28.md` §11)

クローンの直下に一時的に `.code-db.toml` を置き、層を 2 つ宣言した。`board` は `crates/horizon-agentd/src/board_flow/**`、`session` は `crates/horizon-agentd/src/session/**` である。リポジトリには `.code-db.toml` を入れていない(層の構成はオーナーの未決の判断である)。

- **読み A(入口は `session/` に置く)**。2 本の辺は入口から機能への委譲で、`board_flow` から `session` の状態への依存も意図どおり、という読みなので、`board` の `may_depend_on = ["session"]`、`session` の `may_depend_on = ["board"]` とした。`layer_violations` は 0 行、循環 2 番(`board_flow/operations.rs` を最小の要素とする 9 モジュール、16 辺)の `violating_edges` は 0 で、ほかのすべての循環も 0 だった。循環 2 番は `module_cycles` に残る。
- **読み B(`board_flow/` が唯一の置き場)**。`board` の `may_depend_on = ["session"]`、`session` は何にも依存できない、とした。`layer_violations` は次の 2 行だった。

| src_layer | dst_layer | src_module | dst_module | unit_edges | occurrences | nested_modules | cycle |
| --- | --- | --- | --- | ---: | ---: | --- | --- |
| session | board | `session/board.rs` | `board_flow/operations.rs` | 1 | 1 | false | `board_flow/operations.rs` |
| session | board | `session/connection.rs` | `board_flow/operations.rs` | 1 | 1 | false | `board_flow/operations.rs` |

(パスはすべて `crates/horizon-agentd/src/` の下。テストの辺・マクロの辺・曖昧な辺は 0。)循環 2 番の `violating_edges` は 2(16 辺のうち)で、ほかの 23 個の循環はすべて 0 だった。記録が手で追った 2 本(`session/board.rs:105` の `operate`、`session/connection.rs:113` の `organizer_session`)と同じ辺である。

**`session/` の中の 6 モジュールの循環は SQL でも存在した**。読み B の違反の辺を `module_cycle_edges` と同じ辺の集合(親子の辺と同じ型の impl だけの辺を除いた `module_edges`)から除き、`views/graph.sql` と同じ到達可能性の再帰で強連結の組を求め直すと、`horizon-agentd` の中の組は次の 3 つだった。

| 組(最小の要素) | モジュール | 要素 |
| --- | ---: | --- |
| `session/environment.rs` | 6 | `environment.rs`、`exploration.rs`、`panic.rs`、`resume.rs`、`run.rs`、`spawn.rs` |
| `session/attachment.rs` | 3 | `attachment.rs`、`state.rs`、`subscription.rs` |
| `session/approval.rs` | 2 | `approval.rs`、`completion.rs` |

6 モジュールの組は記録の手での数え直しと同じ要素で、`board_flow/operations.rs`、`session/board.rs`、`session/connection.rs` は循環から外れた。下の 2 つの組は、層を置く前から `module_cycles` にある別の循環である。読み A では同じ計算で 9 モジュールの組がそのまま残る。この数え直しは確かめのための SQL であり、クエリとしては加えていない。

循環 2 番の内側の辺(`module_cycle_edges`)は次の 16 本だった(`unit_edges`)。`board_flow/operations.rs` → `session/connection.rs`(3)・`resume.rs`(1)・`spawn.rs`(2)、`session/board.rs` → `board_flow/operations.rs`(1)、`session/connection.rs` → `board_flow/operations.rs`(1)・`session/spawn.rs`(1)、`environment.rs` → `board.rs`(1)・`exploration.rs`(1)、`exploration.rs` → `spawn.rs`(1)、`panic.rs` → `resume.rs`(1)、`resume.rs` → `spawn.rs`(2)、`run.rs` → `environment.rs`(5)・`panic.rs`(5)・`resume.rs`(1)、`spawn.rs` → `panic.rs`(6)・`run.rs`(1)。

そのほか、同じクローンで、`board_flow/mod.rs` を 2 つの層に当てた設定は `code-db` が読み込みの直後にモジュールと層を示して止まり(`FORMAT=csv` の経路ではビューの `error()` で止まった)、`reason` を欠いた `[scope] exclude` は TOML の行と列を示すエラーになった。

### テストとチェック

- `cargo test`(`duckdb-ext/code-db`):単体テスト 19 件(設定ファイル:正しいファイル、空のファイル、`reason` の欠落、未知のキー、宣言されていない層への依存、2 つの層に当たるモジュール。上限:偽の `/proc/meminfo` からの導出、既定値による頭打ち、`/proc/meminfo` が無い場合、2 GB の下限、上書き、大きさの解釈と書式、DuckDB の `formatReadableDecimalSize` との一致。markdown の描画、文の分割、ドットコマンドの引数、変数のリテラル、`tool_memory_max` の大きさ)と、通しのテスト 1 件。通しのテストは、2 つの層(`model` は何にも依存できず、`view` は `model` に依存できる)と、テストのパス・属性の設定を持つフィクスチャのリポジトリに rust-analyzer の索引を作り、`layer_violations` が `model` → `view` の 1 行(循環 `src/model.rs` の中)を返すこと、`module_cycles` の 2 行の `violating_edges` が 1 であること、`#[my_check]` の関数と `src/checks_extra.rs` にテストのタグが付くことを確かめる。
- `check.py fixture`:グラフのフィクスチャに層の段を加えた(`config_layers` と `config_layer_deps` を `code-db` と同じ形で埋め、違反 1 本と `violating_edges` 1、依存を消して違反 2 本と 2、2 つの層に当たるモジュールのエラー、空に戻して 0 行と 0)。全段が通った。
- `check.py refresh`(HEAD のクローン):1 回目 0.39 s、2 回目 0.46 s の更新で、どちらもライブと等しく、`refresh: ok`。キャッシュは、その前の `code-db` の実行で `lang/rust.sql` の定義の変更により導出し直されていた。
- `check.py explain`(HEAD のクローン):7e84967d の `duckdb-ext/sql/`(元の `check.py` と SQL)と比べ、違いは 2 つだった。`module_cycles` のライブの計画の READ_TEXT が 57 から 59 に増えた(`violating_edges` のために `module_layers` を読み、それが `modules` を通してファイルと unit を読む。層が空でも計画には入る)。`layer_violations` が加わった(READ_AST 1、READ_TEXT 67、READ_SCIP 2)。READ_AST の数はどのクエリも変わらない。

## 未確認と限界

- 設定のテストの規則を変えたときに永続キャッシュが導出し直されること(「決めたこと」14)は、テストしていない。
- `systemd-run --user` のある機械では動かしていない(この機械には無く、`prlimit` の経路だけを通した)。
- markdown 以外の `FORMAT` で一致を確かめたのは `box` と `csv` だけである。CLI に渡す経路なので、ほかのモードも CLI の出力そのものになるはずだが、確かめていない。
- `code-db` の中の描画は、ここのクエリと `sql` ファイルが返す型(整数、小数、文字列、真偽値、日付と時刻、リスト、構造体、マップ、NULL、複数行の値、タブ)で CLI と比べた。それ以外の型(UNION、VARIANT、GEOMETRY、BIT など)は比べていない。
