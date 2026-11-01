# コードデータベース:複雑度の言語ごとの構文(2026-11-01)

名前空間の段の fan-in(`code-db-namespace-fan-in-2026-11-01.md`)の次の手順。`unit_complexity` は tree-sitter-rust のノード型(`if_expression`、`match_arm`、`try_expression` など)を直に数えており、Go の unit はすべて 0 だった。列名も Rust の語(`match_count`、`arm_count`、`let_else_count`、`try_count`、`closure_count`)だった。

## 1. オーナーの決定(2026-11-01、3 点とも推奨の形)

- 列名は、言語ごとの正式名の表示を構文の語にも広げる:Go の表示で `match` は `switch`、`arm` は `case`、`closure` は `func_literal`。言語にない構文(Rust の goto、Go のガード、`?`、let-else)の列は NULL。
- Go の判断点は McCabe の数え方:default 以外の case を各 1(switch は網羅的でない)、条件のない `for {}` は 0。
- `goto` は `break`/`continue` と同じく数え、判断点にしない(`goto_count`、Rust は NULL)。

## 2. 形

- 言語ごとの差し込み口 `<language>_control_nodes(file_id, node_id, construct, decisions, opens_level, group_id)`:言語のファイルの木(`file_ast`、言語のファイルより先に読まれる)のノードを中立の構文(if、else_if、match、arm、guard、for、while、loop、try、and、or、let_else、return、break、continue、goto、closure)に分け、McCabe の重み、入れ子の段を開くか、全員のうち 1 つを除いて判断点になる選択肢の組(Rust の match の腕の親、match_block)を付ける。Rust は quality.sql の規則をそのまま移し、Go は tree-sitter-go のノード(`if_statement`、`expression_switch_statement`/`type_switch_statement`/`select_statement`、`expression_case`/`type_case`/`communication_case`、`for_statement` の子が for_clause/range_clause か条件だけか何もないか、`&&`/`||`、`goto_statement`、`func_literal`)で書いた。
- 共通の `unit_complexity`:unit 自身のノード(`unit_own_ast`)について構文ごとの数、`decisions`(重みの和から組ごとに 1 を引く)、開いた段の最大を出す。言語にない構文は、`lang_terms` の正式名が NULL の行で知り、その列を NULL にする。
- 表示:`lang_terms` の行を構文の語にも広げ、置き換えを `_` で区切った語の単位にした(以前の部分文字列の置換では、`try` を足すと `entry_point` が変わる)。SQL は `term_swap`、code-db は `swap_words` で、言語の全語を一度に置き換える。

## 3. 確認

- Rust:20 の層を変更前(06a89b77)と比べた。`duckdb-ext/`(252 units)は差 0 行。Horizon のルートでは、古い索引(macOS の修正を merge する前の木から作ったもの)で `unit_complexity` の 3〜8 行が違った。古い索引の範囲が今のファイルの途中を切ると、腕の一部や `else` の親(let_declaration)が unit の外に出るためで、旧の数え方は親を unit 自身のノードの中から、新しい差し込み口はファイル全体の木から探す。いまの木で索引を作り直すと(8,722 units)、20 の層すべて差 0 行。腕は、最初は「最初の腕だけ 0」としたが、範囲が切れたときにも旧と一致するよう、選択肢の組ごとに 1 を引く形にした。
- Go:`if`、`&&`、`||`、`else if`、3 節の for、range、条件だけの for、`for {}`、switch の case 2 つ、型 switch の case 1 つ、select の case 1 つ、関数リテラルの中の if を持つ関数で、判断点 12、循環的複雑度 13(手で数えた値)。code-db の end-to-end テストに、switch の case 2 つ、`&&`、for の中の if を持つ関数(循環的複雑度 6、入れ子 2)を足した。
- `duckdb-ext/check.sh`、`check.py fixture` は macOS で通った。`check.py` の期待値は、新しい列 `goto_count` を除くと実際の行と完全に一致する項目に限って置き換えた。

## 4. 測ったこと:live の検証の時間

`check.py fixture` は、改名の時点(5adf97a4)で 4 分 37 秒、いまは 8 分 10 秒。時間の大半は、キャッシュを通さない live のクエリで(プロファイルで 470 秒中 426 秒)、`unit_namespace`(手順 2 のファイルの名前空間への回り道)と、それを fan-in・fan-out・coupling から読むようにした手順 3 で、参照のたびに `units` からの導出がやり直されるためと見ている(live の `unit_quality` は 13.8 → 18.2 → 22.1 → 25 秒、`coupling` は 6.5 → 17 秒)。code-db が通るキャッシュの経路は遅くなっていない:Horizon のルート(8,722 units)でキャッシュを作る時間は、改名の時点が 2.2 秒、いまが 1.6 秒。
