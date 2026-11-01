# コードデータベース:制御構文の語彙を言語の層に置く(2026-11-02)

複雑度を言語ごとの構文で数えるようにした(`code-db-go-complexity-2026-11-01.md`)時点では、共通の `unit_complexity` が構文ごとの列(`if_count`、`match_count`、`arm_count`、`try_count`、…)を持ち、Go の語は表示の対応表で置き換え(`match` → `switch`)、Go にない構文は対応表の NULL の行で NULL の列にしていた。内部の語彙を中立にする相談の中で、オーナーは「複雑度の構文については言語毎のアダプタ層で持つべきもの」と判断した。

## 1. 形

- 言語の側(`<language>_control_nodes`)が構文に、その言語自身の語で名前を付ける。Rust は if、else_if、match、arm、guard、for、while、loop、try、and、or、let_else、return、break、continue、closure(それまでのまま)、Go は if、else_if、switch、select、case、for、while、loop、and、or、goto、return、break、continue、func_literal。重み、入れ子、選択肢の組はそれまでのまま。
- 共通の側は構文の名前を知らない:`unit_constructs(unit_id, construct, count)`(縦持ち、構文は言語の語)と `unit_complexity(unit_id, decisions, max_nesting)`。`refresh.sql` は `unit_constructs` も写す。
- `complexity` クエリは `unit_constructs` を DuckDB の `PIVOT` で `<構文>_count` の列に展開する。列は結果の unit が持つ構文から作られ、アルファベット順。言語にない構文の列は出ない。`if_count` は else if を含まなくなった(else if は `else_if_count`)。
- 対応表(`<language>_terms`)から構文の行と NULL の行を除いた。対応表は段の名前の 2 行に戻った。
- code-db はクエリの結果を表示名で出すための一時ビューを一時表にした。DuckDB は値をデータから決める `PIVOT` をビューの中で許さない。また `PIVOT` は同じクエリの CTE を参照できないので、クエリは絞り込みの条件を `PIVOT` の中で繰り返す。

## 2. 確認

- Rust:Horizon のルート(いまの木の索引、8,722 units)で、変更前の `unit_complexity` の構文ごとの列、`decisions`、`max_nesting` が、新しい `unit_complexity` と `unit_constructs` から作り直した値(旧 `if_count` は if と else_if の和)とすべての unit で一致。ほかの層は差 0 行。
- `check.py`:`unit_complexity`(flow の fixture)と `complexity` クエリの期待値は、古い期待値から機械的に作り直し(構文の一覧と、旧の列から求めた数)、それが実際の出力と一致することで確かめた。`check.py fixture` は通った。
- Go:fixture の `complexity` は `switch_count`、`case_count`、`select_count`、`func_literal_count`、`goto_count` を並べ、Rust の構文の列を出さない。code-db の end-to-end テストで確かめる。
