# コードデータベース:型の事実を言語の層に置く(2026-11-02)

制御構文の語彙を言語の層に移した(`code-db-constructs-2026-11-02.md`)次の手順。共通の `units` は Rust の impl ブロックの列(`impl_key`、`impl_type`、`impl_trait`、`impl_header_line`)と Rust の語の `member_of`(`impl`、`trait_impl`、`trait`)を持ち、`views/graph.sql` の `impls`、`impl_targets`、`impl_for`、`implements`、`unit_impl_type` がそれを読んで、型の名前の解決を共通の側でしていた。Go は受け手の型の symbol を `impl_key` に入れて、この形に合わせていた。

## 1. オーナーの決定(2026-11-02、推奨の形)

- 共通の側は中立の事実だけを持つ。言語の側がそれを導き、SCIP の定義の出現(file_id、symbol、name_line、name_column)を鍵にする。言語のファイルは `units` より先に読まれるので、unit_id は使えない。
- 共通の側から impl ブロックを除く。変換(`From` など)も言語の層に置く。
- Rust を先にする。Go のインタフェースの事実は、scip の relationships を拡張で読めるようにしてから。

## 2. 形

- `<language>_type_facts(fact, file_id, symbol, name_line, name_column, to_file_id, to_symbol, to_name_line, to_name_column, to_name)`。事実は 4 種:`member`(メンバーとその型)、`implements`(型と、それが実装するインタフェース)、`implements_member`(メンバーと、それが実装するインタフェースのメンバー)、`converts`(型と、それが変換する相手の型の名前)。
- Rust:それまでの `rust_units` を内部の `rust_unit_rows` とし、impl の列はそこに残す。`rust_units` は impl の列を除き、`member_of` を `member`(inherent impl)、`implementation`(trait impl)、`interface_member`(trait 自身の項目)に置き換える。`rust_type_facts` は `impl_targets` の解決(見出し行の参照 → ファイル → パッケージ → リポジトリ)をそのまま移す。`implements_member` は、trait の symbol にメンバー自身の最後の記述子を足した symbol の trait 項目。
- Go:`go_units` から impl の列を除き、`member_of` は `member`、`interface_member`。`go_type_facts` はメソッドの `member`(symbol の最後の記述子を除いた型)だけ。使われていなかった `receiver_symbol` を除いた。
- 共通の側(`views/graph.sql`):`type_facts`(言語の和)、`type_fact_units(fact, unit_id, to_unit_id, to_name)`、`unit_owner_types(unit_id, type_unit_id)`(最も近い祖先か自身のメンバーの型)、`type_interfaces`、`member_implementations(unit_id, interface_member_unit_id)`、`type_conversions(type_unit_id, to_name)`。`refresh.sql` は `impls`、`impl_targets`、`unit_impl_type` に代えて `type_fact_units`、`unit_owner_types` を写す。
- 状態の名前:`unit_reference_status` の `trait_impl` は `implementation`(`member_implementations` にある unit)、`trait_item` は `interface_item`(`member_of` が `interface_member`)。`unit_scope`、`namespace_edges` の `same_type_unit_edges`、`duplication` の `conversion_pair` は新しいビューから読む。

## 3. 確認

- 変更前(7eb86e96)の SQL と比べた。Horizon のルート(いまの木の索引、8,722 units):旧 `unit_impl_type` と新 `unit_owner_types`(1,770 行)、旧 `impl_for`×`implements` と新 `type_interfaces`(42 行)、旧の `From`/`Into`/`TryFrom`/`TryInto` の impl と新 `type_conversions`(22 行)が一致。`units`(旧の列を除き `member_of` を置き換えたもの)、`unit_reference_status`(状態の名前を置き換えたもの)、`unit_scope`、`unit_fan_in`、`unit_fan_out`、`namespace_edges`、`unit_quality` も差 0 行。`duckdb-ext/`(252 units)も同じく差 0 行(81 / 3 / 1 行)。
- Horizon のルートで、`member` の 1,772 行のうち 1,763 行で型が解決し、trait impl のメンバー 320 のうち 84 が trait の項目に届く(残りは索引の外の trait か、解決しなかった trait)。
- `check.py`:`impl_targets` の検査を `unit_owner_types`、`type_interfaces`、`member_implementations`、`type_conversions` の検査に置き換えた。期待値は古い期待値から作り、実際の出力と一致することで確かめた。`check.py fixture`、`duckdb-ext/check.sh`、code-db の end-to-end テスト(Go の `unit_owner_types`:`Circle.Area`、`Circle.scale` → `Circle`)が通った。

## 4. 残したこと

- Rust の表示名:Rust の利用者には `interface_member`、`implementation`、`interface_item` が中立の語のまま見える。`rust_terms` に interface → trait の行を足すと、引数についてのクエリ `interface` の名前まで `trait` になるので、行を足していない。
- Go の `implements`、`implements_member`:scip-go は relationships(`Circle#` → `Shape#` の実装、`Circle#Area().` → `Shape#Area().`)を書くが、scip_index 拡張はまだ読まない。
