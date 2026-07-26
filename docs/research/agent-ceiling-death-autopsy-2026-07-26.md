# 天井死 3 セッションの解剖 — T-callid dogfooding（2026-07-26 実施分）

同一 brief（call_id identity、1,895 chars）を 3 回走らせ、3 本全てが
~196k の context 天井で provider 400 死した campaign の事後分析。
追加実行なし、`~/.local/share/horizon/agent-events.jsonl`（59.85MB /
69,172 records）の読み取りのみ。分析は Sonnet worker への委譲で実施
（スクリプトは job tmp、揮発）。数値の定義は各表に付記。

| requester | mode | 往復 | 死亡時 input | explore 子 | 子の結末 |
|---|---|---:|---:|---|---|
| 03207301 | fork | 69 | 196,500 | cbacbfb9 | 完走・11,055 chars 報告 |
| 61582a87 | fresh | 87 | 196,435 | d28db628 | 25 turn cap 死 |
| 7097b945 | fresh | 50 | 195,360 | fc43257d | 25 turn cap 死 |

3 本の死亡時 input は 1,140 トークン幅に収束（固定天井の実証）。
cached 率は 5 往復目で 83〜91%、末期は >99%。

## 死亡時 context の構成（chars→4.114 chars/tok 換算の推計）

| 成分 | fork | fresh-61 | fresh-70 |
|---|---:|---:|---:|
| preamble | 3.6% | 3.6% | 3.6% |
| brief | 0.2% | 0.2% | 0.2% |
| **tool 結果** | **65.8%** | **66.6%** | **76.9%** |
| tool call 引数 | 6.6% | 7.0% | 1.2% |
| reasoning | 13.0% | 9.9% | 5.4% |
| assistant 本文 | ~0%（3 本合計 147 chars） | 0% | 0% |
| 未説明（封筒/換算誤差） | 10.8% | 12.7% | 12.8% |

tool 結果の内訳（3 本合計 1,688,366 chars / 244 calls）:
fs.read 77.7% / fs.grep 19.7% / bash 1.2% / agent.explore 0.7% /
fs.edit 0.7%。

## N の分解 — バッチングは皆無

| | fork | fresh-61 | fresh-70 |
|---|---:|---:|---:|
| tool calls / request | **1.22** | **1.22** | **1.08** |
| 1 往復あたり平均増分 | 2,820 tok | 2,222 tok | 3,912 tok |

- **訂正**: 以前「fork requester は 2.8 calls/req」と記録・報告したのは
  誤り。2.8 は**完走した explore 子**（53 calls / 19 req）の値で、
  requester は mode によらず 1.08〜1.22。fork/fresh に差はない。
- Horizon は `parallel_tool_calls: true` を明示送信している
  （`providers/rig/completion.rs` `openai_turn_additional_params`）。
  API 側 affordance は有効・プロンプト奨励はゼロ・モデルは使わない。
- reasoning はセッションあたり 43k〜105k chars 蓄積され、rig 0.39 が
  assistant メッセージの `reasoning_content` として毎リクエスト再送する
  （rig 内コメント: thinking 有効モデルは欠落で 400 になるため意図的）。

## 軌跡の質 — 「太い往復」ではなく「小さすぎる歩幅 × 終わらない」

- doom loop はほぼ不在: 完全重複の連続 call は corpus 全体で **1 件**
  だけ。しかもそれは sandbox 拒否後の bash 再発行が同一 call_id を
  再利用したもの — **brief が修正対象とした当のバグの実発火**。
- **fork**: round 24 に baseline `cargo check`（クリーン）→ round 25
  （36% 地点）から on-target な編集。しかしフィールド 1 個の追加の
  コンパイル余波処理に rounds 28〜61 を消費（bash で python 一括
  パッチ 3 本、`git checkout --` 差し戻し 3 回、check エラー数は
  クリーン→8→4→1→1→6→クリーンと非単調）。最後の check はクリーン
  だが、下流の消費者更新・テスト・gate には一切届かず死亡。
- **fresh-61**: first edit が round 43（49% 地点）。**round 84 まで
  一度もビルドせず**、round 85（最後の成功往復）の `cargo check` で
  自分のコンパイルエラー 3 件を発見した直後、round 86 で 400 死。
- **fresh-70**: 50 往復・195k トークンを **read/grep のみ**で使い切り、
  edit も bash も 0。round 0 で roadmap.md + backlog.md 全文
  （計 ~73k chars ≈ 17.7k tok）を自主的に読むところから開始。
- 再訪 read 率: 36〜63%（fresh-61 は 54 read 中 34 が既読 path 再訪。
  ただし編集進行に伴う別窓の読み直しが主で、同一内容の完全重複では
  ない）。
- gate（test/clippy/fmt/wire-schema）を走らせたセッション: **0/3**。

## explore の実効

- cap 死 2 本: 各 26 往復・42/49 tool calls の探索作業が、requester に
  返ったのは同文の **164 chars エラーのみ**（部分成果ゼロ、最終
  assistant メッセージ自体が未生成）。91 calls 分の作業が消えた。
- 完走 1 本（fork 側）: 11k 報告（file:line 付き）を受け取ったのに、
  fork requester のその後の read は 39 回 / 476k chars — **fresh と
  同水準**。報告は read を代替しなかった。

## 追補（2026-07-27）: 同一タスクの opencode ベースライン

同一 brief・同一 commit（`744c9a7` の worktree）・同一モデル/provider を
opencode 1.18.5（XDG 隔離、`{cargo-cache}/…/opencode-baseline/`）で 1 走。
結果: **opencode でも完走せず、~218k tokens 時点で provider 400 死**。

| 軸 | Horizon (n=3) | opencode (n=1) |
|---|---|---|
| 完走 | 0/3 | 0/1（400 @ ~218k） |
| 往復 | 50〜87 | **105** |
| calls/req | 1.08〜1.22 | **1.13** |
| read 総量 | 393〜476k chars | 586k chars |
| first edit | 36% / 49% / なし | 11%（step 12） |
| edit 回数 | 0〜28 | 63（結果: lib 15 + test 252 エラー） |
| ビルド検証 | 0〜7 回 | **1 回、最終 step** — 直後に死亡 |
| 委譲 | explore ×1 | task ×1（19.6k 報告 → それでも自力 read 586k） |
| reasoning | 43〜105k chars 再送 | 0（この経路では thinking 無効） |

- opencode はプロンプト 3 箇所で並列 call を強く奨励しているが、
  calls/req は 1.13 — **プロンプト奨励はこのモデルのバッチングを
  変えない**（実測）。
- task 委譲の報告を受けても自力 read は減らない — Horizon explore と
  同じパターンが再現。
- todowrite（2 回）と早期編集（11% 地点）は opencode 側のみの挙動で、
  進行の構造には効いたが収束と消費には効かなかった。
- opencode の auto-compact は発火しなかった（発火閾値 196,608 相当を
  超えても要約イベントなし。custom provider の limit 伝播か 1.18.5 の
  挙動差の可能性 — 未追跡）。
- Horizon の死亡点 196.6k は `max_tokens=65,536` を毎回要求する自縛
  でもある: opencode は同じ窓で ~218k まで入力を伸ばせた（その分
  出力予算が小さい）。
- 結論: **消費病理はハーネスではなくモデル（Kimi-K2.7-Code）支配**。
  2×2 の残り 1 セル（同一ハーネス・別モデル）が次の計測。

## 追補 2（2026-07-27）: 同一ハーネス・別モデル（MiniMax-M3）

Horizon（同 commit の worktree、fork seed env）で同一 brief を 1 走
（オーナー指定 `hf:MiniMaxAI/MiniMax-M3`）。

- 結果: **未完走・provider 400 @ 196,280**（max_tokens=65,536 自縛の
  同じ死亡点）。
- **165 リクエスト**: iteration cap 100 で一時停止（leg 1: 102 req）→
  operator が `continue-turn`（leg 2: 63 req）。
- calls/req **0.99**（164 calls / 165 req）— Kimi よりさらに低い。
- 増分 ~1.2k tok/往復 — Kimi（2.2〜3.9k）の半分以下。リーンな分、
  同じ窓に倍の往復が入った。
- スタイル: bash 67 / read 63 / edit 30 / patch 2 / write 1。
  **leg 1 は編集ゼロ**（8.8k chars の設計 doc を書いて cap 到達）。
  **continue 直後の request 104（63% 地点）から実装に相転移** — 強制
  チェックポイントが行動を切り替えた。
- 最終状態: 9 ファイル +394/−158、ただし最後の cargo check は
  コンパイルエラーのまま死亡。cargo 系実行は 3 回。
- explore 1 回は**子セッションが即 400 死**: "Error applying chat
  template for MiniMaxAI…" — fork seeding が複製する履歴形状が M3 の
  chat template に不適合（新規バグ。Kimi は同じ形状を受理していた）。

### 2×2 完了後の不変量

| | Horizon | opencode |
|---|---|---|
| Kimi-K2.7-Code | 0/3 完走、400@195.4〜196.5k、1.08〜1.22 calls/req | 0/1、400@~218k、1.13 |
| MiniMax-M3 | 0/1、400@196.3k、0.99 calls/req | —（未実施） |

全 5 走に共通: (1) **~1 call/往復**（プロンプト奨励・`parallel_tool_calls`
フラグ・モデル・ハーネスの全てに不変）、(2) 委譲はちょうど 1 回使われ、
報告が自力探索を代替しない、(3) 検証が末期に偏る、(4) 完走ゼロ・
context 上限死。モデル差は d とスタイル（M3 はリーン・doc-first、
Kimi は read 太め・opencode 下では edit 乱発）に出たが、**収束性には
出なかった**。

## 読み方（設計への含意、決定は未了）

1. 消費異常の主因は「1 read = 1 往復 = 全履歴再送」を ~65〜87 回
   繰り返す**行動側**にある。増分/往復は 2.2〜3.9k tok で、個々の
   往復が異常に太いわけではない。
2. 行動の内訳は (a) バッチング不使用（affordance 有効でも）、
   (b) 編集の先送りと検証の不在（収束しない）、(c) 手戻りの往復化。
   いずれもモデル（Kimi-K2.7-Code）の trajectory 特性の可能性があり、
   ハーネス起因分との分離には同モデル・別ハーネス（opencode）の
   同一タスク比較が要る（ベースライン整備済み・実行は別途判断）。
3. explore は「報告が read を代替する」効果が現状未実証。cap 死時の
   全喪失は先行実装水準（強制要約で部分成果返却）への修正が確定的に
   有効。
