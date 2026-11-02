# コードデータベース:go.work と cgo(2026-11-02)

Go の実装関係(`code-db-go-implementations-2026-11-02.md`)の次の手順。code-db は tracked の go.mod ごとに scip-go(0.2.7)を走らせており、go.work を読んでいなかった。cgo のファイルの扱いも確かめていなかった。

## 1. 測ったこと

### go.work

フィクスチャは、`a/`(`geom.Area` と `Shape` インタフェース)と、それを使う `b/`(`Box` が `Shape` を実装する)を go.work でまとめたもの。モジュールごとに索引すると、同じ定義を指す symbol が索引ごとに違う。

| | a 自身の索引(定義) | b の索引(参照) |
| --- | --- | --- |
| version | `2b52c5bb5c21`(HEAD の短いハッシュ、scip-go の `--module-version` の既定) | `.` |
| インタフェースのメソッド | `Shape#Size.` | `Shape#Size().` |

そのため、b から a への参照は 1 つも unit の辺にならなかった。go.work のディレクトリで `scip-go index ./a/... ./b/...` と一度に索引すると、両方が `2b52c5bb5c21` と `Shape#Size.` に揃い、`Box#` → `Shape#` の relationships も a の定義の symbol を指した(go.mod がないという警告は出るが、索引はできる)。

go.work を使わず、b の go.mod の `replace example.com/a => ../a` で繋ぐと、version は揃うが、インタフェースのメソッドは `Size().` のまま食い違う。

### cgo

`import "C"` を持つ `num/num.go` と、その関数を呼ぶ `num/plain.go` で試した。

- CGO_ENABLED=1(既定):scip-go は `num/num.go` ではなく、Go のビルドキャッシュにある cgo の生成ファイル(`../../…/Library/Caches/go-build/53/…-d` など 3 つ)を索引する。どのファイルにも一致しないので、num.go の units は 0 で、plain.go からの参照も unit に届かない。生成ファイルの一つ(`num.cgo1.go` にあたるもの)は `//line <元の絶対パス>:1:1` で始まり、定義の行は元のファイルの行と一致した(Twice 13 行目、Free 21 行目)。違うのは文書のパスだけ。
- CGO_ENABLED=0:cgo のファイルはビルドから外れ、索引に出ない。

## 2. オーナーの決定(2026-11-02)

- go.work:tracked の go.work が `use` するモジュールは、go.work のディレクトリで一度に索引する(推奨の形)。`replace` で繋いだ場合の食い違いは制限として書く。
- cgo:制限として書く(生成ファイルから元のファイルへの対応づけ、`CGO_ENABLED=0` で走らせる案は採らない)。

## 3. 形

- `code-db index` は tracked の go.work も読み、`use` の指示(1 行の形、`use ( … )` の塊、引用符、`//` のコメント)からディレクトリを求める。go.work が使うモジュールは、その go.work ごとに 1 回の scip-go(作業ディレクトリは go.work のディレクトリ、`GOWORK` はその go.work、パターンはモジュールごとの `./<module>/...`)で索引する。一つのモジュールを複数の go.work が使うときは、最も深い go.work を選ぶ。索引の名前は、ルートの go.work が `gowork.scip`、それ以外が `gowork-<path の / を +>.scip`。
- ほかのモジュールは今までどおりモジュールごとに索引し、`GOWORK=off` にする。go.work が使わないモジュールを go.work の下で索引すると go コマンドが失敗するのを避けるためと、tracked でない手元の go.work を読まないため。

## 4. 確認

- フィクスチャ(上の a、b に、go.work が使わないモジュール c を足したもの):索引は `gowork.scip` と `go-c.scip`。b の main から a の Area、Shape、Shape.Size への unit の辺、`Box.Size` → `Shape.Size` の `member_implementations`、`Box.Size` の状態 `implementation` が出た。c は単独で索引され、その module は `example.com/c`。
- code-db の単体テストに go.work の解析(1 行の形、塊、引用符、コメント、`use` で始まる別の語)と、モジュールの振り分け(ルートと入れ子の go.work、go.work の外のモジュール、リポジトリの外を指す `use`)を足した。end-to-end テストに、上のフィクスチャの go.work のテストを足した。
- `duckdb-ext/check.sh` が通った(end-to-end テストは scip-go ありで実行)。SQL は変えていない。
