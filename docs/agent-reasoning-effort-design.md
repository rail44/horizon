# エージェントの推論 effort

会話モデル(`default_model` とセッションごとの切り替え)に、推論の量
(effort)を指定できるようにする。プロバイダごとの書き方の違いは rig-core
(0.46)の仕組みに任せ、Horizon は「どの値をどこに持つか」と「どう選ばせるか」
だけを持つ。

作業は二段に分かれる。前段は接続とモデル情報を rig に寄せる部分、後段は
effort そのもの。

## 前段: 接続とモデル情報(実装済み)

- **`vendor`**: `[[providers]]` の entry は rig の vendor
  (`rig_core::providers::registry::ProviderId`)として接続する
  (`crates/horizon-agent/src/config/vendor.rs`、書き方は
  `docs/provider-configuration.md`)。vendor が dialect を決め、dialect が
  生成オプションの書き方と、受け付けないオプションの拒否を決める。rig に
  vendor の無いエンドポイント(DeepInfra、synthetic.new)は `vendor = "openai"`
  と `base_url` で接続する。
- **接続**: vendor の preset(`ProviderId::config`)に鍵と `base_url` を与え、
  `ProviderConfig::completion_model(model, rig_reqwest::shared(), facts)` で
  ターンのモデルを作る(`providers/rig/completion.rs`)。経路は dialect が
  モデルに選ぶもの(OpenAI 本体は Responses。OpenAI の新しいモデルは、
  推論中は chat completions でツールを受け付けない)。`vendor = "openai"` で
  `base_url` を書いた entry(rig に dialect の無い互換 gateway)だけは
  chat completions に固定する。タイトルとジャッジはどの vendor でも
  chat completions(ジャッジは chat の応答の logprobs を読む)
  (`Vendor::connection` / `Vendor::chat_connection`)。
- **モデル情報**: rig の組み込み catalog に、取ってきた models.dev の写しを
  `Catalog::with_models_dev` で重ねたもの(`providers/rig/model_catalog.rs`)。
  vendor とモデル ID で引く。ターンの `facts` と、Tier 1 clearing の
  context window(`/models` の申告が無いとき)の両方に使う。
- **生成オプション**: `CompletionRequest::options(GenerationOptions)`。前段で
  載せているのは `parallel_tool_calls` だけで、OpenAI 自身の vendor(=rig に
  dialect の無い gateway を含む)にだけ送る。rig は Ollama の dialect では
  これを拒否し、既定の `OnUnsupported::Error` ではリクエストが失敗するため、
  dialect のある vendor ではプロバイダの既定に任せる。

## 計測: DeepInfra の `reasoning_effort`(2026-11-23)

`vendor = "openai"` + `base_url` で接続した entry に対し、rig は OpenAI の
書き方(`reasoning_effort: "<level>"`)で送り、OpenAI の catalog に無いモデル
については値を検査しない(`rig-core-0.46.0/src/providers/openai/options.rs`
の `openai_chat`)。それが DeepInfra で効くかを、
`deepseek-ai/DeepSeek-V4.1-Flash-1017` に直接リクエストして確かめた。

| 送ったもの | 結果 |
|---|---|
| 何も送らない | 200、推論トークン 0 |
| `none` / `thinking: {type: "disabled"}` | 200、推論トークン 0 |
| `minimal`〜`max` の 6 段 | 200、推論あり |
| `bogus` | 422、`Input should be 'none', 'minimal', 'low', 'medium', 'high', 'xhigh' or 'max'` |

難しい問題で各 3 回: `low` 4,821〜5,542、`high` 6,108〜8,659、`max`
8,990〜16,000(`max_tokens` 16,000 に達した回を含む)推論トークン。
DeepInfra は rig の `Effort` の 6 段と `none` をそのまま受け付け、段で量が
変わる。また、このモデルは何も送らないと推論しない。

## 後段: effort(実装済み)

- **値**: `ReasoningLevel`(`none`、`minimal`、`low`、`medium`、`high`、
  `xhigh`、`max`。`crates/horizon-agent/src/config/reasoning.rs`)。送るときは
  rig の `Reasoning::Off` / `Reasoning::Effort(..)` にする。`Reasoning::Budget`
  は出さない。
- **既定値**: `default_model = { provider, model, effort = "high" }` の
  `effort`。書かれていなければ `medium`。
- **選択肢**: catalog の行の `reasoning` から。`Listed` なら列挙された段に、
  推論を切れるなら `none` を足したもの、`None`(推論しない)なら無し、
  catalog がモデルを知らないか `Unknown` なら 7 段すべて。budget しか取らない
  モデル(Anthropic の Haiku 4.5 など)は段を持たないので、選べるのは
  `none` だけになる。
- **送り方**: セッションが望む段を、モデルの選択肢に合わせて送る
  (`ReasoningLevel::fit`: 含まれていればそのまま、無ければその下で一番高い段、
  それも無ければ一番低い段)。推論しないモデルには何も送らない。推論する
  モデルには常に送る(上の計測のとおり、送らないことの意味はモデルと
  プロバイダで変わり、画面から読めない)。望む段はモデルを切り替えても
  変わらない。rig が送れないと判断した値はターンの失敗になる。
- **持ち方**: rig のセッションが、開始時・モデル切り替え後・effort 切り替え後に
  今の段と選択肢を知らせ(`ProviderEvent::SessionReasoning`)、`horizon-agent-host` が
  セッションごとに最後の知らせを持って再接続のたびに送り直す。
- **ACP**: `session/set_config_option` の 2 つ目の設定項目、id `effort`
  (`horizon_agent_host::options::EFFORT_CONFIG_ID`、カテゴリ `thought_level`)。
  値は段の語、項目はモデルの選択肢。モデルが受け付けない段への切り替えは
  要求の時点で拒む。切り替えはモデルと同じく次のターンから効く。effort を
  知らないシェルはその項目を読み飛ばし、知らないエージェントは切り替えを
  `Unknown config option` で拒む。
- **ジャッジと要約**: `crate::auxiliary::reasoning_off`(`Reasoning::Off` を
  受け付けられなければ付けずに送る)。
- **画面と操作**: composer のチップは `model · effort`。モデルピッカーの最初の
  段の先頭に `Effort` の行があり、選ぶと段の一覧になる。パレットの
  `Switch Effort…` はピッカーをその一覧から開く。CLI は
  `horizon set-effort <session-id> <level>`。

## `max_tokens`

全ターンに `DEFAULT_AGENT_MAX_OUTPUT_TOKENS`(32,768)を送り、Tier 1 の
有効窓を `context − 32,768` とする(`crates/horizon-agent/src/config.rs`)。
effort を入れた後、`max` のターンが上限で打ち切られるか(打ち切りは
`Truncation` として検出され、画面に出る)を見て見直す。

catalog が知るモデルの最大出力は送らない。catalog の最大出力が context と
同じ値のモデルがあるためで、rig の組み込み catalog の Ollama の行(抜粋):

| モデル | context | 最大出力 |
|---|---|---|
| `kimi-k2.6` | 262,144 | 262,144 |
| `kimi-k2.7-code` | 262,144 | 262,144 |
| `deepseek-v4-pro` | 1,048,576 | 1,048,576 |
| `minimax-m2.7` | 196,608 | 196,608 |
| `deepseek-v4.1-flash` | 1,048,576 | 384,000 |
| `gpt-oss:120b` | 131,072 | 32,768 |

このまま送ると有効窓が 0(clearing が働かない)になり、入力 + `max_tokens`
が context を超えるリクエストを拒むバックエンドでは最初のターンから失敗
しうる。DeepInfra の `/models` も `max_tokens` を context と同じ値で申告する。
