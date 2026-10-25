# SCIP の参照の事実をビューにし、fan-in × churn を移した記録(2026-10-25)

コードデータベースの SQL 層の 5 つ目の増分である。rust-analyzer の SCIP 索引から、どの出現(occurrence)がどの記号をどこで指すかをビューにし、その上にスパイクのクエリ `unit_fan_in_churn` を移し、2 つのキャッシュに SCIP の層を足した。観察だけを記し、推奨は書かない。

設計文書の未決点は、SCIP の読み手の形だった。候補は、Sourcegraph の `scip` CLI の `scip print --json` の出力をコアの `read_json` で読む形(案 1)と、小さな Rust の表関数 `read_scip`(案 2)である。オーナーは 2026-10-25 に「わりと軽微もしくは検証可能なものが多く見えるので、表関数で試しに作ってみましょうか」と答え、案 2 になった。表関数 `read_scip`・`read_scip_symbols`・`read_scip_metadata` の列は依頼の時点で決まっており、別の worker が `duckdb-ext/` の新しい拡張 `scip_index` として並行して実装した(306df6a4)。この作業は、関数が入るまで案 1 を代役として組み(1 節)、代役の測定を案 1 のデータとして取り、関数が入った後に代役を外して、すべての検査と測定をやり直した。本記録の数値は、断りのない限り関数での値である。

前提は [設計文書](../code-database-design.md) と、前の記録 [code-db-unit-changes-2026-10-25.md](code-db-unit-changes-2026-10-25.md)(以下「前回の記録」)である。意味の基準は [スパイクの記録](code-db-spike-2026-10-24.md) と、スパイクのクエリ(`git show 475599d5:scripts/code-db/queries/unit_fan_in_churn.sql`)・ローダー(同 `build.py` の `load_scip`)である。作業は worker の作業ツリーで `origin/code-quality-survey` の 20941942 から始め、関数の入った 306df6a4 に進めた。機械は前回と同じ AMD Ryzen AI 9 HX 370(24 スレッド)、メモリ 62 GB で、オーナーの対話用デスクトップを他の作業と共有している。測定の間の load average は 4〜24 だった。DuckDB は Python の `duckdb==2.0.0` と CLI 2.0.0、sitting_duck は v1.16.0、git_history と scip_index はこの作業ツリーで `duckdb-ext/build.sh` で作ったもの、rust-analyzer は 1.99.1(58cf8f9 2026-10-13)、代役の `scip` CLI は v0.10.0 である。Rust は変えていない。

## 主要な観察

- **`unit_fan_in_churn` の 2,915 行は、同じ木と同じ索引で動かしたスパイクのコードの 2,915 行と、2 行の commits を除いて一致した。** 2 行(`ToolCallRequest` の impl と `ToolCallRequest.identity`)は、git と gix の `-U0` の配置の違いとして gix の記録 §12 と前回の記録が挙げた 901edb40f6 の帰属である。上位はスパイクの記録の表と同じ値だった(`SessionId` 107 ファイル・979 参照・1 コミット、`Event` 89・3,079・21、…)。
- **代役(案 1)でも組めた。** 30.4 MB の索引の JSON は 59.4 MB、変換は 0.3〜0.4 s、`read_json` での 3 つの行の集合の読み込みは各 0.27〜0.47 s、ピーク RSS 418〜471 MiB(ビューのファイルを読んだだけのプロセスは 248〜251 MiB)だった。止めて報告する条件(時間やメモリが妥当でない)には当たらなかった。
- **関数では、同じ 3 つの読み込みが 0.01〜0.09 s、ピーク RSS 240〜260 MiB になった。** 形を整えたビューの行は、代役と関数で両方向とも 0 行の差だった(記号の `kind` は代役が数値、関数が名前なので除き、空の文字列と空のリストは NULL と見なして比べた)。
- **`views/scip.sql` は省いてよいファイルにした。** 索引が無いとき、このファイルだけが失敗し、他のビューと `cache.sql`・`refresh.sql` はそのまま動く(SCIP のビューは空になる)。
- **永続キャッシュでは、SCIP の層は索引の内容のハッシュを鍵に丸ごと作り直す。** 30 MB の `hash()` は約 20 ms だった。作り直しは 0.32〜0.68 s、鍵が同じときは 0.01 s で、索引は読まれない。キャッシュファイルは 628.3 MiB(前回 616.3 MiB)になった。
- **キャッシュの上の `unit_fan_in_churn` は 0.12〜0.15 s、ライブでは 15.05〜15.50 s・ピーク RSS 8.6〜8.7 GB だった。** ライブの時間のほとんどは `unit_changes` の blob の解析で、SCIP のビューのライブの読み込みは 0.1 s 未満である。

## 1. 組んだもの

| ファイル | 変更 |
| --- | --- |
| `views/scip.sql`(新規、省いてよい) | 元の行 `scip_index_occurrences`・`scip_index_symbols`・`scip_index_metadata`、形を整えた行 `scip_document_occurrences`・`scip_document_symbols`、クエリが読む `scip_occurrences`・`scip_symbols` |
| `queries/unit_fan_in_churn.sql`(新規) | スパイクのクエリを 1 始まりの行とビューに移した |
| `00_load.sql` | `scip_index` 拡張の `LOAD`、変数 `scip_index` の既定値(`<repo>/target/code-db/index.scip`) |
| `cache.sql`・`refresh.sql` | SCIP の層(4 節) |
| `check.py` | SCIP の fixture、更新の fixture の SCIP の段階、各コマンドの SCIP の項目 |
| `README.md` | SCIP の節、読む順、キャッシュ、検査、配置、パラメータ |

読む順は前回のままで、最後に `views/scip.sql` を任意で読む。索引は `rust-analyzer scip . --output target/code-db/index.scip` で作る。この作業ツリーでは 276.7 s・ピーク RSS 7.0 GB だった(build script の実行を含み、並行して別のビルドが走っていた。スパイクの記録は 159 s)。

### 3 層のビュー

- **元の行**:3 つの関数の列そのままである(0 始まりの行と文字、`local N` は修飾しない)。関数への切り替えは、この 3 つのビューの本体を替えるだけだった。
- **形を整えた行**(`scip_document_*`):行を 1 始まりにし、文字を 1 始まりの列にした(開始は `start_char + 1`、終わりは最後の文字の次の列で、`nodes` の `end_column` と同じ規約)。`local N` は文書の中でだけ一意なので `local N <document>` に修飾した(出現の記号、記号の `symbol` と `enclosing_symbol`)。`is_reference` は `NOT is_definition` で、スパイクの定義と同じく Definition の役割の無い出現すべてである。記号の無い出現は落とした(この repo の索引には無い)。型はここで固定した。キャッシュの表がソースの型に左右されないためである。
- **クエリが読む行**:`scip_occurrences` は `document` を `files.path` に結んだ `file_id` を足したもので、ソースのファイルでない文書では NULL になる。`scip_symbols` は形を整えた行そのものである。`cache.sql` と `refresh.sql` が差し替えるのはこの 2 つだけで、形を整えた行のビューは差し替えない。差し替えたビューから作り直すと循環するためである。

列の文字を 1 始まりの列に直すことは依頼に無かったが、README の規約(列は 1 始まり)に合わせた。単位は文書の位置の符号化で、rust-analyzer では UTF-8 のバイトである。`file_id` は内部結合でなく外部結合にした。スパイクのローダーは `files` に無い文書の出現を落としていたが、ここでは行を残し、クエリの側で `file_id IS NOT NULL` に絞る。この repo では 509 文書すべてがソースのファイルで、ソースのファイル 526 個のうち 17 個(`duckdb-ext/` など、ルートの cargo ワークスペースの外)は索引に無い。

記号の修飾をマクロにしなかったのは、`refresh.sql` の導出のキー(`derive_key`)が、内部でないすべてのマクロの本文を含むためである。マクロを `views/scip.sql` に置くと、このファイルを読むかどうかで全 unit の導き直しが起きる。代役の `read_json` も同じ理由で、表マクロでなくビューにした。

rust-analyzer は、パッケージの `crate/` 記号を、各ターゲットの根のファイル(fixture では `src/lib.rs` と `tests/it.rs`)の下に挙げる。`scip_symbols` は挙げられた分だけ行を持つ。この repo では、文書の記号 42,830 行と外部記号 9 行の計 42,839 行で、異なる記号は 42,808 個だった。42,808 はスパイクの記録の `scip_symbols` の行数と同じである(スパイクは記号ごとに最初の 1 行だけを残していた)。

### 省いてよいファイルにした理由と、その仕組み

関数は束縛のときにファイルを開き、無ければ誤りになる(代役の `read_json` も同じだった)。`views/scip.sql` を必須にすると、索引の無い利用者は層全体を使えなくなる。そこで任意のファイルにし、最後に読む。

`cache.sql` と `refresh.sql` は 1 つのファイルのまま、`views/scip.sql` を読んだかどうかに関わらず動く必要がある。SQL には条件つきの文が無いので、`CREATE VIEW IF NOT EXISTS` を使った。`views/scip.sql` が読まれていないとき、両ファイルは `scip_document_occurrences`・`scip_document_symbols` を同じ列の空のビューとして作る。読まれていれば何もしない。空のビューは、後で `views/scip.sql` が読まれると置き換わる。読まれたかどうかは、`views/scip.sql` だけが作る `scip_index_occurrences` があるかで判定する。

## 2. `unit_fan_in_churn`

定義はスパイクのクエリのままで、行を 1 始まりにした。unit の fan-in は、その unit のファイル以外で、定義の出現が unit の行の範囲 `[start_line, end_line]` に収まる記号のどれかへの参照の出現を持つ、異なるファイルの数である。ファイル全体にわたるモジュールの定義(rust-analyzer は `crate/` やモジュールの記号の定義を `[0, 0, 14, 0]` のようにファイル全体の範囲で出す)は、どの unit にも収まらない。囲む unit(impl、モジュール)は中身すべての記号を含む。churn は `unit_changes` の `(path, kind, qualified_name)` のコミット数である。パラメータは `kind`、`min_fan_in`(既定 1)、`include_merges` で、スパイクの `$` パラメータを DuckDB の変数にした。見出しの問い、パラメータ、Mo et al. 2021 の根拠の行は残した。

`units` と `scip_occurrences` はそれぞれ `MATERIALIZED` の共通表式で 1 回だけ読む。`check.py explain` では、ライブで `READ_AST` 2(今のファイルと blob)、`READ_SCIP` 1 だった。

### スパイクとの比較

スパイクの `build.py`(475599d5 から `target/code-db/spike` に展開)を、throwaway の clone(20941942)と同じ索引で動かし、`unit_fan_in_churn` の全行をこの層の行と比べた(`target/code-db/scratch/compare_fan_in.py`、コミットしていない)。

| | 行数 | 片側だけの unit | 値の違う行 |
| --- | --- | --- | --- |
| この層とスパイクのコード(同じ木、同じ索引) | 2,915 / 2,915 | 0 / 0 | 2 行の commits(スパイク 2、この層 1) |

違う 2 行は `crates/horizon-agent/src/contract.rs` の `impl ToolCallRequest` と `ToolCallRequest.identity` である。スパイクのコードは git CLI の hunk を読み、この層は gix の hunk を読む。この 2 つは、gix の記録 §12 が git 側の誤った帰属として挙げ、前回の記録が「スパイクにだけある 7 行」に数えた 901edb40f6 の行である。fan-in と参照の数は全行で一致した。

上位はスパイクの記録の表と同じだった。

| unit | fan-in ファイル数 | 参照 occurrence | commits | スパイクの記録 |
| --- | --- | --- | --- | --- |
| `horizon-wire/src/session.rs` `SessionId`(type) | 107 | 979 | 1 | 107 / 979 / 1 |
| `horizon-agent/src/contract.rs` `Event`(type) | 89 | 3,079 | 21 | 89 / 3,079 / 21 |
| `SessionId`(impl) | 78 | 569 | 1 | 78 / 569 / 1 |
| `contract.rs` `ToolCallId` | 68 | 425 | 1 | 68 / 425 / 1 |
| `SessionId.new`(method) | 65 | 470 | 1 | (記録に無い) |
| `contract.rs` `ToolCallRequest` | 64 | 1,103 | 6 | 64 / 1,103 / 6 |

依頼は、新しいコミットによる差と、改名されたファイルの履歴の扱いの差を説明するよう求めていた。差は出なかった。スパイクの記録の木(4ad82864)から 20941942 までの 19 コミットで変わった `.rs` は `duckdb-ext/` の 11 ファイルだけで、`duckdb-ext/` はルートのワークスペースの外なので索引に無い。出現の数もスパイクの記録と同じ 301,687 だった。改名については、この層もスパイクも `(path, kind, qualified_name)` で履歴を数え、改名をたどらない点が同じで、`SessionId` が 1 コミットなのはスパイクの記録の観察(`horizon-wire` への切り出しで履歴が切れた)のとおりである。

## 3. 代役(案 1)と関数(案 2)

### 代役

関数が入るまで、`views/scip.sql` の元の行の 3 つのビューを、`scip print --json` の出力の上に組んだ。CLI は Sourcegraph の `scip` v0.10.0(2026-09-03 公開)の `scip-linux-amd64.tar.gz` を `target/code-db/tools/scip-v0.10.0/` に置いた。tarball の sha256 は `eeb28ebbff443c01609fb1691809958f3f0c7a8651af689a5bc8560061cd8ab0`(リリースの `.sha256` と一致)、バイナリ `scip` は `7af5dbd13ba1d447457bb735d3f43372a067950a51b5234d2465f7a57a0d8eb2` である。

この CLI の `--json` は protobuf の JSON 対応(camelCase、列挙は名前)ではなく、Go の構造体の `encoding/json` の出力だった。名前は snake_case(`relative_path`、`symbol_roles`、`enclosing_range`)、列挙は数値、空の欄は省かれ、`"TypedRange": null` のような欄も出る。スパイクの `scip.py` の JSON の読み手は camelCase を前提にしており、この出力は読めない(スパイクは protobuf を直接読んでいたので、影響は無かった)。代役は、索引全体を 1 つの JSON オブジェクトとして `read_json(format := 'unstructured', maximum_object_size := 2147483647, columns := {...})` で、列の型を明示して読み、`documents` と各文書の `occurrences`・`symbols` を `unnest` した。範囲は要素の数(3 か 4)で分けた。

代役の測定(throwaway の clone、各回は新しいプロセス、3 回の最小〜最大):

| 項目 | 値 |
| --- | --- |
| 索引(protobuf) | 30,385,713 バイト、509 文書、出現 301,687、外部記号 9 |
| `scip print --json` の出力 | 59,372,955 バイト |
| 変換の時間 | 0.3〜0.4 s、ピーク RSS 280〜303 MiB |
| `count(*) FROM scip_index_occurrences` | 0.27〜0.34 s、ピーク RSS 418〜423 MiB |
| `count(*) FROM scip_index_symbols` | 0.31〜0.40 s、ピーク RSS 467〜471 MiB |
| `SELECT * FROM scip_index_metadata` | 0.38〜0.47 s、ピーク RSS 421〜424 MiB |
| `count(*) FROM scip_occurrences` / `scip_symbols` | 0.25〜0.27 s / 0.25〜0.41 s |
| ビューのファイルを読んだだけのプロセス | ピーク RSS 248〜251 MiB |
| `refresh.sql` の SCIP の層の作り直し(初回) | 1.30 s |

JSON 全体を 1 つの表に読むだけなら、CLI 2.0.0 で 0.46 s だった。`scip_index_symbols` は文書の記号と外部記号の 2 か所を読むので、JSON の読み込みを `MATERIALIZED` の共通表式 1 つにした。

### 関数への切り替え

306df6a4 を取り込み、`duckdb-ext/build.sh` で 2 つの拡張を作り直した。`00_load.sql` に 2 つ目の `LOAD` を足し、元の行の 3 つのビューを `read_scip(getvariable('scip_index'))` などに替え、代役の JSON の読み込みと `check.py` の変換の手順を消した。関数は `getvariable` を引数に受け付けた。

関数の列は依頼で決まっていたとおりで、食い違いは無かった。代役との違いは、決まっていなかった細部だけだった。`kind` と `text_document_encoding` は列挙の名前(`Function`、`UTF8`)で、代役は数値だった。無い文字列は `''`、無い `documentation` は空のリストで、代役は NULL だった。`version` は 0、`tool_arguments` は空のリストだった。`scip_document_symbols` の `kind` は、代役のあいだ INTEGER に固定していたのを VARCHAR に替え、キャッシュの表も合わせた。

形を整えた行を代役と関数で比べた(`target/code-db/scratch/compare_standin.py`、コミットしていない)。出現 301,687 行は全列で両方向に 0 行の差、記号 42,839 行は `kind` を除き、`''` と空のリストを NULL と見なすと両方向に 0 行の差だった。

関数での測定(同じ条件):

| 文 | ライブ | ピーク RSS | `cache.sql` の上 |
| --- | --- | --- | --- |
| `count(*) FROM scip_index_occurrences` | 0.06〜0.09 s | 251〜259 MiB | 0.08〜0.16 s(コピーしないビュー) |
| `count(*) FROM scip_index_symbols` | 0.02〜0.03 s | 257〜260 MiB | 0.02〜0.03 s |
| `SELECT * FROM scip_index_metadata` | 0.01 s | 240〜251 MiB | 0.01 s |
| `count(*) FROM scip_occurrences` | 0.08 s | 256〜262 MiB | 0.00〜0.01 s |
| `count(*) FROM scip_symbols` | 0.03 s | 242〜258 MiB | 0.00 s |
| ビューのファイルを読んだだけ | 0.00 s | 250〜263 MiB | |
| `unit_fan_in_churn` | 15.05〜15.50 s | 8,641〜8,740 MiB | 0.12〜0.15 s |

`cache.sql` の上のピーク RSS は作成を含むので 8.7〜9.0 GB で、表から省いた。

## 4. キャッシュ

### 永続キャッシュ(`refresh.sql`)

表 `cache.scip_occurrences`・`cache.scip_symbols` に、形を整えた行(`file_id` を除く)を持つ。`scip_occurrences` は問い合わせのときに `files` と結ぶ。前回までの `nodes` と同じ扱いで、`00_load.sql` を読み直して `file_id` が付け直されても合う。

鍵は、`scip_index` のパス、`read_blob` で読んだ内容の `hash()`、形を整える 5 つのビューの本文の md5 をつないだものである。依頼は、30 MB のハッシュが遅ければ大きさと更新時刻にすることを認めていた。`hash(content)` は 0.02 s、`md5(content)` は 0.07〜0.08 s だったので、内容のハッシュにした。鍵が変わったとき(と、`meta` が無いとき、前回の更新が終わっていないとき)は 2 つの表を空にして作り直す。差分の更新はしない。索引はファイル全体が 1 つの成果物で、作り直しが 0.3〜0.7 s で済むためである。鍵が同じとき、挿入の `WHERE getvariable('refresh_scip_reset')` は計画で `Empty Result` に畳まれ、索引は読まれない(fixture で計画を確かめている)。

`views/scip.sql` が読まれていないとき、鍵は NULL になり、表は空にされ、2 つのビューは空になる。ライブで SCIP のビューが無い状態に合わせた。`views/scip.sql` が読まれているのに索引が無いときは、`meta.state` を `refreshing` にする前に誤りで止める。途中で止まると、次の更新が全体の作り直し(約 22 s)になるためである。

`meta` には `scip_key`、`scip_cache`(`rebuilt` か `kept`)、`scip_occurrences_inserted`、`scip_symbols_inserted` と、層ごとの時間 `scip` を足し、スキーマの版を 3 に上げた。版 2 のファイルは「削除せよ」の誤りになる。`meta` の列を最初は `scip` と名づけたが、層ごとの時間の `scip` と `check.py` の中で同じ名前になり、`measure-refresh` の集計が壊れた。前回の記録の `blob_files` と同じ失敗で、`scip_cache` にした。

`views/scip.sql` を読むかどうかは、他の層の作り直しを起こさない。上の通り、導出のキーに入るマクロを置かず、導出のキーに入る名前(`*_units`、`*_tags`)のビューも置いていない。更新の fixture で、SCIP の段階のあいだ他の層の統計がすべて 0 であることを確かめている。

`uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 3`:

| 場合 | wall time | SCIP の層 | ピーク RSS | 前回の記録 |
| --- | --- | --- | --- | --- |
| 空のキャッシュ | 22.01〜22.38 s | 0.65〜0.68 s | 6,715〜6,888 MiB | 20.05〜25.79 s |
| 何も変わっていない | 0.22〜0.26 s | 0.01 s | 372〜401 MiB | 0.24〜0.25 s |
| 1 ファイルの編集 | 0.32〜0.39 s | 0.01 s | 381〜408 MiB | 0.31〜0.33 s |
| 10 ファイルの編集 | 0.41 s | 0.01 s | 480〜494 MiB | 0.46〜0.66 s |
| 3 ファイルを変えた 1 コミット | 0.39〜0.42 s | 0.01 s | 392〜441 MiB | 0.36〜0.43 s |
| amend(履歴の作り直し) | 3.06〜4.45 s | 0.01 s | 450〜472 MiB | 3.56〜4.17 s |
| 索引の変更(SCIP の作り直し) | 0.62〜0.84 s | 0.32〜0.51 s | 404〜465 MiB | — |

「索引の変更」は、索引の末尾に空の文書 1 つ(protobuf の欄 2、長さ 0 の 2 バイト)を足して内容のハッシュを変えた場合で、行は同じである。測定の後に索引を元のバイト列に戻した(sha256 で確かめた)。キャッシュファイルは測定の終わりで 628.3 MiB(WAL を含む)だった。

キャッシュからのクエリは、`unit_fan_in_churn` が 0.15 s・ピーク RSS 770〜789 MiB(更新を含む)で、他の 6 つは前回の記録と同じ範囲だった(`unit_churn` 0.17〜0.18 s、`clone_change_consistency` 0.21〜0.22 s)。

`check.py refresh target/code-db/clone` では、1 回目(空のキャッシュ、28.7 s、SCIP の層 1.27 s)と 2 回目(何も変わっていない、0.95 s)の両方で、13 ビューと 7 つのクエリがライブと一致した。`check.py cli target/code-db/clone` では、CLI の `.read` と Python で、件数(526・9,073・14,646・34,808・1,502・301,687・42,839)と 7 つのクエリの行数がライブと更新後の両方で一致した。

### セッションキャッシュ(`cache.sql`)

`scip_document_occurrences`・`scip_document_symbols` を一時表 `scip_occurrences_cache`・`scip_symbols_cache` に写し、`scip_occurrences`(`file_id` は問い合わせのときに結ぶ)と `scip_symbols` をそれらに向けた。作成は 18.29〜18.50 s(前回 17.04〜20.44 s)で、うち SCIP の 2 層が 0.14〜0.15 s と 0.04 s だった。`views/scip.sql` が無いときは、`refresh.sql` と同じ空のビューを作ってから写すので、SCIP のビューは空で存在する。

## 5. 検査(`check.py fixture`)

SCIP の fixture を足した。最初の fixture のリポジトリの `src/` と `tests/` を、パッケージ名 `fixture` の `Cargo.toml` と一緒に一時ディレクトリに写し、rust-analyzer でその索引を作って、リポジトリの `target/code-db/index.scip`(既定の `scip_index`、fixture では `target/` は無視される)に置く。rust-analyzer の実行は約 6 s である。rust-analyzer が無ければ、メッセージを出して飛ばす。手で作った protobuf の索引でなく本物の索引にしたのは、役割の bit、囲む範囲、`local N`、ファイル全体のモジュールの定義を、rust-analyzer が実際に出す形で確かめるためである。代わりに、主張は rust-analyzer の版に依存する。

確かめていること:

- メタデータ(`rust-analyzer`、4 文書、出現の数が `count(*)` と同じ、外部記号 0)。4 文書すべてが `files` の行に結ばれる(`src/scratch.rs` はどのクレートのモジュールの木にも無いので索引に無い)。
- トレイト `Shape` の定義の出現が 1 始まりの行と列(`pub trait Shape` の行、列 11〜16)で、囲む範囲がその unit の行(1〜3)と同じ。参照の出現 3 つ(`rect.rs` の `use` と `impl`、`shapes.rs` の `impl`)。
- `local N` が文書ごとに修飾されている(`local 0` が 3 つの文書で別の記号になる)、`scip_symbols` の `enclosing_symbol` も同じ。パッケージの `crate/` 記号が 2 つの文書の下に挙がる。
- `unit_fan_in_churn` の全行:`src/lib.rs` の `area`(結合テストから呼ばれる、fan-in 1、参照 1、2 コミット)と `src/shapes.rs` の `Shape`(`rect.rs` から 2 回、1 コミット)。`kind := 'type'` で後者だけ、`min_fan_in := 2` で 0 行。`lib.rs` の `mod rect;` などは参照で、モジュールの定義はファイル全体にわたるので、どの unit の fan-in にもならない。
- `cache.sql` の上で同じ行になり、クエリの計画に `READ_SCIP`(と `READ_JSON`)が無い。
- `tests/*` をファイルの集合から外すと、`tests/it.rs` の出現は `file_id` を持たず、`area` の fan-in は数えられない。
- `views/scip.sql` を読まない接続で `cache.sql` を読むと、SCIP のビューとコピーが空で存在する。

更新の fixture には、中断の後の段階の次に SCIP の段階を足した:`views/scip.sql` を読んだ後の更新(SCIP は作り直し、他の層の統計は 0、挿入の行数がライブの行数と同じ)、何も変えない更新(`kept`、挿入の計画が索引を読まない)、索引の変更(作り直し)、索引が無いときの誤り(`meta.state` は `ok` のまま)。新しいセッションで `views/scip.sql` を読まずに更新すると、SCIP の表が空になり、他は作り直されない。これまでの段階の更新(`views/scip.sql` なし)は、初回と中断の後で `rebuilt`(0 行)、他で `kept` である。すべての段階で、更新後の各ビュー(SCIP の 2 つを含む)と各クエリがライブと一致する。

何も変わっていないときの、同じセッションでの 3 回の読み込みは、切り替えの前で 126〜168 ms、後の 2 回の実行で 285〜298 ms と 139〜150 ms だった(条件は 500 ms 未満のまま)。285〜298 ms の実行中は他の作業の負荷が高かった。

## 6. うまくいかなかったこと・残っていること

- **`meta` の列名の衝突**(4 節)で `measure-refresh` が一度落ちた。名前を変えて測り直した。
- **索引は木と照合されない。** 索引を作った後に編集されたファイルでは、行の位置がずれる。README に「編集の後は作り直す」と書いただけである。
- **履歴の blob の索引は無い。** fan-in は今の木だけで、churn だけが履歴を持つ。
- **SCIP の relationships は数だけ**で、rust-analyzer 1.99.1 は書かない。トレイトの実装からトレイトのメソッドへの参照は出現として出ないので、`Shape.area` の fan-in は 0 である。
- **rust-analyzer の索引の時間**は、この作業ツリーで 276.7 s で、負荷のない条件では測っていない。索引の作成は層の外の手順である。
- `scip_symbols` の `kind` は、代役のあいだ数値だったものを名前に替えた。代役の数値の測定はその形での値である。
- スパイクのコードとの 2 行の差は、hunk の出所の違いで、この層の側は直していない(前回の記録と同じ扱い)。

## コマンド

```sh
duckdb-ext/build.sh
git clone -q . target/code-db/clone
duckdb-ext/sql/jscpd.sh target/code-db/clone target/code-db/clone/target/code-db/jscpd src crates preview-plugin/src
rust-analyzer scip . --output target/code-db/index.scip      # この作業ツリーと clone は同じ木
cp target/code-db/index.scip target/code-db/clone/target/code-db/
uv run --locked --script duckdb-ext/sql/check.py fixture
uv run --locked --script duckdb-ext/sql/check.py repo target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py refresh target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py cli target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py explain target/code-db/clone
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only scip_
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only baseline
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only fan_in
uv run --locked --script duckdb-ext/sql/check.py measure target/code-db/clone --runs 3 --only "build cache"
uv run --locked --script duckdb-ext/sql/check.py measure-refresh target/code-db/clone --runs 3
```

代役(4925816b の `views/scip.sql` と `check.py`)では、`scip print --json` の出力を `index.scip.json` に置き、`check.py` がそれを索引より古いときに作り直していた。スパイクとの比較は、`git archive 475599d5 scripts/code-db` を `target/code-db/spike` に展開し、`uv run --locked --script target/code-db/spike/scripts/code-db/build.py build target/code-db/clone --out … --languages rust --clones … --scip target/code-db/index.scip` の後、`build.py query … unit_fan_in_churn --format csv` の出力と比べた。
