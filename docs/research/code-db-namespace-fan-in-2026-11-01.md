# コードデータベース:名前空間の段の fan-in と可視性の境界(2026-11-01)

Go のパッケージを名前空間にした(`code-db-go-packages-2026-11-01.md`)次の手順。fan-in と fan-out はファイルと package(Rust の package、Go の module)の 2 つの段しか数えておらず、パッケージや mod をまたいで使われているかは見えなかった。また「公開したものが外で使われているか」を問うクエリ(`public_surface`、`intent`、`interface`、`coupling` の `contract_candidate`)はどれも package の段をまたぐ数を使っており、モジュールが 1 つの Go リポジトリではすべて 0 になっていた。

## 1. オーナーの決定(2026-11-01、3 点とも推奨の形)

- 名前空間の段の数を言語共通で足す。
- 公開されたものの使われ方は、言語ごとの可視性の境界をまたぐ数で見る。Rust の `pub` は crate の外(package の段)、Go の exported な名前はパッケージの外(namespace の段)に開く。言語ごとに 1 行で差し込む。
- Rust の名前空間の参照の読み方(テキストで `crate`、`super`、`self` などを除く判定と、crate の root を `src/lib.rs` に数える規則)を、quality.sql から `lang/rust.sql` に移す。

## 2. 形

- `unit_fan_in` に `fan_in_namespaces`、`unit_fan_out` に `fan_out_namespaces`(参照する側、される側の unit の名前空間 `unit_namespace` が自分と違うものの数)。`unit_quality` にも載せた。
- `coupling` に `level := 'namespace'`:名前空間ごとに、ほかの名前空間からの参照を集計する。loc は名前空間の unit のもの、複数ファイルの名前空間(Go のパッケージ)はファイルの unit の和。列に `namespace` と `fan_in_namespaces`、`fan_out_namespaces` を足した。
- 可視性の境界:`<language>_visibility_boundary`(Rust は package、Go は namespace)、`views/terms.sql` の `visibility_boundary()`(表示の言語のもの、なければ package)。`public_surface` は境界の単位ごとに集計し(Go ではパッケージごと)、列名を段の語を含まない `used_outside`、`pub_unused_outside`、`outside_users` に変え、`namespace` 列を足した(境界が package のときは NULL)。`intent` と `interface` は両方の段の fan-in を出し、境界の段で絞り込みと並べ替えをする。`contract_candidate` のしきい値は境界の段の名前で受け付ける:Rust は `min_packages`、Go は Go のパッケージの数なので内部名 `min_namespaces`(Go の利用者が書くのは表示名の `min_packages`)。
- 名前空間の参照:`<language>_namespace_references(owner_file_id, symbol, file_id, start_line, start_column)`。言語のファイルは units とグラフのビューより先に読まれ、DuckDB はビューも表マクロも作成の時点で参照先を束縛するので、この差し込み口は SCIP の出現、シンボル情報、ソースの行、ファイルだけから作る。quality.sql はこれを名前空間の unit と `unit_occurrences` に結びつける。Go は空のビュー。

言語ごとの差し込み口は、`<language>_units`、`<language>_file_namespaces`、`<language>_terms`、`<language>_visibility_boundary`、`<language>_namespace_references` の 5 つになった。Go の後ろの 2 つは 1〜2 行。

## 3. 確認

- Rust:変更前(4bb93ccc)と後で 20 の層を比べ、変更前の列を名前で選んで比べると、`duckdb-ext/`(252 units)と Horizon のルート(8,718 units)のどちらも差は 0 行(`unit_fan_in` の行も一致:名前空間の参照の判定を移しても同じ)。
- `check.py fixture`:期待値は、新しい列(`fan_in_namespaces`、`fan_out_namespaces`、クエリの `namespace`)を除くと実際の行と完全に一致する項目に限って、実際の行に置き換えた(`unit_quality`、`coupling` 4 つ、`interface`、`intent`、`public_surface`)。`assert_counting_rules` が列の位置で読む箇所を直した。
- Go の fixture:`coupling level=package`(Go の名前空間の段)が `shapes`(1 つのパッケージから使われる)と `cmd/app`(1 つのパッケージを使う)を返し、`public_surface` が Go のパッケージごとに出る。code-db の end-to-end テストに足した。
- `duckdb-ext/check.sh` は macOS で通った。

## 4. 残り

- Rust 固有のまま共通のビューに残るもの:`unit_complexity` のノード型、`unit_reference_status` の `package_root`(symbol が ` crate/` で終わる)。
