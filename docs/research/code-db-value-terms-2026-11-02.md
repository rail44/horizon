# コードデータベース:語彙の値の表示名を語彙ごとに持つ(2026-11-02)

型の事実を言語の層に置いた(`code-db-type-facts-2026-11-02.md`)あと、Rust の利用者には `member_of` の `interface_member`、`implementation` と、参照されない理由の `implementation`、`interface_item` が中立の語のまま見えていた。

## 1. 問題

表示名の対応表(`<language>_terms`)の 1 行は「この語はどこに出てきても置き換える」という意味で、ビュー名、列名、クエリ名、パラメータ名、語彙の列の値のすべてに語の単位で効く(`term_swap`、code-db の `swap_words`)。段の名前(package、namespace)は段の意味でしか使われないので、これで足りていた。Rust に interface → trait の行を足すと、引数と可視性についてのクエリ `interface` まで `trait` になる。同じ綴りで意味の違う語を、綴りだけで区別できない。`try` を足したら `entry_point` が変わった件(`code-db-go-complexity-2026-11-01.md` §2)も、同じ問題の現れだった。

## 2. オーナーの決定(2026-11-02)

置き換えの仕組みのほうを直す。段の名前は今の語単位の置き換えのまま。語彙の値は、言語ごとに語彙ごとの対応表に持ち、その語彙を持つ列の値だけを完全一致で置き換える。

## 3. 形

- 言語ごとの `<language>_value_terms(vocabulary, internal, official)`。語彙は `member_of`(`units.member_of`)と `status`(`unit_reference_status.status`、`unit_quality.reference_status`)。Rust は `member_of` の member → impl、implementation → trait_impl、interface_member → trait と、`status` の implementation → trait_impl、interface_item → trait_item(型の事実より前の表示名に戻る)。Go は行を持たない。Go 自身の語が内部の語だから。
- `views/terms.sql` の `lang_value_terms` が言語の和。`display_column` は語彙の列を、表示言語がその語彙に持つ値の `CASE` で包み、ほかの値は `display_term` に任せる。`member_of` を語彙の列に加え、型の事実の変更で列がなくなった `resolved_by` を除いた。
- ビュー名、列名に入った型の語(`type_interfaces`、`member_implementations`、`interface_member_unit_id`)は言い換えない。
- 入力の向きは変えていない。`member_of` と `status` の値を受け取るパラメータはない。

## 4. 確認

- Horizon のルート(いまの木の索引)の `rust` スキーマ:`member_of` は impl 1,452、trait_impl 320、trait 36、`status` と `reference_status` は trait_impl 187、trait_item 8。`kind` は今までどおり module を含む。
- code-db の end-to-end テストの Rust のフィクスチャに、trait、struct、inherent impl、trait impl を持つ `src/shape.rs` を足した。`rust.units` と `rust.unit_reference_status` が trait / trait_item、trait_impl / trait_impl、impl / unreferenced を出し、クエリ `interface` はその名前のまま動く。Go のテストは `interface_member` のまま通る。
- `duckdb-ext/check.sh`、`check.py fixture` が通った。
