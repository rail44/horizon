# コードデータベース:参照されずに動く unit の理由を言語に渡す(2026-11-01)

複雑度を言語ごとにした(`code-db-go-complexity-2026-11-01.md`)あと、共通のビューに残っていた最後の言語固有の規則を移した。`unit_reference_status` は、自分の symbol が参照されない unit に構造上の理由を付ける。そのうち 2 つが Rust の規則だった:`'package_root'`(symbol が ` crate/` で終わるファイルの unit、crate の root)と `'entry_point'`(`main` という名前の関数)。Go では、`package main` のファイルが `unreferenced` に出、`init` も起点にならず、`package_root` は Go の表示で `module_root` になって意味が違った。

## 1. オーナーの決定(2026-11-01)

- 理由は言語ビューの units の列 `entry` として渡す(NULL、`'entry_point'`、`'root'`)。`unit_reference_status` はこの列を読むだけ。言語のファイルは units より前に読まれるが、unit の属性なので `<language>_units` の中で計算できる。
- 状態の値 `'package_root'` は、段の語を含まない `'root'`(ビルドの対象やプログラムの根)に改める。

## 2. 規則

- Rust:`main` という名前の関数が `'entry_point'`、symbol が ` crate/` で終わるファイルの unit が `'root'`(それまでの規則のまま)。
- Go:`package main` の `main` と、すべての `init`(ランタイムが呼ぶ)が `'entry_point'`、`package main` のファイルの unit が `'root'`。

## 3. 確認

- Rust:変更前(54af9ae6)と後で 20 の層を比べ、`package_root` を `root` に対応づけると `duckdb-ext/`(252 units)は差 0 行(units は `entry` 列を除いて一致)。Horizon のルートは下の記録のとおり。
- Go の fixture:`cmd/app/main.go` のファイルの unit が `root`、その `main` と `shapes` の `init` が `entry_point`。code-db の end-to-end テストに足した。
- `check.py` の期待値の `'package_root'` は値の改名なので `'root'` に書き換えた。

共通のビュー(`views/`、`queries/`)には、言語の規則がなくなった。言語ごとの差し込み口は `<language>_units`(`entry` を含む)、`_file_namespaces`、`_terms`、`_visibility_boundary`、`_namespace_references`、`_control_nodes` の 6 つ。
