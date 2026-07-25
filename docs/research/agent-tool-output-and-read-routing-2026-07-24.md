# Agent tool output / read routing 調査（2026-07-24）

Agent #61 のトークン消費を起点に、OpenCode、Crush、Hermes Agent の
read/search の使い分けと、古い tool result の扱いを再調査した記録。
参照した revision は次のとおり。

- OpenCode: `743f6410f2e5002723fc5e893039ac49fbfe0de8`
- Crush: `7946b21e2e7c41838cb7685b41032151a20248b5`
- Hermes Agent: `d9165d7a678d4105f42921a7fc1886df3804531b`

## Agent #61 の実測

Agent #61 は provider request 199 回、tool call 208 回で、tool result の
JSON は約 889k characters、そのうち `fs.read` は約 639k characters だった。
`fs.read` 49 回（23 unique paths）の本文を window 指定の有無で分けると
次のようになる。

| 用法 | 回数 | 本文合計 | 平均 | 最大 |
|---|---:|---:|---:|---:|
| `offset` / `limit` 指定あり | 33 | 143,400 chars | 4,345 | 24,464 |
| 指定なし | 16 | 456,386 chars | 28,524 | 95,398 |

指定なしは回数では少数だが、read 本文の約 76% を占める。また同一 version /
同一 window の完全重複が 2 組あった。このため、まず read の無指定既定値と
出力上限を狭め、次に検索から必要箇所を読む経路を強くするのが、モデルの
推論手順を大きく変えずに効く。

## read / search の使い分け

OpenCode の read tool description は、特定内容を探す場合は grep、path が
不明なら glob、既知の独立ファイルは parallel read と明示する。既定は
2,000 lines だが 50 KiB の hard cap を併用している。

Crush は上位の tool guidance で、必要な `offset` / `limit` だけを読み、
whole-file read を避けるよう指示し、view の既定は 200 lines。ただし編集
workflow には「編集前にファイル全体を読む」という逆向きの指示も残るため、
個別プロンプトの一句だけでなく tool contract 側の上限が必要である。

Hermes Agent は read の既定 500 lines、最大 2,000 lines と
`next_offset` を持ち、所在確認には `search_files` を使わせる。さらに同一
read の exact dedup がある。3 call 以上の探索は `execute_code` 内でまとめ、
中間結果を会話 context に載せない経路も持つ。

Horizon では既存の独立 `fs.glob` / `fs.grep` を活かし、`fs.read` の
description から明示的に routing する。小さな 30-line slice を連打させる
より、検索後に意味のある一つの window を読む方を優先する。

## tool output と履歴

OpenCode の production prune は canonical DB を変更せず、provider prompt
projection 上で古い tool output を置換する。直近 40k tokens と直近 2 user
turns を保護し、回収可能量が 20k tokens 未満なら小さな prune を行わない。

Crush は rolling summary を持つ。以前候補に挙がった「小モデルによる
low-signal line deletion」は、調査した current revision には存在しない。

Hermes Agent は deterministic な dedup / demotion を先に行い、その後に
rolling summary を使う。まず意味判定を要しない圧縮を行う形は Horizon にも
適用しやすい。

## Horizon の決定

> **後日の変更（2026-07-25）。** 以下のうち 2 点はその後の実測で覆った。
> `fs.grep` の context 返却は撤回され、grep は所在（path + line_number）
> のみを返す（`d74a75e`）— 実測で context 行は所在の約 5 倍のコストを
> 持ち、後続の read と重なったのは 16% だけだった
> （`agent-read-navigation-prior-art-2026-07-25.md` と roadmap の
> context-consumption 項を参照）。また provider projection の
> exact-duplicate 除去と early soft prune は、履歴 pruning 一式の撤去
> （`162967f`、オーナー決定）とともに削除された。read の bounds と
> prompt routing は現行のまま。

今回の slice は次を導入する。

- `fs.read`: default 500 lines、explicit maximum 2,000 lines、本文 50,000
  characters hard cap、`next_offset` を返す。mtime と file size から
  `content_version` を返す。
- `fs.grep`: directory に加えて single file を受け取り、前後 context
  （各最大 10 lines）を返す。各行 2,000 characters、結果 JSON 全体
  50,000 characters を上限にする。
- prompt routing: specific content は grep、unknown path は glob、
  independent known files は parallel read、という入口を tool description
  に置く。
- provider projection: 同じ path / line window / `content_version` の
  `fs.read` は新しい一つを残して exact duplicate を除く。
- early soft prune: protected recent tool results 8k estimated tokens を残し、
  古い tool result の回収可能量が 8k tokens に達した時だけ一括で
  placeholder 化する。context hard limit 超過時の既存 prune はその後も働く。

exact duplicate と soft prune は provider request 用に clone した履歴だけを
変更する。canonical `rig_history`、event log、DuckDB projection に記録された
実際の tool result は保持するため、`recall` で回収できる。raw artifact、
UI 表示、model projection を完全に三層分離する共通 artifact store は、
bash を含む全 tool の統一策として別 slice に残す。
