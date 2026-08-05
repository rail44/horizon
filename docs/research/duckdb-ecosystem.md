# DuckDB エコシステム — 用途適合の調査記録(2026-08-06)

調査日: 2026-08-06(DuckDB 1.5.5 時点、2.0.0 は「2026年秋」予定)。
web 検証つき。発端は「単一書き手の射影 + 複数ローカル消費者」という
想定に対し、コア DuckDB の不在(トリガ・CDC・プロセス間共有)を
エコシステムが埋めていないか、というオーナーの問い。**結論として
「複数プロセスからの読み」という要件自体が logd 設計で消滅した**ため
(`docs/logd-design.md`)、ここの大半は不採用の記録である。ただし
事実は正確に残す — 前提が変われば答えも変わるので。

## コアの再確認(すべて現行 doc で確認)

- ファイルロックは「1プロセス read-write」**排他的または**「N プロセス
  全員 read-only」。書き手が居る間は read-only ですら開けない。
  プロセス間の並行は公式に「Quack か DuckLake を使え」が現在の答え
  ([Concurrency])。
- トリガ・CDC・update hook・変更カウンタ(`data_version` 相当)は
  すべて不在。`pragma_database_size()` は checkpoint(WAL 既定 16MB)
  でしか動かず変更検知に使えない。**`.duckdb`/`.wal` の mtime 監視は
  無意味**(テストで動き本番で沈黙する型)。
- 2.0(秋)のロードマップ: **Quack の stable 化**、async I/O(済)、
  Rust 拡張サポート、C API 移行。「トリガ」は DuckCon の tweet 由来で
  公式ロードマップに無く、仮に来てもプロセス内発火なので設計根拠に
  しない。

## エコシステムの2つの本物(不採用だが記録)

- **Quack remote protocol**(1.5.3+ の core 拡張、beta): DuckDB を
  HTTP/2 の client-server にする。「外部並行・内部直列」はまさに
  単一書き手デーモンの形。ただし: 破壊的変更を宣言中、クエリ
  キャンセル未実装、**認可が既定で全許可・read-only 制限の公式手段が
  SQL マクロの正規表現** — エージェントが消費者に居る前提では不可。
  2.0 で stable + Rust 拡張(まともな認可)が来る。**再訪トリガ:
  Quack 2.0 stable 後、外部プロセスに任意 SQL を開きたくなったとき**。
- **DuckLake**(2026-04 に v1.0、production 表記): カタログ(SQL DB)+
  Parquet の分離。**カタログを SQLite にすると単一マシン・複数ローカル
  プロセスの読み書きが公式サポート**になる(DuckDB ファイルを
  カタログにすると元のロック問題が再発する — SQLite カタログは便宜
  ではなく機構)。単調な `snapshot_id` と `table_changes()` の変更
  フィードも持つ。既知の弱点は複数**書き手**の競合(単一書き手なら
  回避)。不採用の理由: 解こうとする問題(プロセス間共有)が要件に
  無い。ストレージ形式の変更・拡張バイナリ・libduckdb ≥1.5.3 という
  コストだけが残る。

## 採用に効いた比較(§6「正直な代替」)

SQLite(WAL モード)は「単一書き手 + 複数プロセス読み手」と
`PRAGMA data_version`(接続外のコミットで変わる無料の変更検知)を
最初から持つ。ただし本ワークロードでは: ~350MB の列指向圧縮データが
行ストアでは数 GB 級になり、sessions/turns/tool-calls への分析的
スキャン(recall の substring 検索、usage 集計)は行単位読みになる —
**DuckDB が 10〜100 倍効く種類の負荷をちょうど手放す**。

この比較が DuckDB 続投の再評価の材料になった(結論と理由は
`docs/logd-design.md` の「射影エンジン」節が正)。

## 運用ノート

- 本機の distro libduckdb は 1.5.0(AGENTS.md が動的リンクを意図的に
  固定)。Quack/DuckLake はどちらも ≥1.5.3 + 実行時拡張ロードを要求
  するため、採用時はバンドル版への移行か拡張の事前配置が前提。
- `duckdb_extensions()` に見える `quack` は extension-template のデモ
  (`SELECT quack('Jane')` → `Quack Jane 🐥`)であり remote protocol
  ではない。名前衝突に注意。

出典: duckdb.org(concurrency / pragmas / roadmap / release calendar /
quack overview・security・FAQ / async I/O blog)、ducklake.select
(1.0 announcement / choosing a catalog / data inlining / change feed)、
github.com/duckdb(discussions #5946 #12408 #12562 #14676、ducklake
issues #128 #233)、sqlite.org(wal.html / pragma data_version)、
duckdb-rs README。
