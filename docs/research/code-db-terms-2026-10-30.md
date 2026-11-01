# コードデータベース:段の名前と言語ごとの表示(2026-10-30)

Go を足した(`code-db-go-2026-10-30.md`)あと、ファイルより上の 2 つの段の名前を決め直した。それまでの名前 `crate` と `module` は Rust の語で、Go で使うときに `crate` という名前が出てくることはありえない(オーナー)。

## 1. 段の性質

| 性質 | Rust | Go |
|---|---|---|
| マニフェストが宣言し、版を持ち、依存を書く単位 | package(`Cargo.toml`) | module(`go.mod`) |
| 名前空間・可視性の単位 | module(`mod`) | package(ディレクトリ) |
| コンパイルの単位 | crate | package |
| 循環を言語が禁じる | crate 間 | package 間 |

Go の package は、名前空間と可視性では Rust の module に、コンパイルの単位と循環の禁止では Rust の crate に当たる(オーナーの指摘「go でいう package って rust でいう mod では?」から整理した)。units の `crate` 列の中身は、Rust でも rust-analyzer が symbol に付ける Cargo の package だった。外側の段はマニフェストの単位(Cargo package と Go module)、内側の段は名前空間の単位(Rust module と Go package)とした。

## 2. 名前の決め方

比べたのは、(A) 中立の名前を 1 つ持つ、(B) 言語ごとの正式名で引けるようにする、の 2 つ。どちらの言語の正式名を中立の名前にしても、Go では `package` と `module` が逆の段を指すので、Go の読み手には知っている語が逆の意味で出てくる。オーナーは B を選んだ。

B でもクエリを言語ごとに書かないために、クエリの中で段を指す識別子(内部名)が要る。言語ごとに必要なのは、内部名から正式名への 2 行の対応表だけで、残りは一度だけ作る表示の層になる(クエリを言語ごとに書くと、言語ごとに約 630 行の複製になる)。混在リポジトリは言語ごとに引く(SCIP の索引は言語ごとで、言語をまたぐ参照はない)ので、内部名は利用者に見えない。

内部名の候補として、似た単位をほかのプロジェクトがどう呼んでいるかを調べた。

| プロジェクト | 外側 | 内側 |
|---|---|---|
| SCIP(このデータベースの入力) | `Package`:"Unit of packaging and distribution"、Go と JVM の module に当たると明記 | `Namespace`:"Unit of code abstraction and/or namespacing"、Go と JVM の package に当たると明記 |
| CodeQL(Go) | `GoModModuleLine` | `Package` |
| jQAssistant(Java) | `Artifact` | `Package` |
| CycloneDX | `component`(SBOM の要素) | — |
| C4 model | (Container が配備の単位) | component はコンテナの中の機能のまとまり |

`component` は C4 の意味(内側寄り)と衝突する。内部名は利用者に見えないので Go の package との衝突は `lang/go.sql` の中での読み替えにとどまる(オーナーの指摘)ことから、入力の SCIP と同じ `package` と `namespace` にした。

## 3. 実装

- 改名:`crate` → `package`、段の意味の `module` → `namespace`、その派生(`fan_in_packages`、`package_edges`、`package_cycles`、`min_packages`、`namespace_edges`、`namespace_cycles`、`namespace_graph`、`unit_namespace`、`namespace_path`、`nested_namespaces`、`namespace_layers` など)、値(`level := 'package'`、kind `'namespace'`、status `'package_root'`、resolved_by `'package'`)。`is_file_module` は Go のファイルが名前空間ではないので、段の語を含まない `is_file_unit` にした。Rust の構文(`crate::`、`pub(crate)`、`' crate/'`、`mod x;`)、SCIP の種別 `Module`、sitting_duck の `scope_module` はそのまま。クエリのファイル名は `namespace_graph.sql`、`namespace_cycles.sql`。
- 対応表:`lang/rust.sql` の `rust_terms`(package → package、namespace → module)、`lang/go.sql` の `go_terms`(package → module、namespace → package)。
- 表示の層(`views/terms.sql`):表示の言語(`lang`、なければ `files` の唯一の言語)、2 語を同時に置き換えるマクロ(Go は入れ替えになるので順に置き換えると二重に変わる)、語彙の列(`kind`、`level`、`status`、`reference_status`、`resolved_by`、`unit_kind`)の値の置き換え、言語の名前のスキーマに全ビューを表示名で作る DDL(`go.units`、`go.package_edges`)、クエリの結果を表示名で出す SELECT。
- code-db:`views/terms.sql` までを軽く読んで表示の言語を決め、複数言語で `lang` がなければ止める。クエリ名・変数名・語彙の値を内部名に直し、結果を表示名で出し、スキーマを作る。設定ファイルの `[[layers]]` は `modules`(Rust)と `packages`(Go)のどちらも受け付ける。
- 途中の不具合:`duckdb_columns()` が ATTACH したキャッシュの同名の表の列も返し、表示のビューの列が二重になった。データベースで絞った。

## 4. 確認

- Rust の結果:改名の前後で 20 の層(units から unit_quality まで)を Parquet に書き、列の位置で比べた。`duckdb-ext/`(252 units)と Horizon のルート(8,718 units)のどちらも、改名した値(kind、status、resolved_by)を対応づけると差は 0 行。
- code-db の end-to-end テスト:Rust は `rust.units` を引き、`level=package` で size を出す。Go は `go.units` を Go の名前で引き、`package_graph` の列名に `namespace` も `crate` も出ないこと、`coupling level=module` が Go module の段を返すこと、Rust のファイルを足すと `lang=` を求めて止まり、`lang=go` で Go だけを読むことを確かめる。
- `duckdb-ext/check.sh` と `check.py fixture` は macOS で通った。`check.py` の fixture は改名の前から macOS で落ちていた:`connect` が `repo` を正規化したパス(`/private/var/…`)で設定するのに、fixture が書く jscpd の報告は正規化前のパス(`/var/…`)で、クローンがどのファイルにも結びつかなかった(変更前の HEAD でも同じ箇所で落ちることを確かめた)。報告も正規化したパスで書くようにした。

## 5. 残り

- DuckDB の CLI が出すエラー(`FORMAT` が markdown 以外、`shell`)は内部名のまま。
- Go のファイルの unit は、まだ kind が `namespace`(表示は `package`)で、名前空間はファイル単位。Go の package(ディレクトリ)に束ねるのが次の手順。
