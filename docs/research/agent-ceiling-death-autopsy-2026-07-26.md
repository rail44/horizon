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
- **訂正（2026-07-27 監査、下の追補 3 参照）**: 「Horizon が
  `max_tokens=65,536` を毎回要求している」は誤り — Horizon は
  `max_tokens` を**一切送っていない**（未設定・省略、rig-core 0.39 の
  シリアライズは `Option::is_none` で丸ごと省く）。196.6k という値
  自体は無関係ではなく、synthetic.new 自身の `GET /openai/v1/models`
  が `hf:moonshotai/Kimi-K2.7-Code` に対し `context_length: 262144` /
  `max_output_length: 65536` を返す（2026-07-27 に実叩き確認、
  `262144 - 65536 = 196608` が実測死亡点 195.4k〜196.5k とほぼ一致）。
  **検証済み**なのはこの申告値の一致だけで、「`max_tokens` 省略時に
  バックエンドがこの申告最大出力を丸ごと予約する」はそこからの
  **推測**（synthetic.new のドキュメントに明記なし）。opencode が
  同じ窓で ~218k まで入力を伸ばせた差についても、opencode 側が実際に
  何を送っているかは未確認 — 同じ推測に立てば辻褄は合うが、それ以上
  の主張はしない。
- 結論: **消費病理はハーネスではなくモデル（Kimi-K2.7-Code）支配**。
  2×2 の残り 1 セル（同一ハーネス・別モデル）が次の計測。

## 追補 2（2026-07-27）: 同一ハーネス・別モデル（MiniMax-M3）

Horizon（同 commit の worktree、fork seed env）で同一 brief を 1 走
（オーナー指定 `hf:MiniMaxAI/MiniMax-M3`）。

- 結果: **未完走・provider 400 @ 196,280**（`262144 - 65536 = 196608`
  に近い、Kimi と同型の死亡点 — ただし「Horizon が `max_tokens=65,536`
  を要求している」わけではない。追補 1 の訂正・下の追補 3 を参照。
  synthetic.new は `hf:MiniMaxAI/MiniMax-M3` にも同じ
  `context_length: 262144` / `max_output_length: 65536` を申告して
  いる、2026-07-27 に実叩き確認済み）。
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

## 追補 3（2026-07-27）: `max_tokens` 監査と修正

追補 1・2 の「`max_tokens=65,536` を毎回要求する自縛」という記述は
誤りだった（両方訂正済み、上記参照）。誤りの実体を監査し、
`crates/horizon-agent` に修正を入れた記録。実装差分は本 doc の範囲外
（コード側は `crates/horizon-agent/src/config.rs`・
`crates/horizon-agent/src/providers/rig/completion.rs` の diff を参照）。

### 監査結果 — Horizon が実際に送っている補完パラメータ

`crates/horizon-agent/src/providers/rig/completion.rs`
`rig_openai_turn_streaming` の builder 呼び出しチェーン
（`model.completion_request(prompt).messages(..).tools(..)
.preamble(..).additional_params(..).stream()`、監査時点＝修正前）と
rig-core 0.39.0（`~/.cargo/registry/src/*/rig-core-0.39.0/`）のソースを
突き合わせた結果:

| パラメータ | 送信状態（監査時点＝修正前） | 根拠 |
|---|---|---|
| `temperature` | **未送信**（`Option<f64>` 常に `None`） | Horizon 側で `.temperature(..)` 呼び出しなし。rig-core 側 `providers/openai/completion/mod.rs` の `CompletionRequest.temperature` は `#[serde(skip_serializing_if = "Option::is_none")]` — `None` は JSON から丸ごと省略される（rig 自身のテスト `test_max_tokens_omitted_when_none` と同型の温度版で確認可能な構造）。 |
| `top_p` | **未送信、かつ rig-core の OpenAI Chat Completions リクエスト型にそもそもフィールドが存在しない** | `top_p` は rig-core の `providers/openai/completion/mod.rs`（Chat Completions 用 `CompletionRequest`）にフィールド無し。`top_p` を持つのは Gemini・`responses_api`（別エンドポイント）のみ。Horizon が使うのは Chat Completions 経路なので、`additional_params` に手動で詰めない限り送信経路自体が無い。 |
| `max_tokens` | **未送信**（`Option<u64>` 常に `None`） — **本 audit で修正、下記参照** | 温度と同型: `CompletionRequest.max_tokens` は `#[serde(skip_serializing_if = "Option::is_none")]`。rig-core 自身のテストが対称に存在: `test_max_tokens_is_forwarded_to_request`（`Some(4096)` → `serialized["max_tokens"] == 4096`）と `test_max_tokens_omitted_when_none`（`None` → `serialized.get("max_tokens").is_none()`）。 |
| `parallel_tool_calls` | **送信**（`true` 固定） | `openai_turn_additional_params()` が `additional_params: {"parallel_tool_calls": true}` を明示送信（監査前から既存、変更なし）。 |
| `tool_choice` | 未送信（`None`） | Horizon 側で `.tool_choice(..)` 呼び出しなし → rig-core・OpenAI 双方の既定（通常 `"auto"`）に委ねる。挙動として問題は観測されていないため今回は変更しない。 |

rig-core の builder API には `.max_tokens(u64)` / `.max_tokens_opt(Option<u64>)`・
`.temperature(f64)` / `.temperature_opt(Option<f64>)` が第一級 setter として
存在する（`rig-core-0.39.0/src/completion/request.rs`）。`additional_params`
経由の回避策は不要 — 単に呼んでいなかっただけ。

### synthetic.new 側の申告値（2026-07-27、実 API 実行で確認）

`GET https://api.synthetic.new/openai/v1/models`（認証不要、2026-07-27 に
直接叩いて確認）が返す JSON は、モデルごとに `context_length` と
`max_output_length` を含む。今回のキャンペーンで使った 2 モデルの該当
エントリ:

```json
// hf:moonshotai/Kimi-K2.7-Code
{ "context_length": 262144, "max_output_length": 65536,
  "supported_sampling_parameters": ["temperature","top_k","top_p",
    "repetition_penalty","frequency_penalty","presence_penalty","stop","seed"] }

// hf:MiniMaxAI/MiniMax-M3
{ "context_length": 262144, "max_output_length": 65536,
  "supported_sampling_parameters": ["temperature","top_k","top_p",
    "repetition_penalty","frequency_penalty","presence_penalty","stop","seed"] }
```

`262144 - 65536 = 196608`。実測死亡点（fork 196,500 / fresh-61 196,435 /
fresh-70 195,360 / opencode-baseline は別モデル扱いのため対象外 /
M3 196,280）はいずれもこの値の 1% 未満の差に収まる。**検証済み**なのは
この申告値と実測死亡点の近さそのもの。「`max_tokens` 省略時にバック
エンドがこの申告最大出力を丸ごと予約している」はそこからの**推測**で、
synthetic.new の `/chat/completions` ドキュメント（後述）はこの挙動を
明記していない。

synthetic.new の `/docs/openai/chat-completions` ページ（2026-07-27 access）
は `max_tokens` パラメータを次のようにしか説明していない（HTML から抽出
した verbatim）:

> `max_tokens` | `number` | optional | "Maximum number of tokens to
> generate"

省略時のデフォルト挙動についての記載はこのページ・`/docs/api/overview`・
`/docs/api/models` のいずれにも見当たらなかった。

### ベンダー推奨サンプリングパラメータ（引用、2026-07-27 access）

**Kimi K2.7 Code** — Hugging Face モデルカード
(`https://huggingface.co/moonshotai/Kimi-K2.7-Code/raw/main/README.md`,
"For third-party APIs deployed with vLLM or SGLang" の節、synthetic.new は
まさにこの third-party vLLM/SGLang 系ホストに該当):

> \- The recommended `temperature` will be `1.0` for Thinking mode.
>
> \- The recommended `top_p` is `0.95`.

同カードの Context Length 表記: `| **Context Length** | 256K |`
（= 262,144、synthetic.new の申告と一致）。

対照として Moonshot 自身の公式ホスト API ドキュメント
(`https://platform.kimi.ai/docs/guide/kimi-k2-7-code-quickstart`,
2026-07-27 access, HTML から抽出) はより強い文言を使う
（**Moonshot 自身のエンドポイント限定の話** — synthetic.new のような
third-party ホストがこの検証を再実装しているという証拠は無い）:

> temperature: "... fixed value 1.0. Any other value will result in an
> error"
>
> top_p: "... fixed value 0.95. Any other value will result in an error"
>
> max_tokens: "The maximum number of tokens to generate for the chat
> completion." ... "Default to be 32k aka 32768"

**MiniMax M3** — Hugging Face モデルカード
(`https://huggingface.co/MiniMaxAI/MiniMax-M3/raw/main/README.md`,
2026-07-27 access, verbatim):

> We recommend the following parameters for best performance:
> `temperature=1.0`, `top_p=0.95`.

### 決定 — `temperature`/`top_p` は変更しない、`max_tokens` のみ追加

- **`max_tokens`**: `crates/horizon-agent/src/config.rs` に
  `DEFAULT_AGENT_MAX_OUTPUT_TOKENS: u64 = 32_768` を追加し、
  `RigAgentConfig::max_output_tokens` として `rig_openai_turn_streaming`
  の builder チェーンに `.max_tokens(config.max_output_tokens)` を追加。
  32,768 の根拠: 本キャンペーンの実測往復あたり出力は稀な大きい
  file-write を除き概ね 3k トークン以下で、32,768 は単一往復に十分な
  余裕を残しつつ、未使用のまま予約されていた 65,536 のうち約半分
  （~33k トークン）を input 側に回収する。副次的に、Moonshot 自身の
  公式 API がこの同一モデルに対して文書化しているデフォルト値
  （"32k aka 32768"）と一致した — ただし Horizon が話しているのは
  synthetic.new（third-party ホスト）であって Moonshot 自身の
  エンドポイントではないため、この一致は補強材料であって
  synthetic.new の実挙動の保証ではない。
- **`temperature`/`top_p`**: 変更なし・意図的に未設定のまま維持。
  理由: (1) 設定サーフェスは 2026-07-18 の narrowing 決定で凍結済み
  — 新しい config ファイルキーは追加しない。(2) vendor 側の文言は
  third-party ホストに対しては "recommended"（1.0/0.95）であって
  "required" ではなく、一般に OpenAI 互換バックエンドの `temperature`
  既定値は 1.0 であることが多いため、未設定のままでも vendor 推奨値と
  概ね一致している可能性が高い。(3) 明示値を焼き込むことで、
  synthetic.new が将来 Moonshot 自身の「固定値以外はエラー」制約を
  再現するようになった場合の破損リスクをわざわざ作り込む理由がない
  — 何も送らなければこのリスクは原理的に存在しない。

### 監査で見つかった副産物

- `docs/research/agent-context-memory-separation-2026-07-20.md`（582行目
  付近）に、synthetic.new の `/models` が `max_output_length` を返す
  ことが既に 2026-07-20 時点で記録されていた（axis A の実装根拠として）。
  今回の 65,536 の出どころはこの既存記録と一致する — 新規発見ではなく
  再確認。
- テスト: `crates/horizon-agent/src/config.rs`
  `rig_agent_config_falls_back_to_built_in_defaults_when_provider_values_are_none`
  に `max_output_tokens` の既定値アサートを追加。
  `crates/horizon-agent/src/providers/rig/tests.rs` に
  `openai_turn_completion_request_carries_the_explicit_max_tokens` を
  新設 — rig-core の実 builder chain（ネットワーク I/O なしで構築可能）
  を通して `max_tokens` が実際に request に乗ることを検証。
