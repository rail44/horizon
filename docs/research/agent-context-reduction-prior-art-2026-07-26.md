# Agent 文脈削減の先行実装調査 — OpenCode / Hermes（2026-07-26）

`agent-tool-output-and-read-routing-2026-07-24.md`・
`agent-read-navigation-prior-art-2026-07-25.md` の続き。消費の支配項を
**d（1 往復あたりの履歴増分）× N（往復数）** と置いた方針相談のため、
既存 2 本の doc がカバーしていない観点をソースレベルで確認した記録。
調査は Sonnet worker 2 本（OpenCode / Hermes 各 1）への委譲で行い、
本 doc はその報告の統合。引用は `path:line`（各 repo の下記 revision 時点）。

- OpenCode: `sst/opencode` @ `743f6410f2e5002723fc5e893039ac49fbfe0de8`
  （7/24 調査と同一 revision を checkout。live path は
  `packages/opencode/src/session/*` — `packages/core/src/session/runner/*`
  は未配線の並行実装で、以下は前者のみを指す）
- Hermes Agent: `NousResearch/hermes-agent` @
  `d9165d7a678d4105f42921a7fc1886df3804531b`（同上、`git cat-file -e` で
  同一性確認済み）

## 1. read の重複処理（d-1）

**OpenCode: 何もない。**

- read の dedup・短絡経路・既読状態の追跡は皆無。同一 read は常に全文を
  再返却する（`tool/read.ts` 全読で確認）。
- 「編集前に read 必須。していなければエラー」はツール記述の文言だけで
  （`tool/write.txt:5`, `tool/edit.txt:3`）、実装はどちらも一切
  チェックしない。プロンプト上のフィクション。
- 唯一のガードは doom-loop 検出：`session/processor.ts:29`
  `DOOM_LOOP_THRESHOLD = 3` — 直近 3 パーツが同一ツール・byte 同一入力
  なら `permission.ask` で割り込む（既定 action は "ask"）。出力の
  短縮ではなく実行の中断。

**Hermes: ツール層 dedup がある（append-only・cache 安全）。**

- `tools/file_tools.py:1209-1267`。キーは `(path, offset, limit)` +
  前回 read 時の mtime。ヒットしたら**新しい tool result として**短い
  stub を返す — 過去メッセージには触れない：
  > "File unchanged since last read. The content from the earlier
  > read_file result in this conversation is still current — refer to
  > that instead of re-reading."（`file_tools.py:796-800`）
- mtime が変わっていれば素通しで実 read。write/patch は同 path の
  dedup エントリを全て無効化（`file_tools.py:1453-1494`）。
- エスカレーション：stub 2 連発で hard block、同一実 read 3 回で
  warning・4 回以上で BLOCKED（`file_tools.py:1234-1257,1379-1397`）。
- 別途、圧縮イベント内には md5 による完全重複除去もある（§2）。
  そちらは**古い側を書き換える**が、発火は圧縮時のみ。

**Horizon への含意**: cache 安全な dedup の前例は Hermes のツール層型
そのもの（append-only、新しい呼び出し側に短い応答）。ただし自前計測
（7/25 doc §読み直しではない）では range-union の重複は read バイトの
21% が上限で、効果は中程度。grep locations-only 化後の read +73% 補償で
再 read 率が変わった可能性は未計測。

## 2. 履歴の圧縮（d-3）

**OpenCode**

- **prune（古い tool 出力の placeholder 化）は実装済みだが既定 OFF**
  （config `compaction.prune`、default false —
  `packages/core/src/v1/config/config.ts:154-156`）。
  定数: PRUNE_PROTECT 40k / PRUNE_MINIMUM 20k / 直近 2 user turn 保護 /
  `skill` ツール出力は不可侵（`session/compaction.ts:28-34`）。
  変異は storage ではなく projection：`compacted` タイムスタンプを
  立てるだけで、リクエスト構築時に出力を
  `"[Old tool result content cleared]"` に差し替える
  （`session/message-v2.ts:293-296`）。**call の引数は残る**。
- **auto-compact は既定 ON**。発火は絶対トークン
  （`model.limit.input − reserved（既定 min(20k, maxOutput)）` 超過、
  `session/overflow.ts:10-34`）。専用の隠し compaction agent が固定
  Markdown テンプレ（Objective / Important Details / Work State
  (Completed/Active/Blocked) / Next Move / Relevant Files）で要約。
  既存要約があれば「更新」を指示する iterative 型。
- 要約後: 直近 2 user turn を**予算付きで原文維持**
  （`preserve_recent_tokens` = usable の 25% を [2k, 8k] に clamp、
  収まらない turn は分割）。履歴は DB に全量残り、provider 向け配列だけ
  境界以前を除外する projection（`filterCompacted`,
  `message-v2.ts:521-583`）。セッション ID 同一・破壊なし。
- auto 圧縮後は合成 user message
  「Continue if you have next steps, or stop and ask…」で自走継続。
- **prompt cache への言及はコード・コメント共にゼロ**。cache breakpoint
  は毎リクエスト先頭 system×2 + 末尾 2 メッセージに機械的に再配置
  （`provider/transform.ts:357-380`）。prune が cache を壊さないのは
  「古い側しか触らない」ことの構造的帰結で、設計意図の記述はない。

**Hermes**

- 発火: 文脈の 50%（小文脈モデルは 75% に引き上げ、閾値トークンの床
  64k。`context_compressor.py:1614-1720`, `model_metadata.py:196`）。
  turn 数・経過による発火はなく、実測トークン比のみ。
- 1 回の `compress()` 内の段階（`context_compressor.py:947-955`）:
  1. **md5 完全重複除去** — 全履歴を走査し、同一内容（≥200 chars）の
     tool 出力は古い側を
     `"[Duplicate tool output — same content as a more recent call]"`
     に書き換え（`context_compressor.py:2135-2159`）
  2. **demotion** — LLM を使わない規則ベースの 1 行要約
     （`[terminal] ran `npm test` -> exit 0, 47 lines output` の形）へ、
     保護 tail より古い全 tool 結果を落とす
  3. tool_call の引数 JSON も 500 chars 超は切詰め（JSON 妥当性維持）
  4. **pressure demotion** — 保護 tail 自体が予算の 1.5 倍を超えるなら
     tail 内も demote（ただし直近 3 メッセージは常に原文）
  5. **LLM rolling summary** — head（先頭 3）と tail（閾値の 20% 予算、
     メッセージ数の床 8）を除いた middle だけを、**補助モデル**
     （`summary_model_override`、失敗時は主モデルに fallback）で
     構造化テンプレへ要約。既存要約があれば iterative update。
     要約は最大 10k トークン。
- 履歴は**永続書き換え**（既定は session_id を rotate して SQLite を
  分割、`compression.in_place` で同一 id のまま書き換え。
  `conversation_compression.py:1222-1233`）。
- cache への態度は明示的：圧縮イベント自体が cache 破壊なので、
  dedup/demotion をそこに同梱すれば追加コストはないという整理
  （`context_compressor.py:2726-2730` — 要約は「cached prefix の外」
  という注記、`conversation_compression.py:1764-1772` — system prompt
  部分は KV-cache 温存のため byte 一致を維持）。

**Horizon への含意**: 両実装が同じ形に収斂している —
**履歴書き換えはまれな圧縮イベントに束ね、cache 損はそこで 1 回だけ
払う。連続的（毎ターン）の prune は OpenCode ですら既定 OFF**。
Horizon が prune を撤去した判断（`162967f`）と同型。つまり d-3 は独立
機能ではなく、天井対策（compaction + 強制要約）の内部フェーズとして
設計するのが先行実装の水準。決定論フェーズ（dedup/demotion）→ LLM
要約の順序（Hermes）と、projection で DB を無傷に保つ実装
（OpenCode）は組み合わせ可能。

## 3. 委譲の構造化（d-2）

- **決定論的に委譲を強制する実装は今回も見つからない**（7/25 doc の
  「routing はどこも例外なくプロンプト」を再確認）。決定論的なのは
  委譲の**周辺**だけ：
  - OpenCode: subagent 深さ上限 1（`tool/task.ts:104-117`）、explore
    agent は read 系 7 ツールの whitelist のみ
    （`agent/agent.ts:196-218`）、子への `todowrite`/`task` 自動 deny。
  - Hermes: `delegate_task`（並列 batch 可、子の中間 tool call は親に
    一切載らず要約のみ返る、再帰 delegate は blocked —
    `tools/delegate_tool.py:1-49`）。
- **コードレベルの incentive の唯一の前例は Hermes の iteration
  refund**: そのターンの tool call が execute_code（PTC）だけなら
  iteration 消費を返金する（`conversation_loop.py:5424-5430`）。
  委譲・畳み込みを「予算上おトク」にする構造で、強制はしない。
- プロンプト誘導の強度は OpenCode が最も高い：
  `session/prompt/anthropic.txt:86`
  > "VERY IMPORTANT: When exploring the codebase … it is CRITICAL that
  > you use the Task tool instead of running search commands directly."
- **追加ターンの前例**: OpenCode Task は `task_id` を渡すと同じ
  subagent セッションを継続できる（one-shot ではない。
  `tool/task.ts:47-50,136-138`）。Horizon の `agent.explore` follow_up
  と同型。
- 親への返却は「最後の text パートのみ」（OpenCode、サイズ上限なし）／
  「要約のみ」（Hermes delegate）。

## 4. cap 到達時の挙動

どちらも**ハードエラーにしない。強制要約で部分成果を保全する**。

- OpenCode: step cap（既定は無限、agent 設定時のみ）到達で
  `MAX_STEPS_PROMPT` を注入 — 「tools 無効。テキストのみで、達成済み・
  残作業・次の推奨を要約せよ」（`session/prompt.ts:1178-1281`）。
  subagent が cap に当たると、この強制要約がそのまま `task_result`
  として親に返る = **cap でも部分成果は失われない**。
- Hermes: `max_iterations` 90（subagent は 50 で独立予算）+ grace call
  1 回。到達したら合成 user message
  > "You've reached the maximum number of tool-calling iterations
  > allowed. Please provide a final response summarizing what you've
  > found and accomplished so far, without calling any more tools."
  を積んで **toolless の追加 1 コール**を主モデルに投げ、それを最終
  応答にする（`chat_completion_helpers.py:1904-1910`,
  `turn_finalizer.py:90-142`）。ユーザ向けにも「`continue` で続行可」
  と案内。検証待ちの応答が既にあればそれを温存。
- **Horizon の現状（探索 cap 死で成果全喪失、エラーに follow-up 案内
  なし）は両実装の水準に達していない。**

## 5. バッチ入力と並列実行（N-1 / N-2）

- **複数範囲・複数ファイルを 1 呼び出しで受ける read はどちらにも
  ない**。read は両者とも単一 path + 単一窓。
- 配列・複数対象入力の前例は書き込み・整理系に集中:
  OpenCode `question`/`todo`/`apply_patch`（複数ファイル hunk 可、
  ただし edit と排他）; Hermes `web_extract`（urls ≤5）/`memory`
  （operations batch）/`todo`/`delegate_task`（tasks batch）/`patch`
  （V4A 形式で複数ファイル）。
- 「複数読みたい」への両者の答え:
  - OpenCode = **並列 tool call**。1 応答内の複数 call を
    `concurrency: "unbounded"` で並行実行（`session/processor.ts:571-575`）。
    call 数上限なし、API の `parallel_tool_calls` フラグには触れず、
    プロンプト 3 箇所で強く奨励（`default.txt:82`, `anthropic.txt:83-84`,
    `task.txt:13` — 「独立な call は 1 メッセージにまとめよ」）。
  - Hermes = **実行側スケジューリング + PTC**。プロンプト奨励はゼロで、
    モデルが出した batch をルールエンジンが並列化（read-only 安全集合、
    path 重なり検査、8 workers。`tool_dispatch_helpers.py:41-59,108-200`）。
    多段の読み＋加工は execute_code に畳む（stdout 上限 50k、
    スクリプト内 tool call 上限 50）。
- Horizon の現状: 複数 call/応答は end-to-end で動き、
  `parallel_tool_calls: true` も明示送信している
  （`providers/rig/completion.rs`）。だが requester の実測は
  1.08〜1.22 calls/req でバッチングは事実上不使用
  （`agent-ceiling-death-autopsy-2026-07-26.md` — 当初 2.8 calls/req
  と記録したのは explore 子セッションの値で、requester の値ではない。
  訂正済み）。並列を促す文言は system prompt にない。

## 6. 既知事項の深掘り（preamble / スキーマ遅延開示）

- Hermes `tool_search` の 10% ゲートを実確認（`tool_search.py:234-258`。
  context 長不明時は固定 20k — 「Anthropic/OpenAI 双方で品質低下が
  観測された崖」とコメントが引用）。core ツールは決して defer しない。
- `tool_describe` の結果は**通常の tool result テキストとして**文脈に
  入り、tools 配列へ恒久昇格しない。組み立ては毎リクエスト stateless
  に再計算（`tool_search.py:14-18,529-582`）。

## 7. 方針への写像（相談材料。決定は未了）

| 論点 | 先行実装の形 | Horizon への含意 |
|---|---|---|
| d-1 read dedup | Hermes ツール層（append-only・mtime キー） | 前例通り実装可・cache 安全。ただし自前計測では効果上限 ~21% |
| d-3 圧縮 | 圧縮イベントに一括（決定論→LLM の順）。連続 prune は既定 OFF | 独立の eviction は作らず、天井対策と一体の compaction として設計 |
| d-2 委譲 | 強制は皆無。予算 refund（Hermes）が唯一のコード incentive | 予算型 incentive が「プロンプト以外」の唯一の前例形 |
| cap | 両者とも強制要約で部分成果保全 + 続行案内 | explore の cap 死修正はこの形をそのまま輸入できる |
| N-1 multi-read | 前例なし | やるなら Horizon 独自判断になる |
| N-2 並列 call | 実行系は両者並行。奨励はプロンプト（OpenCode）か無し（Hermes） | まず自前の実行系の並行性を確認。奨励文言を入れるかは別判断 |
