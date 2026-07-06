# Plan 06 — recall ツール(履歴検索)

Status: ready(開始は plan 03 の着地後 — 同じクレートを触るため)

## 背景

トークン窓(rig-memory 統合)で「送信ビュー」は刈られるが、真実の履歴
は JSONL/DuckDB に全量残っている。Letta 調査の実証(検索は再帰要約に
大差で勝つ、単純プリミティブが専用機構に勝つ)に従い、「要約の高度化
ではなく、窓の外に落ちた過去を検索で取り戻すツール」を次の一手とする
(docs/research/letta.md、docs/research/agent-prompting.md)。

## スコープ

- horizon-agent の新ツール(命名はドメインで。例: `recall`)。検索対象
  は DuckDB プロジェクション。read 系=自動許可の分類。
- 参考の最小 API 形: Letta Filesystem の grep / open 相当(意味検索=
  埋め込みは後日 rig-fastembed で拡張可能な形にだけしておく)。
- 出力の切り詰めは bash ツールの cap(head+tail+退避パス)の流儀を踏襲。

## 完了条件

- トークン窓の外に落ちた過去の指示・結果を、エージェントが recall で
  取り戻して正しく答える一連が実機で通る。
- ゲート緑。config つまみが必要なら example と drift guard 同期。

## ドメインセッションに委ねる設計判断

- クエリの形(文字列 grep / 構造化フィルタの範囲)。
- 検索範囲(セッション内のみか、セッション横断か。横断は情報設計と
  プライバシー面の検討込みで)。
- ツール名と結果の提示形式。

## 参照

docs/research/letta.md / docs/research/agent-prompting.md /
docs/agent-duckdb-state-design.md / crates/horizon-agent(トークン窓:
providers/rig/completion.rs)
