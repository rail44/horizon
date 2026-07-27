# Compaction — 二層の文脈削減（Tier 1: 復元可能な clearing / Tier 2: 状態要約）

Status: designed 2026-07-28（オーナー承認）。**Tier 1 は 2026-07-28 実装
済み**（下記「Tier 1 実装記録」）。Tier 2 は未実装 — 計測後に着手（証拠の
順序に忠実な段階導入）。

証拠基盤: `docs/research/agent-compaction-prior-art-2026-07-28.md`（本
設計の全判断の出典。以下「証拠 doc」）と
`docs/research/agent-context-memory-separation-2026-07-20.md`（axis A/B
の原設計と 2026-07-25 撤去の決定記録 — 撤去理由は「素の消費問題より
先に lossy 変換を入れる順序が誤り」であり、その前提は 2026-07-26〜27
の消費削減campaign（autopsy doc 追補 1〜4）で消化済み）。

## 動機

供給側（委譲・バッチング・max_tokens・routing）を測り切って直した後も、
T-callid 級のタスクは有効窓 229k を使い切って死ぬ（追補 4）。残る手段は
「窓に入れる量を減らす」か「窓を超えて生存する」で、本設計は両方を
証拠に基づく順序で提供する。さらに Recovery-Bench（汚染文脈で相対
−57%）により、刈り込みは生存機能であると同時に品質機能である。

## Tier 1 — 復元可能な機械的 clearing（一次・主力）

古い tool 結果の**本文だけ**を参照 placeholder に置換する。

- **projection 方式**: event log・DuckDB は無傷。provider へ送る
  rig_history の組み立て時にのみ適用（撤去された seam の再導入 —
  2026-07-25 記録に「1 call site の 1 関数」と明記）。
- placeholder は「ツール名・引数の要点・元サイズ・再取得手段
  （recall.read の範囲 / 再実行）」を含む 1 行。**tool call と call_id
  のペアは保持**（ペア分断は provider 400 の既知源）。
- **保護**: (1) user メッセージと brief は構造的に対象外（tool 結果
  しか触らない — instruction eviction を構成的に不可能にする）、
  (2) 直近 tail は実測トークン予算（初期値 16k）で原文維持、
  (3) 未回収の tool call batch は跨がない、(4) TaskNotification は
  対象外。ツール別の恒久例外は v1 では設けない。
- **発火**: 実測 input（provider_request_usage）が有効窓の 60% 超過、
  かつ回収可能量が floor（初期値 16k トークン相当）以上の時だけ一括
  実行（cache 全損を「まれに一度」に留める。OpenCode の
  PRUNE_MINIMUM と同思想）。
- 有効窓 = `/models` 申告の context_length − 送信中の max_tokens。
  sessiond がセッション開始時に取得・キャッシュし、取得不能なら
  **発火しない**（crush の cw==0 保護）+ 保守的既定 128k で警告。
- 透明性: 何をどの範囲 clear したかを専用イベントとして event log に
  記録し、transcript に区切りを表示。
- 計測スイッチ: 閾値の env-only 強制（LangChain 方式の強制発火計測用。
  file config には載せない）。

根拠（証拠 doc §1・§3）: 統制比較で LLM 要約と同等（2508.21433）、
Anthropic の公表実測は clearing+memory 側のみ、Manus「不可逆圧縮は
論理的にリスク」、Claude Code の公式実行順（clearing → 要約）。
Horizon 固有の強み: 消した本文は recall で**実際に**再取得できる
（event log が正本のまま残るため）。

## Tier 1 実装記録（2026-07-28）

実装コード（すべて `crates/horizon-agent`）:

- `providers/rig/clearing.rs` — pass の決定（`plan_clearing_pass`）、
  provider view の seam（`history_for_provider_request`）、セッション状態
  （`ClearingState`）、resume 再生（`cleared_call_ids_from_events`）。
- `providers/rig/model_limits.rs` — `GET {base_url}/models` を
  `(base_url, model)` ごとにプロセス内 1 回だけ取得しキャッシュ（失敗も
  キャッシュ）。
- 呼び出し点は `providers/rig/completion.rs` の `complete_rig_turn` の
  1 箇所のみ（2026-07-25 の撤去記録が「1 call site の 1 関数」と述べた
  とおり）。

確定した定数（`config.rs`、いずれも初期値）:

| 定数 | 実装値 |
|---|---|
| `DEFAULT_CLEARING_TRIGGER_PCT` | 60 |
| `CLEARING_RECOVERY_FLOOR_TOKENS` | 16,384 |
| `CLEARING_TAIL_BUDGET_TOKENS` | 16,384 |
| `CLEARING_CHARS_PER_TOKEN` | 4 |
| 有効窓 | `context_length − 32,768`（送信中の `max_tokens`） |

設計からの確定事項・差分:

- **fallback 有効窓 128k は採らなかった。** 設計表の「fallback 有効窓
  128k」は「発火なし + 警告」と併記されていたが、発火しない以上その数値
  は何にも使われない。実装は限界不明なら窓を持たず（`None`）、
  セッション開始時に stderr へ 1 行出すだけ。推測した窓を根拠に履歴を
  削ることは起きない。
- **cleared set は凍結**。pass は 1 回だけ集合を決め、
  `Event::HistoryCleared`（`cleared_call_ids` + `recovered_chars`）として
  event log に載る。以後のリクエストは同じ集合を適用するので projection は
  バイト一致し、cache 損は pass ごとに 1 回。**リクエストごとの再計算は
  却下設計**（seam の doc comment に明記）。
- **保護は構造で担保**: 触るのは tool result の content のみ（user /
  assistant / TaskNotification は型として対象外）、tail 予算まで遡って
  停止、最後に tool call を要求した assistant メッセージ＝「今の往復」は
  丸ごと除外（未回収バッチを半端に消せない）。ツール別例外は無し。
- **placeholder** は 1 行:
  `[cleared old tool result: fs.read path="…" (12345 chars). The full
  result is retained in the session event log — use recall.search /
  recall.read, or re-run the tool.]`
- **計測スイッチ**: `HORIZON_AGENT_CLEARING_THRESHOLD_PCT`（env only、
  1..=100 にクランプ、範囲外・非数値は既定値）。config file には載せない。
- **wire 影響**: `Event` に variant を 1 つ追加（`Unknown` の手前）した
  ため、v14 の `MessageRole::TaskNotification` と同じ理由で
  `SESSION_PROTOCOL_VERSION` を 14 → 15。`MIN_SUPPORTED_PROTOCOL_VERSION`
  は 11 のまま（旧 peer は `Unknown` として読み飛ばし、失うのは transcript
  の区切り表示だけ）。
- **transcript**: `AgentFrameItem::HistoryCleared` として区切り行を描画
  （"cleared N old tool result(s) (~X chars) — recoverable via recall"）。
  burst はこの行で閉じる — burst 範囲は 1 行の receipt に畳まれ内部の item
  は個別描画されないため、吸収させると唯一の痕跡が消える。
- **turn 規約**: pass は provider round の直前（`complete_rig_turn` の
  入口）でのみ走る。`WaitingForApproval` 中はリクエストを組み立てないので
  構造的に走らず、`Event::TurnEnded` は不変。

未実施: 実測（T-callid 再走。死亡点の移動、発火回数・回収量、cache 損、
recall 再取得率）。Tier 2 の着手判断はこの計測の後。

## Tier 2 — LLM 状態要約（最終段・生存保証）

Tier 1 適用後もなお 80% を超える場合のみ。

- **同一モデル・同一 system prompt・同一 prefix で要約リクエストを
  発行**（cache read で安価に。別 prompt/モデルは全履歴を非 cache
  単価で払う — Anthropic 明文。Letta の「安価な別モデル」方式は
  採らない）。
- 構造化テンプレート（goal / 決定 / 完了・進行・詰まり / 次の一手 /
  関係ファイルとシンボル・エラーの逐語転記）+ **iterative 更新**
  （既存要約があれば「更新」を指示。再要約の重ね掛けは Codex 実測の
  13.7%→6.9% 崩壊経路）。
- **要約は畳んだ生ログの往復範囲への参照を必ず含み、詳細は recall で
  再取得可能**と本文中に明示する。この「参照付き要約 + fetch 可能な
  生ログ」は field が収斂しながら比較測定ゼロの形（証拠 doc §5）—
  Horizon が最初に測る。
- 組み立て: `[brief 原文（pinning）][要約][直近 tail 原文][新規往復…]`。
  user メッセージは要約対象から除外し原文系で保つ。
- **防御（Gemini の failure カタログ輸入）**: 要約が元より膨張したら
  破棄、失敗したらラッチして以後は Tier 1 のみ、要約自体の発行が
  window に入らない事態を防ぐため発火点は Tier 1 より十分手前。

## 閾値と数値（すべて初期値・無根拠が field の実態。計測で調整）

| 定数 | 初期値 | 備考 |
|---|---|---|
| Tier 1 発火 | 有効窓の 60% | + 回収 floor 16k |
| Tier 2 発火 | 有効窓の 80% | Tier 1 後の実測で再判定 |
| tail 保護予算 | 16k トークン | 往復単位で遡る（turn 非依存） |
| 要約上限 | 8k トークン | |
| fallback 有効窓 | 128k | /models 不能時、発火なし + 警告 |

## Letta の示唆との差分（オーナー確認済み、2026-07-28）

原則面（正本・参照・検索と刈り込みの併用・読み取り専用保護領域）は
letta.md の survey に準拠。実行面は測定に従い乖離する:

1. 要約は安価な別モデルでなく**同一モデル + prefix 共有**（cache 明文
   + 定額 provider という事情）
2. **復元可能な機械層（Tier 1）は Letta に無い層** — masking 論文と
   Anthropic 実測に基づく追加
3. ページングの主導権は**エージェントでなくハーネス**（Cognition の
   自己要約不十分の実測、Letta 自身の red-teaming が根拠）
4. **エージェント編集の可変メモリブロック（core memory / todo）は
   見送り** — 証拠が割れている（Manus の todo 撤回）ことに加え、
   これは DuckDB 採用の狙い（ナレッジベース・archival 層構想）と
   一体で設計すべき領域のため、そちらの設計回に送る（オーナー方針）
5. archival 層・sleep-time compute もスコープ外（sleep-time は async
   task の購読配管が将来の土台になる）

## 実装順と計測

1. **Tier 1 のみ実装** → T-callid 再走で計測: 死亡点の移動（または
   完走）、clearing 発火回数・回収量、cache 損の実測、recall 再取得の
   発生率
2. 結果を見て Tier 2 実装（発火が観測できる規模のタスクで再計測。
   強制発火 env で反復 compaction の品質推移も測る）
3. turn 規約との整合: clearing/要約はいずれも往復間で実行、
   `Event::TurnEnded` 境界は不変、WaitingForApproval 中は実行しない
