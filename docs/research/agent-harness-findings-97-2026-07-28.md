# ハーネス改善の実測所見 — session aa95e066 の解剖（2026-07-28）

オーナーの問い「この走行から Horizon ハーネス側の改善点を調べたい」への
回答。イベントログ 10,483 件（seq 102441–121204、実働 ~3.3h）の全解析
（Opus worker 委譲）。実装レビューとは別軸の、ハーネス挙動の記録。

## 総括

compaction シリーズの目的（context を理由に死なない）はこの走行で達成
され、拘束は context から**トークン予算と無関係な 3 つの人間面**へ移った:
(a) モデルに「行動しない」手段が無く、待機がポーリングになる。
(b) この走行の operator 割り込みは 100% が judge のパーサ欠陥で製造され、
うち 3 件は grant 整形欠陥が承認を捨てた。
(c) brief が要求する検証（ゲート）は sandbox 内から構造的に到達不能で、
ハーネスは正直な代替を提供しない（issue 010）。

## 実測の要点

- **ポーリング**: task_output 72 回中 63 回が「still running」（100% が
  完了通知前 = 全て無駄）。57 往復・全リクエストの 13.4%・実働の ~18%。
  同一子への再ポーリング中央値 51.9 秒 vs 子の完了中央値 455 秒（8.8 倍
  速く叩いている）。reasoning に「待つ方法を探して見つからない」逐語が
  4 箇所。
- **judge stage-2 は 8/8 で unparseable**（stage-1 は reasoning_effort
  "none" で 7/7 正常）。300 トークン予算を think が食い潰し VERDICT 行が
  出ない → fail-safe 人間行き。
- **承認 8 件・拒否 0 件**。うち 3 件は「/tmp の DirectoryTree grant を
  提示 → 承認 → revalidate が SYSTEM_ROOTS で必ず拒否」で承認が無効化
  （resolve_denial に is_overbroad_tree が無い非対称）。3 件は自 worktree
  内の git stash/pop（tier 化可能、計 766 秒ブロック）。**真の境界リスク
  は 0 件**。
- **clearing は行動的に不可視**: recall 使用 0、placeholder 言及 0、
  clear 起因の再実行 ≤4/471（0.85%）。コストほぼゼロの実証。ただし
  placeholder が案内する recall 経路は死んだ導線。
- **ガード停止はモデルに知覚されない**: doom×2 + cap×3、停止後の初手が
  4/5 で「同一 call の再発行 + 空 reasoning」。ContinueTurn は何も注入
  しない。doom ガードは子 session_id のローテーションで 26/23 連続
  ポーリングをすり抜け。cap 相転移説はこの走行では不成立（切替 r72→73
  は子の完了通知駆動。追補 5 訂正済み）。
- **子の 75% が劣化**: 8 本中 完走 2・cap 3・provider エラー死 3。
  429（first-token 前）は安全に retry 可能なのにしていない。子 1 本は
  429 で 10 リクエスト/394k トークンを喪失し「部分報告」の中身が
  `</mm:think>` だけ。
- **`</mm:think>` 漏れ**: 全 426 応答の 49% が裸のタグのみ。17 時間の
  実会話文は 4,902 chars。serving 層（vLLM 系 parser）のバグ類型 —
  ハーネス側対処はオーナー判断待ち（防御ストリップ + 発生カウント案）。
- **ゲートのラチェット**: 除外フィルタを 5→30 項へ 20 往復かけて成長、
  r335–426 の 92 往復（21.6%）を消費。fail-fast 既定が「1 回 1 失敗開示」
  を強制。/tmp 書き込みの silent fail（exit 0 なのに未作成）で 2 往復
  追加損失。issue 010 の実演。
- **観測性**: operator の continue・承認可否がイベントに残らず、状態
  遷移からの推測が必要（17 時間で User メッセージ 1 件）。
- bash 結果の定型メタデータ（denied_network_routes 等）~30k chars が
  毎往復再送。

## 改善候補（調査のランク順・実装状態）

1. judge stage-2 パーサ修正 — **worker 実装中（2026-07-28）**
2. grant 整形に overbroad 検査を前置き + $TMPDIR 案内 — **同上**
3. turn-yield affordance + still-running の situated 案内
4. sandbox 安全な nextest プロファイル or 検証責務の明文化（issue 010）
5. first-token 前の 429/503 retry + 子起動の同時実行を provider 上限に
   合わせる
6. `</mm:think>` の防御ストリップ（オーナー判断待ち — 上流バグ類型）
7. 自 worktree 内 git メタデータ書き込みの tier 化
8. ガード停止時の注入（「N 往復連続。目標と次の 3 手を述べよ」）
9. 引数ローテーション耐性のあるポーリング検出（tool_id 単独カウンタ）
10. task_output の unchanged 短絡
11. bash 結果メタデータの痩身
12. continue/承認可否のイベント化
