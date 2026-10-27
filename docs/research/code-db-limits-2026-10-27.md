# コードデータベースのメモリ・スピル上限(2026-10-27)

第 10 増分の記録である。SQL 層(`duckdb-ext/sql/00_load.sql`、`run.sh`、`check.py`)に、読み込みの時点の機械の状態から計算するメモリ・スレッド・スピルの上限と、外部ツールのメモリ上限を入れ、その挙動を測った。Rust のコードは変えていない。

## 経緯

第 9 増分で、`unit_change_sides` の最初の案がライブで 30 GB を超え、`check.py refresh` は 47 GB で OOM により終了した(`code-db-attribution-2026-10-27.md`)。このときスワップは埋まっていた。sitting_duck の評価(`sitting-duck-evaluation-2026-10-24.md` §6 と主要な観察)では、`memory_limit = '4GB'` のもとで 1 つのクエリが一時ファイルに 23 GB を書いた。

オーナーの発言(2026-10-27):

> メモリを使い切るのは機構として原理的に発生しないようにできない?

> 妥当な値が動的に設定されるようにしてください

## 実装した規則

`00_load.sql` は読まれるたびに次を設定する。変数で上書きでき、上書きしたものは計算しない。

| 設定 | 計算 | 上書きの変数 |
| --- | --- | --- |
| `memory_limit` | 「いま OS が出せる量」の 60%。いま出せる量は `/proc/meminfo` の `MemAvailable` に、この DuckDB がすでに持っている量(`duckdb_memory()` の合計)を足したもの。上限は DuckDB の既定値(物理メモリの 80%)、2 GB を下回るときは読み込みを止めてメッセージを出す。`/proc/meminfo` が読めないときは DuckDB の既定値の 60% | `memory_limit` |
| `threads` | CPU 数と、`memory_limit` の GB 数(切り捨て、最低 1)の小さいほう | `threads` |
| `temp_directory` | `repo/target/code-db/tmp` | なし |
| `max_temp_directory_size` | 一時ディレクトリのあるファイルシステムの空きの 50%。空きは変数 `temp_free_bytes` で受け取り、`run.sh` は `df`、`check.py` は `shutil.disk_usage` で計算して渡す。渡されないときは DuckDB の既定値(空きの 90%)のままにし、そのことを要約行に書く | `temp_limit` |

ファイルの最後の文が、選んだ値を 1 行で返す。この機械では次のとおりだった。

```
limits: memory 25.3 GB (60% of 42.2 GB available, /proc/meminfo), temp 29.1 GB (50% of 58.2 GB free), threads 24 (CPUs)
```

`run.sh index` は jscpd と `rust-analyzer scip` を、`MemAvailable` の 80%(`tool_memory_max=SIZE` で上書き)の上限のもとで起動する。`systemd-run --user` が使えれば `--scope -p MemoryMax=… -p MemorySwapMax=0`、使えなければ `prlimit --as=…`、どちらもなければ上限なしで警告を出す。この機械には `systemd-run` がなく(Void Linux、runit)、cgroup v2 の memory コントローラも有効になっていないので、`prlimit` の経路だけを試した。systemd の経路は動かしていない。

## 簡単な確認

- DuckDB 2.0.0 の `read_text('/proc/meminfo')` は動く。`/proc` のファイルはサイズ 0 と報告されるが、1,672 バイトの内容が返った。
- `SET memory_limit = getvariable('m')` のように、`SET` は式を受け付ける。`RESET memory_limit` と `RESET threads` で DuckDB の既定値(49.6 GiB、24)に戻り、そこから既定値を読める。
- `/proc/meminfo` を存在しないパスに置き換えた写しでは、`memory 31.9 GB (60% of DuckDB's default 53.2 GB; /proc/nonexist unreadable)` になった。割合を 0.01 にした写しでは、`only 42.9 GB of memory is available, and 60% of it is below the 2 GB floor: free memory, or set the variable memory_limit (README.md, Limits)` で止まった。
- `memory_limit='4GB'` を与えると `threads` は 4、`'2GB'` では 2 になった。

## 測定

使い捨てのクローン(`target/code-db/clone`、e51314d0)で測った。ピーク RSS は、プロセス木の最大の RSS を 0.5 秒ごとに採ったものと、`RUSAGE_CHILDREN` の `ru_maxrss` の両方を取り、両者はどの回も 0.1 GB 以内で一致した。スピルの量は一時ディレクトリの使用ブロックを 0.5 秒ごとに採った。測定の開始時点で、スワップは 15 GB のうち 14 GB が使われていた。ブロブのファイル(`target/code-db/blobs`)は最初の回で書かれ、以後の回では既にあった。

### `check.py fixture`

すべて ok だった(fixture、history、lineage、scip、graph、counting、refresh の各 fixture)。fixture のリポジトリは `/tmp`(tmpfs、空き 33 GB)にあり、要約行は `temp 16.5 GB (50% of 33.0 GB free)` だった。

### 冷えた refresh(動的な上限)

`check.py refresh CLONE`(キャッシュを消してから):

| 項目 | 値 |
| --- | --- |
| 上限 | memory 24.8 GB(41.4 GB の 60%)、temp 29.4 GB、threads 24 |
| 1 回目の refresh | 42.50 s、ライブと等しい |
| 2 回目の refresh | 1.51 s、ライブと等しい |
| プロセス全体 | 1,179 s、ピーク RSS 23.3 GB、スピルのピーク 2.3 GB |

プロセス全体の時間とメモリの大半は、比較のためのライブ側の写し(`snapshot_live`)と比較のクエリである。ピーク RSS には、Python に取り出した行も含まれる。

最初の実行は、2 回目の `refresh_against_live` が `00_load.sql` を読み直したところで `Not implemented Error: Cannot switch temporary directory after the current one has been used` で止まった。DuckDB は一度スピルした一時ディレクトリを、同じパスへでも設定し直させない。`check.py` は、接続がすでにそのディレクトリを持つとき、読み直す `00_load.sql` から `SET temp_directory` の文を除くようにした。CLI で同じ読み直しをするとこのエラーになる(README の Limits に書いた)。

`run.sh sql CLONE meta.sql`(refresh.sql だけの冷えた実行、キャッシュとスピルのディレクトリを消してから):

| memory_limit | threads | refresh.sql の total | プロセスの wall | ピーク RSS | スピルのピーク | 結果 |
| --- | --- | --- | --- | --- | --- | --- |
| 25.3 GB(動的) | 24 | 39.8 s | 43.3 s | 7.26 GB | 0 | 成功 |
| 動的(2 回目) | 24 | — | 30.5 s | 6.96 GB | 0 | 成功 |
| 4 GB | 4 | 69.2 s | 73.1 s | 4.51 GB | 0 | 成功 |
| 2 GB | 2 | 68.1 s | 70.8 s | 2.55 GB | 0.89 GB | 成功 |

段ごとの秒数(refresh.sql の `meta.seconds`):

| 段 | 動的 | 4 GB | 2 GB |
| --- | --- | --- | --- |
| nodes | 0.94 | 2.23 | 2.53 |
| blob_nodes | 15.72 | 41.51 | 39.82 |
| blob_units | 3.32 | 5.96 | 15.71 |
| lineage | 12.68 | 12.43 | 6.65 |
| その他の合計 | 7.1 | 7.1 | 3.4 |

2 GB のときのスピルは、開始から 45〜67 s の間(0.7〜0.9 GB)にだけあった。段の秒数を積み上げると blob_units の段(およそ 48〜63 s)に当たる。4 GB ではどの段もスピルしなかった。

2 GB で作ったキャッシュと動的な上限で作ったキャッシュを、全テーブル(meta を除く 18 表)について行数と `sum(hash(行))` で比べ、すべて一致した。動的な上限のキャッシュがライブと等しいことは上の `check.py refresh` で確かめた。

### 重いライブのクエリ:`unit_churn`(キャッシュなし)

`views/*.sql` を読み、`refresh.sql` も `cache.sql` も読まずに `queries/unit_churn.sql` を実行した(CLI)。

| memory_limit | threads | クエリの時間 | ピーク RSS | スピルのピーク | 結果 |
| --- | --- | --- | --- | --- | --- |
| 29.6 GB(動的、49.3 GB の 60%) | 24 | 24.9 s(2 回目 24.5 s) | 17.9 GB | 0 | 9,264 行 |
| 4 GB | 4 | 77.4 s(2 回目 60.7 s) | 4.60 GB | 9.9 GB | 9,264 行 |

2 つの結果は行数と `sum(hash(行))` が一致した。4 GB のときのスピルは 33 s 付近から始まり、40〜60 s に 9.9 GB で頭打ちになり、終わる直前に消えた。

### スピルの上限:`temp_limit = '1GB'`

同じ `unit_churn` を `memory_limit = '4GB'`、`temp_limit = '1GB'` で実行すると、39.6 s で次のエラーになった。ピーク RSS は 4.55 GB だった。

```
Out of Memory Error: failed to offload data block of size 256.0 KiB (953.5 MiB/953.6 MiB used).
This limit was set by the 'max_temp_directory_size' setting.
```

### 外部ツールの上限

`run.sh index CLONE`:

| 上限 | 結果 | 時間 | ピーク RSS | ピークのアドレス空間(VmPeak) |
| --- | --- | --- | --- | --- |
| 34.3 GB(42.8 GB の 80%)、prlimit --as | 成功 | rust-analyzer 150.9 s、jscpd 0.8 s | 7.37 GB | 未測定 |
| `tool_memory_max=10G`(10.7 GB)、prlimit --as | 成功 | rust-analyzer 45.3 s | 7.38 GB | 9.58 GB |
| `tool_memory_max=4G`(4.3 GB)、prlimit --as | rust-analyzer が 4.5 s で失敗 | — | 2.12 GB | — |

4G のとき、ログの末尾は `memory allocation of 45400 bytes failed` と `Aborted` で、`run.sh` は次の 1 行で止まった。前の `index.scip` は残った。

```
run.sh: rust-analyzer scip was killed by signal 6 under the 4.3 GB memory cap (prlimit; raise it with tool_memory_max=SIZE if the log shows an allocation failure); see …/index.scip.log
```

`prlimit --as` はアドレス空間の上限であって RSS の上限ではない。rust-analyzer は RSS 7.4 GB のときにアドレス空間 9.6 GB を使っており、4.3 GB の上限では RSS 2.1 GB の時点で割り当てに失敗した。jscpd は `--as` を 2 GB にしても `--version` が動いた。上限は 1 プロセスごとにかかり、プロセス木全体の合計にはかからない。スワップの使用量の変化は測っていない。

### 上限を超えた量

DuckDB のプロセスのピーク RSS と `memory_limit` の差:

| 実行 | memory_limit | ピーク RSS | 差 |
| --- | --- | --- | --- |
| 冷えた refresh、4 GB | 4.0 GB | 4.51 GB | +0.51 GB(13%) |
| 冷えた refresh、2 GB | 2.0 GB | 2.55 GB | +0.55 GB(27%) |
| `unit_churn` ライブ、4 GB | 4.0 GB | 4.60 GB | +0.60 GB(15%) |
| `unit_churn` + temp 1 GB、4 GB | 4.0 GB | 4.55 GB | +0.55 GB |
| 冷えた refresh、動的 | 25.3 GB | 7.26 GB | 上限に届かない |
| `unit_churn` ライブ、動的 | 29.6 GB | 17.9 GB | 上限に届かない |

上限を超えた分は 0.5〜0.6 GB で、上限の大きさにあまり依らなかった。内訳(拡張の中の割り当て、tree-sitter の木、バッファ管理の外のもの)は切り分けていない。

## 割合について

60%・50%・80% と 1 GB あたり 1 スレッドは、指示どおりのままにした。この機械の測定では、動的な値で失敗したものも、上限を超えて OS 側を圧迫したものもなく、割合を動かす根拠は得られなかった。

## 記録との違い

- 冷えた refresh のピーク(記録では 6.7〜8.7 GB)は 7.0〜7.3 GB、`unit_churn` のライブ(記録では約 17 GB)は 17.9 GB、`rust-analyzer scip` の約 7 GB は 7.4 GB で、記録と矛盾しない。`rust-analyzer scip` の時間は README の 160〜280 s に対して 150.9 s と、キャッシュが温まった 2 回目の 45.3 s だった。
- この増分の前は `temp_directory` を設定しておらず、インメモリのデータベースは DuckDB の既定の `.tmp`(プロセスのカレントディレクトリ、つまり Horizon のチェックアウトの直下)にスピルしていた。

## 未検証

- `systemd-run --user --scope` の経路(この機械にない)。`MemorySwapMax=0` の効果も含む。
- `/proc/meminfo` が存在するが読めない場合(`read_text` がエラーになり、読み込みが止まるはず)。
- メモリの少ない状態で動的な値そのものが 2 GB の下限を割る場合の実機の挙動(割合を変えた写しでだけ確かめた)。
- `memory_limit` を超えた 0.5〜0.6 GB の内訳。
- 2 GB と 4 GB の `check.py refresh`(ライブとの比較つき)。上ではキャッシュどうしの比較だけを行った。
