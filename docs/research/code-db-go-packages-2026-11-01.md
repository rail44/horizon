# コードデータベース:Go のパッケージを名前空間にする(2026-11-01)

段の名前を決めた(`code-db-terms-2026-10-30.md`)次の手順。Go の名前空間(内部名 namespace、Go の表示では package)は、それまでファイル単位だった:`namespaces` が `kind = 'namespace'` の unit 1 つにつき 1 行を作り、名前と `namespace_path` を graph.sql に直に書かれた Rust の規則(`lib.rs`/`main.rs`/`mod.rs` はディレクトリ、ほかはパスから `.rs` を除く)で決めていたためで、複数のファイルにまたがる Go のパッケージを 1 つにできなかった。パッケージへの参照も、パッケージの symbol が全ファイルで同じなので、ファイルの unit それぞれに丸ごと数えられていた。

## 1. オーナーの決定(2026-11-01、3 点とも推奨の形)

- Go の名前空間の名前はディレクトリのパス(ルートは `.`)。import パスではない:Rust の名前空間もファイルのパスで、設定ファイルの層のパターン(`internal/**`)もパスで書ける。
- 外部テストパッケージ(同じディレクトリの `package x_test`)は同じディレクトリの名前空間に含める。
- `namespace_path` もディレクトリで、ディレクトリのパッケージは下のパッケージの祖先になる(`nested_namespaces`)。Go のパッケージに意味上の入れ子はないが、ディレクトリの入れ子(`internal/`)は構造として読まれる。Go はパッケージの循環を禁じるので、循環の判定への影響はない。

## 2. 形

- 言語ごとの差し込み口を 1 つ足した:`<language>_file_namespaces(file_id, namespace, namespace_path)`、各ファイルが属する名前空間。Rust は graph.sql にあった規則をそのまま `lang/rust.sql` に移し(ファイルが自分の名前空間、ファイルのモジュール)、Go は `lang/go.sql` でディレクトリにした。graph.sql は `file_namespaces` として両者の和をとり、言語を知らない。
- `namespaces`:ファイルの unit があるファイルの名前空間を名前で束ね(Go のパッケージは複数ファイルで 1 行)、ファイルの中の名前空間の unit(Rust の `mod x { … }`)を加える。`file_id` と `namespace_unit_id` は、1 ファイルの名前空間ならそのファイルとファイルの unit、複数ファイルなら NULL。
- `unit_namespace`:いちばん近い祖先の名前空間の unit、なければファイルの名前空間。
- Go のファイルの unit は kind `file`(新しい中立の種別)。名前空間ではないので、`'namespace'` と同じく、`symbol_units` で自分の symbol(パッケージの symbol)を持たず、`unit_fan_in` で中身を合算せず、`size` の unit の段に入らない。パッケージへの参照はどのファイルの unit にも数えない(手順 3 で名前空間の段の fan-in を足す)。

## 3. 確認

- Rust:変更前(5adf97a4)と後で 20 の層を Parquet に書いて比べ、`duckdb-ext/`(252 units)と Horizon のルート(8,718 units)のどちらも差は 0 行(規則を移しただけで、Rust の値は改名もない)。
- Go の fixture:名前空間が `cmd/app`(1 ファイル)と `shapes`(3 ファイル、`file_id` NULL)の 2 つになり、名前空間の辺は `cmd/app` → `shapes` の 1 本だけ(`shapes_test.go` → `shapes.go` は同じパッケージの中になった)。ファイルの unit に fan-in の行はない。code-db の end-to-end テストに、`package_graph` がこの 1 本だけを返すことを足した。
- `duckdb-ext/check.sh`、`check.py fixture` は macOS で通った。

## 4. 残り

- 名前空間の段の fan-in(パッケージ、mod をまたいで使われているか)は手順 3。
- `unit_fan_in` がテキストから読む Rust の名前空間の参照(`crate`、`super`、`self`、`src/lib.rs` の root)は、まだ quality.sql にある。
