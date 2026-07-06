# エージェント・プロンプティング調査 — 役割つきエージェントとスキル機構への布石

Horizon のエージェントのシステムプロンプト(`crates/horizon-agent/src/prompt.rs`)
は意図的に薄い。この文書は「その薄さは妥当か」をベストプラクティス調査で
検証し(Part 1)、「役割(role)つきエージェント定義」を将来足すときに現行
コードの何が邪魔になるかをコードリーディングで特定し(Part 2)、Claude
Code の Skills を最小の先行事例として「段階開示」機構の最小形を1案だけ描
く(Part 3)。**この文書は推奨を記録するのみで、実装はしない。**

`docs/agent-tools-design.md` の "System Prompt" 節がすでに「過剰処方は新
しいモデルに実測で害をなす」という原則を明言し、Anthropic のエンジニアリ
ング記事・SWE-agent 論文・OpenAI のエージェントガイドを Key Sources に挙
げている。本調査はその節を追認しつつ、(a) システムプロンプトの中身その
もの、(b) 現用モデル Kimi K2 固有の癖、(c) 役割定義とスキル機構という、
同ドキュメントが扱っていない3点を掘り下げる。

---

## Part 1: プロンプトのベストプラクティス調査

### 1.1 Anthropic の公式ガイダンス

**実証・公式。** [Building Effective AI Agents](https://www.anthropic.com/research/building-effective-agents)
は「洗練されたフレームワークより単純で組み合わせ可能なパターンの方がう
まくいく」を出発点の原則とし、「まず基本的なプロンプトから始め、単純な
アプローチで不十分だとわかってから多段のシステムへ拡張する」ことを推奨
する。成功するエージェント実装の3原則(設計の単純さ/透明な計画ステッ
プ/入念に文書化されたツールインターフェース)のうち**単純さを最初に置
く**。[Effective Context Engineering for AI Agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
は過剰に硬直した if-then ロジックが「脆くなる」「保守コストを増やし続け
る」と明言し、「硬直した if-then」と「行動を導けない曖昧な指示」の両極
端を避けるべきとする。「コンテキストは貴重で有限な資源として扱い、各推
論ステップで戦略的に取捨選択する」という just-in-time 方針も明言してお
り、これは環境ブロックを最小限にする Horizon の設計、および Part 3 のス
キル機構を裏付ける。[Writing Effective Tools for AI Agents](https://www.anthropic.com/engineering/writing-tools-for-agents)
も同じ思想で、「肥大化したツールセット」「どのツールを使うべきか曖昧な
判断点」を最もよくある失敗モードと明言する(`docs/agent-tools-design.md`
は既にこれを踏まえてツール数を絞っている)。

**結論。** Horizon の「アイデンティティ1行+環境ブロック+ツール方針3
行、手順の処方なし」という形は、なんとなく薄いのではなく Anthropic の一
次資料と整合した選択と言える。

**参考(第三者分析、非公式)。** [How Claude Code Builds a System Prompt](https://www.dbreunig.com/2026/04/04/how-claude-code-builds-a-system-prompt.html)
は Claude Code の実プロンプトを「モデルに考え方を教える場ではなく作業環
境をセットアップする場」と分析する——事実関係(断片数など)は参考程度
に留めるべきだが、位置づけ自体は Horizon の環境ブロックの意図と合う。

### 1.2 OpenAI: モデル世代とプロンプト処方量

**実証・公式、ただし「短ければ良い」ではない。** [GPT-5 prompting guide](https://developers.openai.com/cookbook/examples/gpt-5/gpt-5_prompting_guide)
は「GPT-5 は指示に外科的な精度で従う」性質を明言した上で、これを諸刃の
剣とする——「矛盾した/曖昧な指示を含む拙いプロンプトは、他のモデルより
GPT-5 に対してより有害になりうる」。メカニズムとして「(旧モデルのよう
に矛盾を無視するのではなく)矛盾を解決しようと推論トークンを浪費する」
ことを挙げる。[GPT-5.1 guide](https://developers.openai.com/cookbook/examples/gpt-5/gpt-5-1_prompting_guide)
・[troubleshooting guide](https://developers.openai.com/cookbook/examples/gpt-5/gpt-5_troubleshooting_guide)
も同旨で、「プロンプトの別々の節どうしの矛盾」がエージェントの過剰な確
認・迷走の原因になるケースを挙げる。

実例として、Cursor が公開した知見(OpenAI のガイドが引用)では「徹底的
に情報収集しろ」という指示が旧モデルには有効だったが、GPT-5 には「もと
もと自発的に文脈収集する性質」と衝突してツール呼び出しの繰り返しを誘発
し、**削除した方が性能が上がった**。ただし別の医療スケジューリング例で
は矛盾したルール自体の解消が解であり、単純な短縮が処方箋ではない。
OpenAI 自身の [prompt optimizer cookbook](https://developers.openai.com/cookbook/examples/gpt-5/prompt-optimization-cookbook)
のコーディング事例では、曖昧な許可(分岐を生む言い回し)を**具体的な制
約に置き換えて増やす**ことで遵守スコアが改善しており、
[Codex 自身のシステムプロンプトテンプレート](https://developers.openai.com/cookbook/examples/gpt-5/codex_prompting_guide)
も決して短くはない。GPT-5 は `verbosity` パラメータや自発性を上下させる
"eagerness" 制御(`reasoning_effort` と組み合わせ)を新設しており、これ
は「プロンプト文面を書き足して調整する」代わりに**専用のノブを用意する**
方向への転換で、過剰処方の害を制度的に認めた上での対策と読める。
[Codex best practices](https://developers.openai.com/codex/learn/best-practices)
は「恒久的なルールを AGENTS.md やスキルに逃さず、プロンプトに溜め込む」
ことを名指しでアンチパターンとし、AGENTS.md を「チームの作業方針をエー
ジェントに伝える最良の場所」と位置づける。

**結論。** OpenAI 自身の主張は「短ければ良い」ではなく、「**矛盾・曖昧
さの除去**」と「**恒久的なルールは設定ファイル(AGENTS.md)に逃がし、
プロンプトで繰り返さない**」の2点に集約される。後者は Part 1.5 のリポ
ジトリ指示ファイル論とも直結する。「新しいモデルほど短いプロンプトが良
い」という定性的な言説自体は([Sean Goedecke: Prompts are technical debt too](https://www.seangoedecke.com/prompts-are-technical-debt-too/)
などに見られる)コミュニティの通説であり、OpenAI 自身の断定ではない。

### 1.3 Kimi K2 系のプロンプト指針(最重要・現用モデル固有)

事前予想に反し、Moonshot 公式(モデルカード・クイックスタート)に Horizon
の実装へ直結する具体的な記述が複数見つかった。現行モデルは
"Kimi K2.7-Code"([HF モデルカード](https://huggingface.co/moonshotai/Kimi-K2.7-Code)、
[公式クイックスタート](https://platform.kimi.ai/docs/guide/kimi-k2-7-code-quickstart))。

**実証・公式、Horizon の設計と直接関係するもの。**

1. **システムプロンプトにツールの使い方を書くな、と明言されている。**
   K2.6 向け公式エージェント構築ガイドは「System Prompt にツールやその
   使い方を明記する必要は無く、むしろ Kimi K2.6 の自律的な意思決定を妨
   げうる」とし、プロンプトには role/業務文脈/出力フォーマット/制約の
   みを書くよう推奨する([platform.kimi.ai: use-kimi-k2-to-setup-agent](https://platform.kimi.ai/docs/guide/use-kimi-k2-to-setup-agent))。
   Horizon の「Tool policy」節(3行、絶対パス必須/検索ツール優先/失敗
   時のリトライ)はツールの説明そのものではなく利用方針だが、この公式
   警告が指す「ツールの使い方の明記」に近い領域ではあり、効果があるか
   どうか検証の価値がある論点として記録しておく。
2. **`reasoning_content` を消してはいけない、という明言——そして
   Horizon はまさにこれを消している経路がある。** K2.7-Code は
   thinking を常時強制し(無効化不可)、クイックスタートは「マルチステ
   ップのツール呼び出し中、直前ターンの `reasoning_content` を会話履歴
   に保持し続けなければならない。さもないとエラーになる」と明言する。
   Horizon が使う `rig-core`(0.39.0)の OpenAI 互換プロバイダは、進行中
   の1プロセス内でならこれを自動的に処理する(`AssistantContent::Reasoning`
   を `reasoning_content` として往復させる、`providers/openai/completion/mod.rs`
   のコメント自身が「一部のプロバイダはツール呼び出しターンでの echo
   back を要求する」と明記)。**しかし** Horizon 側で履歴をディスクから
   再構築する経路——`crates/horizon-agent/src/providers/rig/mapping.rs`
   の `rig_messages_from_horizon_events`(103-128行)——は
   `Event::ReasoningDelta(_) => None` として reasoning を完全に捨てて
   おり、これは `history.rs::load_rig_history` 経由で**あらゆる rig セ
   ッション開始時**(`agentd` の再起動後の再開、`Reload Agent Runtime`
   後の再接続を含む)に呼ばれる。`docs/agent-duckdb-state-design.md` は
   `provider_payload_json` に "reasoning blocks" を退避する設計意図を既
   に記しているが、実装(`rig_messages_from_horizon_events`)はまだそこ
   を読んでいない。**結果として、ツール呼び出しを含む会話が
   agentd 再起動をまたいで再開されたとき、次のマルチステップツール呼び
   出しターンで Moonshot 側の明言通りのエラーが起きうる、という具体的
   でコードに裏付けられたリスクを今回の調査で見つけた**(直す提案はし
   ない——これは本調査のスコープ外の実装作業)。
3. **サンプリングパラメータが固定。** 公式ドキュメントは
   temperature=1.0, top_p=0.95, n=1, presence/frequency penalty=0.0 を
   既定・実質固定とし、「他の値はエラーを起こす」「デフォルト値の使用
   を推奨」と明言する([migrating-from-openai-to-kimi](https://platform.kimi.ai/docs/guide/migrating-from-openai-to-kimi))。
   Horizon の `RigAgentConfig.temperature`/`max_tokens` は既定 `None`
   (=フィールドを送らない)なので**既定設定では衝突しない**が、
   `config.example.toml` はユーザーが `[provider].temperature` を明示
   設定できることを謳っており、K2.7-Code に向けたままこれを設定すると
   Moonshot 側の制約とぶつかりうる——config の汎用性とモデル固有制約の
   衝突点として記録。
4. **`tool_choice: "required"` 非対応。** `auto`/`none`/`null` のみで、
   「ツール呼び出しを強制したいならプロンプト側で行う」ことが公式に案
   内されている(同 migration doc)。並列ツール呼び出しは公式にサポー
   ト対象。コンテキスト長は 256K(K2.7-Code/K2-Thinking/K2.6/K2.5)。

**コミュニティ・非公式(裏付けは弱いが実害の実例あり)。** vLLM のエン
ジニアリング記事(Moonshot エンジニアの原因究明をクレジット、
[vllm.ai/blog/2025-10-28-kimi-k2-accuracy](https://vllm.ai/blog/2025-10-28-kimi-k2-accuracy))
は K2 のネイティブ chat template が tool_call ID を厳密に
`functions.func_name:idx` 形式で期待し、履歴中の ID が崩れているとパー
サが `IndexError` を起こしたり、モデルがツール呼び出しを幻視する原因に
なると報告する。さらに [MoonshotAI/Kimi-K2 issue #128](https://github.com/MoonshotAI/Kimi-K2/issues/128)
は自前ホスト(vLLM/SGLang)の K2.6 で2ターン目以降のツール呼び出しが高
確率で空応答になる既知バグを報告している(Moonshot 公式ホスト API では
再現しないとのこと)。Horizon の設定コメント(`config.rs` の
`DEFAULT_HISTORY_TOKEN_BUDGET`)がモデル ID を `hf:moonshotai/Kimi-K2.7-Code`
と記していることから、実際のエンドポイントが Moonshot 公式なのか
HF 系のホスティング経由(=vLLM/SGLang 系エンジンの可能性)なのかによっ
て、この既知バグの当否が変わる——**どのバックエンドで動いているかの確
認自体が価値のある次アクション**として記録しておく。

### 1.4 主要コーディングエージェントのシステムプロンプト構造分析

いずれも公式文書ではなく、公開/抽出されたプロンプトの第三者分析が中心
([x1xhlol/system-prompts-and-models-of-ai-tools](https://github.com/x1xhlol/system-prompts-and-models-of-ai-tools)
等)。Cursor(リーク分析)は環境ブロックを本文に持たずトーン指示(「簡
潔に」「繰り返すな」)は明確、テスト規律はリンタ修正の3回リトライ止ま
りと薄い。opencode(非公式分析)は環境ブロック(モデル/cwd/プラットフ
ォーム/日付)を組み立てステップとして持つ。Cline は
`.clinerules` をシステムプロンプトから明示参照すると公式文書化されてお
り([docs.cline.bot](https://docs.cline.bot/customization/cline-rules))、
横断規約 `~/.agents/AGENTS.md` も読む。GitHub Copilot は
`.github/copilot-instructions.md` を全リクエストで読み込み、AGENTS.md
も併用(公式)。Claude Code は公式ドキュメント
([code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory))
によれば**CLAUDE.md はシステムプロンプトの一部ではなく**直後のユーザー
メッセージとして渡され、トーン指示・検証規律(「後戻りできない操作の前
に確認」「フロントエンド変更はブラウザで確認してから完了報告」)は明確
に存在する。

**共通点と Horizon への示唆。** 短い identity+トーン/コミュニケーショ
ン規約+ツール利用規約、という組み合わせはほぼ全エージェントに共通する
de facto standard。環境ブロックはシェルを自身で持つターミナル常駐型
(Claude Code、opencode)に多く、エディタ常駐型(Cursor)には無い——
Horizon はターミナル/エージェントが第一級で対等という方向性上、環境ブ
ロックを持つ側の設計で妥当。**リポジトリ指示ファイルの読み込みは、いず
れのエージェントでもプロンプト本文の"文章"としては実装されておらず**、
別の文脈注入・ツール読み込みの仕組みとして実装される——Part 1.5 で詳
述。

### 1.5 リポジトリ指示ファイルの標準化動向

[agents.md](https://agents.md) は OpenAI 主導で始まり(2025年8月)
Google/Cursor/Factory/Amp 等が参加、現在は Linux Foundation 傘下の
Agentic AI Foundation がスチュワードする規約に育っている。20以上のツー
ルが対応を謳う。読み込み方式は大きく2系統に分かれる: **無条件連結(主
流)**——Codex CLI は起動時にグローバル→プロジェクトルート→cwd までの
各ディレクトリの `AGENTS.md` を階層的に連結し**下位が上位を上書きする**
([developers.openai.com/codex/guides/agents-md](https://developers.openai.com/codex/guides/agents-md))、
Claude Code は CLAUDE.md を cwd から上へ走査し全て**連結**する(上書き
ではない、cwd より下のサブディレクトリ分は実際にそのファイルを読んだ時
に遅延ロード、[code.claude.com/docs/en/memory](https://code.claude.com/docs/en/memory))、
GitHub Copilot・Cursor・Cline も同系統。**能動的/オプトイン(例外)**
——Aider の `CONVENTIONS.md` は `/read` 明示か設定ファイルでの指定が無
い限り自動では読まれない
([aider.chat/docs/usage/conventions.html](https://aider.chat/docs/usage/conventions.html))。

**結論。** 主流は「無条件連結」で、Horizon のように**読み込み経路が皆
無**なのは、今回調査した現役エージェントの中では明確な外れ値。ただし
「無条件連結」は Part 1.1/1.2 の just-in-time 原則(コンテキストを出し
惜しみする)とは逆方向であり、Aider 流のオプトイン(=ツールで能動的に
読む)の方が Horizon の薄さの哲学とは相性が良い——Part 2/Part 3 で改め
て扱う。

---

## Part 2: Horizon 側の構造監査

### 2.1 現状のプロンプト注入経路(1関数・1呼び出し元)

- `system_prompt(environment: &SessionEnvironment) -> String`
  (`prompt.rs:47`)は自由関数で、引数はセッション環境事実
  (`cwd`/`os`/`git_repo`)のみ。role・許可ツール一覧・モデル名など役
  割ごとに変わりうる情報を受け取る余地が構造的に無い。
- 呼び出し元は唯一 `providers/rig/completion.rs::rig_openai_turn_streaming`
  で、`.preamble(system_prompt(environment))` として rig の
  `completion_request` に直接埋め込む(147-153行)。
- `environment` はセッション開始時に一度だけ
  `providers/rig/session.rs::spawn_rig_session` 内で
  `SessionEnvironment::current()` として取得され(43行)、セッションの
  全ターンで使い回される——ターンごと/役割ごとに変える余地は無い。
- ツール定義は別の自由関数 `tools/catalog.rs::definitions()` が全ツール
  の固定リストを返し、`completion.rs::rig_tool_definitions()` が無条件
  にそれを rig の `ToolDefinition` へ変換して `.tools(...)` に渡す
  (462-467行)。セッション/役割単位でツールセットを絞る・足すフィルタ
  機構が存在しない。

### 2.2 config は「1プロセス=1モデル設定」

- `AgentConfig { rig, persistence, tools }`(`config.rs`)は
  `horizon-agentd` プロセス起動時に一度だけ、env + 単一 TOML ファイル
  から構築される(`AgentConfig::from_env_and_file`)。
- これがそのまま `providers::rig::Provider::new(config, ..)` に渡り、
  `Provider` 構造体のフィールドとして焼き込まれる
  (`providers/rig/mod.rs:23-34`)。`Provider::start_session` は常に
  `self.config.clone()` を使い、呼び出しごとに config を差し替える経路
  が無い(`mod.rs:42-48`)。
- `ProviderRegistry` は `ProviderId → Arc<dyn Provider>` の `HashMap`
  (`contract.rs:333-335`)。現在は `"builtin.agent.rig"` という1つの
  `ProviderId` に1つの `Provider` インスタンスが登録されているだけ
  (`default_provider_id()` もこの文字列を固定で返す、359-361行)。
- `horizon`(GUI 側)は `horizon-agentd` をサブプロセスとして起動するだ
  けで、Horizon の設定ファイルの中身を agentd に引き渡す経路は無い
  (`src/` 配下に `AgentConfig::from_env_and_file` の呼び出しは存在しな
  いことを確認済み)。`config.rs` のモジュールコメントは「Horizon 側で
  変換して渡す」という当初想定(`horizon` の `src/agent/config.rs` とい
  う設計時点の見立て、現存しないファイル)を記しているが、実装では
  `horizon-agentd` が自ら同じ TOML ファイルを直接パースする形
  (`load_file_config` 他、74-157行)に落ち着いており、コメントと実装
  が食い違っている(役割の話とは別軸の、ドキュメント鮮度の問題として
  記録)。

### 2.3 `SessionNew.config_overrides` は配線されていない

- `wire.rs::SessionNew { session_id, provider_id, config_overrides:
  Option<serde_json::Value> }`(170-174行)。ドキュメントコメント自身が
  「a later step が実override フィールドを定義するまでのプレースホルダ
  で、step 2 ではどこからも生成・消費されていない」と明言している。
- 実際の唯一の生成箇所 `src/agent/agentd_runtime.rs:155-159` では
  `config_overrides: None` が固定値。
- 受け手側の `contract::StartSession { session_id, provider_id }`
  (`contract.rs:51-54`)には `config_overrides` を運ぶフィールドすら無
  く、`ProviderRegistry::start_session` もこれを無視する(363-373行)。
  **wire レベルには器があるが、contract 層にも provider 層にも配管が繋
  がっていない、文字通り死んでいるフィールド。**

### 2.4 「役割つきエージェント」を足すときに邪魔になるもの(まとめ)

1. `system_prompt` が role 引数を取らない自由関数であること(2.1)。
2. `tools::definitions()` が固定リストを返す自由関数で、role/session 単
   位のフィルタ機構が無いこと(2.1)。
3. `RigAgentConfig`(モデル名含む)が `Provider` インスタンスに 1:1 で焼
   き込まれ、「1 provider = 1 config = 1 モデル」が実質的な制約になって
   いること(2.2)。複数 role に複数モデルを持たせるには「role ごとに別
   `Provider`/`ProviderId` を登録する」か「`start_session` 時に config
   を渡せるようコントラクトを変える」かの二択に迫られる。
4. wire の `config_overrides` が無型のプレースホルダのまま放置され、
   role をどう運ぶか(専用フィールドを生やすか、これを実体化するか)の
   決定がまだ無いこと(2.3)。
5. `StartSession`/`Initialization` にも role 相当のフィールドが無く、
   `Provider` トレイト自体が role という概念を知らないこと。

### 2.5 最小リファクタリング推奨(今やる価値があるもの)

role が1つしか無い現状で role 抽象を先回りしてコードに刻むのは、
Horizon 自身の「過剰処方は害」という哲学に反する。今すぐ着手する価値が
あるのは、**role の有無に関わらずどのみち必要になる、後方互換な拡張点**
の2点に限定するのが妥当:

- `system_prompt` に「追加のガイダンス行」を任意で差し込める形(既定は
  現行出力と完全一致)を用意する。呼び出し元は1箇所のみで変更コストは
  小さく、role 専用ではなく Part 3 のスキル案内(3.2)にもそのまま使え
  る共通の差し込み口になる。1.5 で見つけた「リポジトリ指示ファイルを読
  む経路が皆無」というギャップの受け皿にもなる。
- `rig_tool_definitions()` に任意のツール ID 許可リスト(既定は「フィル
  タなし=現状通り」)を渡せるようにしておく。これも role 専用ではな
  く、将来「スキルが有効化するツールだけ渡す」という同じ差し込み口。
- それ以外(config の複数プロファイル化、wire の role フィールド化、
  `Provider` トレイトの role 対応)は、実際の2つ目の role が決まるまで
  設計の当否を検証しようがなく、今は着手しない方が一貫性がある。

### 2.6 役割導入時にやればよいこと(後回しでよいもの)

- **role をどの層の概念にするかの決定。** (a) role ごとに別
  `ProviderId`/`Provider` を登録する(既存の登録キー付けをそのまま使え
  て実装コストは最小だが、role が増えるたびセッションループ配線が重複
  する)か、(b) `StartSession`/`SessionNew`/`Initialization` に
  `role_id` を新設し単一の `Provider` が role レジストリを引く(重複は
  避けられるが `AgentConfig` を「名前付き role 設定のレジストリ」に作
  り替え、wire のコントラクトバージョンも上げることになる)。
- **`config_overrides` の決着。** 無型 JSON プレースホルダのまま実体化
  するのではなく、`config.rs` の他設定が一貫して型付きであることに合わ
  せ `role_id: String`(または型付き role 記述子)に置き換える方が流儀
  に合う。wire バージョン変更を伴うので、他の role 導入作業とまとめて
  一度にやるべきタイミングの問題であり、今切り出す理由は無い。
- **ツール許可リスト・承認ポリシーの role 連動。** 2.5 のフックを実際の
  role 別リストで埋める。`ToolPermission` は現状 `Definition` ごとに固
  定で role に関わらず一律——"review" role はより緩い(読み取り専用の
  みで承認自体が不要)、といった role 別ポリシーは role が実在してから
  検討すべき開いた論点。
- **agentd の config を「1プロセス=1モデル」から解放する。** role ごと
  に異なるモデルを使うなら、`config.rs` のスキーマに複数の named
  provider profile(例: `[provider.review]`)を持たせるか、`SessionNew`
  経由で config を渡す形に変えるかの選択が要る。`AGENTS.md` に明記され
  た env > file > default という優先順位ルールの適用範囲を広げる話でも
  あり、拙速に決めず role 導入の設計相談の中で扱うべき。

---

## Part 3: スキル機構の設計材料

### 3.1 Claude Code の Skills(SKILL.md)の要点

[Agent Skills overview](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)・
[Skill authoring best practices](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices)
によれば、3段階の段階開示(progressive disclosure)を取る:

1. **常時ロード。** 各 `SKILL.md` の YAML frontmatter にある `name`
   (64文字以内)と `description`(1024文字以内、「何をするか」と「い
   つ使うべきか」の両方を含める)が起動時にシステムプロンプトへ注入さ
   れる。1件あたり概ね100トークン程度で、多数の Skill を「存在は知って
   いるが中身はまだ読んでいない」状態でインストールできる。
2. **トリガー時ロード。** タスクと description が合致すると判断した時
   だけ、`SKILL.md` 本文(500行未満が目安)をファイル読み込みで取得
   し、初めてコンテキストに入れる。
3. **必要時ロード。** 本文が参照する追加ファイル(リファレンス文書、ス
   クリプト)は本文中で言及された時にさらに読む/実行する。スクリプトは
   コード自体をコンテキストに入れず実行結果だけを取り込める。

発見の仕組みは明示的な呼び出し API ではなく、**常時ロードされた
description とタスクの照合によるエージェントの自律判断**である。設計上
の根拠は Part 1.1 と同じ「コンテキストは貴重で有限」で、「メタデータで
気づかせ、本体で詳細を渡し、リンク先で必要なだけ渡す」構造は「よく整理
されたマニュアル」に例えられている。

### 3.2 Horizon に置くなら: 最小形の一案

CLI(horizon 自体)の使い方を最初のユースケースとして想定した最小形の一
案(実装しない、設計材料としてのみ提示):

- **ファイル規約。** `docs/skills/<skill-id>/SKILL.md` のような固定パス
  規約に `name`/`description` の frontmatter + Markdown 本文という
  Claude Skills 同等の最小構成を踏襲する。role や対象ツールなどの追加
  フィールドは必要になってから足す。
- **発見方法。** セッション開始時に、その固定パス配下の `SKILL.md` 群の
  frontmatter だけを軽量に列挙し、`system_prompt` の追加行(2.5 で用意
  する拡張点)として「利用可能なスキル: `<name>` — `<description>`」の
  一覧を差し込む。本文は読まない。
- **プロンプトへの案内。** 既存の「Tool policy」節に1行、"関連するスキ
  ルがあれば `fs.read` でその `SKILL.md` を読んでから作業すること" 程
  度の誘導に留める(手順の処方ではなく道具の存在を教えるだけ)。実際の
  読み込みは既存の `fs.read` に委ね、新規ツールは増やさない。

この最小形は Part 1.5 で指摘した「AGENTS.md を読む経路が無い」というギ
ャップとも設計思想を共有する——どちらも「エージェントに存在を知らせた
上で、ツールで能動的に取りに行かせる」という同じパターンであり、実装す
るなら両者を同じ拡張点(2.5)の上で一緒に設計するのが自然である。

---

**前提訂正(2026-07-06、所有者からの補足)**: 実際のプロバイダーは
Moonshot 直ではなく synthetic.new(OpenAI 互換、`hf:` プレフィクス)。
Part 1 の Kimi 固有要件(reasoning_content 保持等)は Moonshot 公式 API
のドキュメントに基づくもので、synthetic.new の互換層で同じ制約が現れる
かは同社ドキュメント(models ページ)に記載がなく未確定。resume 経路の
リスクは実測検証待ちに格下げ。

**実測結果(2026-07-06)**: `api.synthetic.new/openai/v1/chat/completions`
に `hf:moonshotai/Kimi-K2.7-Code` を直接叩いて検証。レスポンスには
`reasoning_content` フィールドが確かに載る(`tool_calls` の `id` も
vLLM 系エンジンに特徴的な `functions.<name>:<idx>` 形式)。しかし、直前
ターンの assistant tool-call メッセージから `reasoning_content` を完全
に落とした履歴(Horizon の `rig_messages_from_horizon_events` と同じ
形)で追撃のツール呼び出しターンを送っても、単発でも2ラウンド連続で
も HTTP 200 で正常に会話が継続し、エラーは一切再現しなかった(reasoning
を残した対照リクエストとの応答は実質同等)。Moonshot 公式ドキュメントの
「reasoning_content を落とすとエラー」という制約は、少なくとも
synthetic.new の互換層では効いていない。**判定: 再現せず。コード変更は
行わない。**
