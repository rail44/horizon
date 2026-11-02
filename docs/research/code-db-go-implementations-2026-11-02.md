# コードデータベース:Go の実装関係を SCIP の relationships から読む(2026-11-02)

型の事実を言語の層に置いた(`code-db-type-facts-2026-11-02.md`)時点で、Go は `member` の事実しか出していなかった。Go の型は、インタフェースを実装すると書かない。scip-go は見つけた実装関係を `SymbolInformation.relationships` に書くが、scip_index 拡張はその数(`relationship_count`)しか返していなかった。

## 1. scip-go が書くもの(0.2.7、測ったこと)

Go のフィクスチャ(`Shape` インタフェース、`Circle` 構造体と、その `Area`)で書かれる relationships は 2 つで、どちらも `is_implementation` だけが立つ。

- `…/Circle#` → `…/Shape#`(型からインタフェースへ)
- `…/Circle#Area().` → `…/Shape#Area.`(メソッドからインタフェースのメソッドへ。インタフェースのメソッドの記述子は `Area.` で、`Area().` ではない)

`func (c Circle) String() string` と、別のインタフェース `Named` と構造体 `Dog` を足した複製では、さらに次のものが書かれた。索引の外のインタフェースにも relationships が書かれる。

- `Circle#` → `fmt/Stringer#`、`Circle#String().` → `fmt/Stringer#String().`
- `Dog#` → `Named#`、`Dog#Name().` → `Named#Name.`

インタフェースから実装への逆向きの関係は書かれない。

## 2. 形

- `scip-index-core`:`SymbolRow` の `relationship_count` を `relationships: Vec<Relationship>` に置き換えた(`Relationship` は symbol と 4 つの真偽値:is_reference、is_implementation、is_type_definition、is_definition)。`relationships(path)` は、各 relationship に、それを持つ symbol information の document と symbol を付けた行を返す。
- 拡張:`read_scip_relationships(path)` が `(document, symbol, related_symbol, is_reference, is_implementation, is_type_definition, is_definition, index_path)` を返す。`read_scip_symbols` の `relationship_count` は列のまま残し、その数を返す。`scip-probe` に `relationships` の行の組を足し、`scripts/scip_ext_parity.py` はそれも比べる。
- SQL:`scip_index_relationships` と、文書をリポジトリからの相対パスにして local の symbol を修飾する `scip_relationships`。読むのは、キャッシュを作る間の言語の層の型の事実だけ(`type_fact_units` が写される)なので、写しは作らない。
- `go_type_facts`:`is_implementation` の relationships から、型の行を `implements`(相手は定義を持つインタフェースだけ。Rust の `implements` が解決したものだけを出すのと同じ)、メソッドの行を `implements_member`(索引の外のインタフェースのメソッドなら相手は NULL。Rust の外部 trait と同じ)にする。Go は変換を宣言しないので `converts` はない。
- その結果、インタフェースを実装するだけで直接参照されない Go のメソッドは、`unreferenced` でなく `implementation` になる。

## 3. 確認

- パリティ:`cli_parity.rs` で、手で作った索引(relationship 3 つ:4 つの真偽値と未知のフィールドを含む)と、Go のフィクスチャの scip-go の索引(`SCIP_INDEX` を指定した ignored テスト:occurrence 188、symbol 68、relationship 2)が、`scip print --json` から作った行と一致した。`scip_ext_parity.py` は、Go のフィクスチャと Horizon のルートの索引で、4 つの関数すべてが `scip-probe` の TSV と順序まで一致した(Horizon のルートの relationship は 0 行。rust-analyzer 1.99.1 は書かない)。`scip-index-core` の単体テストと host-check のテストに relationships の検査を足した。
- Go のフィクスチャ:`implements` は Circle → Shape、`implements_member` は Circle.Area → Shape.Area。上の複製では、`Circle.String`(相手は NULL)と `Dog.Name` が `implementation`、`Named.Name` が `interface_item`。
- code-db の end-to-end テスト:Go の `type_interfaces`(Circle → Shape)と `member_implementations`(Circle.Area → Shape.Area)を足した。
- `duckdb-ext/check.sh`(end-to-end テストは scip-go ありで実行)、`check.py fixture` が通った。Rust の層は変わらない。rust-analyzer の索引に relationships はない。
