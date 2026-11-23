# エージェントのコアと拡張の切り分けの調査(2026-11-23)

## 問い

エージェントのフレームワーク pi・Mastra・eve・Flue を参考に、今日までにコモディティ化したエージェントのコアの機能と、その上で挙動をカスタマイズする拡張の部分とを分けたい(オーナーの依頼、2026-11-23)。続く相談で、目標は次のように絞られた。

> まずはそれを意識した形へのコードベースとインターフェースの整備と、コモディティ化した部品にベストプラクティスを反映していくのを目標にしましょうか。
> 言い換えると、別プログラムやwasmの形に切り出すことが容易な構造にはしたいけれども、まずはモジュールとして整理できていればそれでよいです。

この文書は観察の記録で、Horizon にどれを採るかの推奨は書かない。推奨と決定は設計文書の側に置く。

## 出典の確かさの印

調査はサブエージェントが 2026-11-23 に行い、それぞれの報告をここにまとめた。4 つのリポジトリは既定ブランチを shallow clone して読んだ(ビルドや実行はしていない)。

- **[S]**: ソースコードを読んで確かめたもの
- **[D]**: 公式の文書だけによるもの

| 名前 | 特定したもの | 読んだ版 |
|---|---|---|
| pi | `github.com/earendil-works/pi`(旧 `badlogic/pi-mono`、301 で転送される)。npm の `@earendil-works/*` | `pi-coding-agent`/`pi-agent-core` 1.6.0(2026-11-20)、commit f25aecaa |
| Mastra | `github.com/mastra-ai/mastra` | `@mastra/core` 1.86.0-alpha.5、commit 61d640c1。clone に `@mastra/memory`・`@mastra/mcp`・`@mastra/observability` は含まれず、それらは型と文書だけで読んだ |
| eve | Vercel の `github.com/vercel/eve`(eve.dev)。他に同名のエージェントのフレームワークは見つからなかった | 1.3.0、commit e254c0ff |
| Flue | Astro チームの `github.com/withastro/flue`。`scuffi/flue` などの同名は調べていない | `@flue/runtime` 2.3.1、commit 129c6796 |

eve と Flue は名前からの推定で、オーナーが別のものを指していた可能性は残る。

4 つとも TypeScript で、拡張はすべてエージェントと同じプロセスで動き、拡張のコード自体を sandbox に入れるものはない。sandbox に入るのはモデルが打つ shell だけ(pi はそれもない)。

Flue のループは pi の `pi-agent-core` そのもので(`packages/runtime/package.json` の依存、`reference/agent-behavior.md`)、4 つは独立した 4 例ではない。

## 各フレームワークのコアと拡張面

### pi

- **コア(`pi-agent-core`)**: `Agent` クラスと `AgentState`(model・thinkingLevel・tools・messages・pendingToolCalls)、低水準の `agentLoop`。イベントは `agent_start/end`・`turn_start/end`・`message_*`・`tool_execution_*`。`steer()` と `followUp()` の 2 つのキュー。ライブラリとしての口に `transformContext`・`convertToLlm`・`prepareRequest`・`finishTurn`・`beforeToolCall`・`afterToolCall` がある。セッションと compaction は agent-core になく、`pi-coding-agent` の側にある。[S]
- **プロバイダ(`pi-ai`)**: 統一したプロバイダとモデルの一覧、usage と料金、thinking level、ツール入力の部分 JSON のストリーム。[S]
- **拡張面**(`docs/extensions.md`): `(pi: ExtensionAPI) => void` を default export する。[S]
  - イベントは約 25 種で、戻り値で書き換えや拒否ができる。`input`(変換・処理済み)、`before_agent_start`(system prompt の差し替え・節の変更)、`context`(モデルに送るメッセージの書き換え)、`before_provider_request`(ペイロードの差し替え)、`tool_call`(`{block, reason, terminate}`)、`tool_result`(内容の修正)、`turn_end`/`agent_before_settle`(`{continue}`)、`session_before_compact`(取り消し・独自の要約)、`session_before_fork` など。
  - 登録: `registerTool`(描画・`exposure`)、`registerCommand`、`registerProvider`、`registerMcpServer` など。
  - 読み込み: `~/.pi/agent/extensions/` と `.pi/extensions/` から自動で探す(project のほうは project trust の後)。jiti で TypeScript をそのまま読む。OS の権限そのままで動くと文書に明記している(`docs/security.md`)。
- **姿勢**: README は「minimal, extensible agent harness … skips features like sub-agents and plan mode」。サブエージェント・plan mode・承認のゲートは例の拡張として置く。0.99.0(2026-09-29)から MCP・codemode・tool_search を、公開の拡張 API で書いた「built-in extensions」として既定で読み込む(`-builtin:mcp` で外せる)。[S]
- 実験的な `pi-durable` には別の拡張の形 `defineExtension({tools, sections, hooks, wraps, tasks})` があり、system prompt を名前付きの節として扱う。[S]

### Mastra

- **コア(`@mastra/core`)**: `Agent` クラス。設定は `instructions`・`model`・`tools`・`memory`・`skills`・`workspace`・`inputProcessors`・`outputProcessors`・`errorProcessors`・`agents`(サブエージェント)など、多くが `RequestContext` から毎回決められる。ループは AI SDK の provider 仕様の上の自前。承認(`requireApproval`・`suspend()`/`resumeStream()`)とサブエージェントはコアにある。[S]
- **拡張面は Processor の段**(`reference/processors/processor-interface.mdx`)[S]:

  | 段 | いつ | 何を変えられるか |
  |---|---|---|
  | `processInput` | リクエストごとに 1 回 | メッセージと system メッセージ |
  | `processInputStep` | 各ステップ | model・`toolChoice`・`activeTools`・tools・messages・providerOptions |
  | `processLLMRequest` | プロバイダ呼び出しごと | 最終の prompt |
  | `processOutputStream` | チャンクごと | チャンク(null で捨てる) |
  | `processOutputStep` | モデルの後、ツールの前 | retry の要求 |
  | `processToolResult` | ツール結果ごと | 伏せ字・中止 |
  | `processAPIError` | プロバイダのエラー | `{retry: true}` |

  どの段も `abort()` で止められる。memory も processor の集まりとして実装されている(`MessageHistory`・`SemanticRecall`・`WorkingMemory`)。ツールには `beforeToolCall`(`{proceed:false}` で実行を飛ばす)と `afterToolCall` がある。
- **読み込み**: コードで渡すか、`src/mastra/agents/<name>/` のファイルの規約。パッケージの形式はない。[S]

### eve

- **コア**: 会話ごとに checkpoint を持つ durable な workflow。既定のツールは `bash`・`read_file`・`write_file`・`edit_file`・`web_fetch`・`web_search`・`agent`。compaction・承認(`approval: never()|once()|always()|auto()`、`auto` は判定モデルに聞く)・サブエージェント・memory の slot がコアにある。[S]
- **拡張面**: 作者が書くのはファイルの並び(`instructions.md`・`tools/*.ts`・`skills/`・`hooks/`・`subagents/`)。[S]
  - モデルに見えるものは `defineDynamic` で、モデル呼び出しの前に毎回決め直す。
  - **フックは観測専用**: `defineHook({events: {...}})` の戻り値は無視される。できるのは副作用と `ctx.cancel()`・`ctx.continue()` だけで、「hooks can't add text to the model's context」と文書に書いている(`docs/guides/hooks.md`)。
  - 拡張のパッケージ: `defineExtension({config})` がツール・スキル・サブエージェント・指示の断片・フックを束ねる。`agent/extensions/crm.ts` に置くとファイル名が名前空間になる(`crm__search`)。拡張の root は agent の設定・memory・sandbox を変えられない。git・browser・computer-use などの機能を、同梱の拡張として出している。

### Flue

- **コア(`@flue/runtime`)**: pi-agent-core のループ。入力はすべて、モデルが動く前に durable に記録する。会話ごとのキューは 1 本ずつ流れ、実行中に来たメッセージは次のターン境界で合流する。system prompt は一度組み立てたら固定し、後の変更は追記の「signal」として伝える。[S]
- **拡張面は React 風の関数**: エージェントは、モデル呼び出しの前に毎回評価し直される関数。`useModel`・`useTool`・`useSkill`・`useSubagent`・`useInstruction`・`usePersistentState`、イベントの口 `useAgentStart`・`useAgentFinish`(signal を足すと続ける、最大 32 回)。再利用は `use*` 関数で、パッケージの形式はない。[S]
- 承認の API はない。文書は永続状態と `useAgentStart` で組む型を示している(`guide/tools.md`)。ツール呼び出しを横取りする口は文書には見当たらない。[D]

## 共通するコアと、拡張に出すもの

4 つすべて(1 つが外れるときはそれを記す)のコアにあるもの:

1. ループ: ストリームで受けたモデル呼び出し → ツールを並列に実行 → ツール呼び出しがなくなるまで繰り返す。ターンとメッセージのイベント、AbortSignal での中断。
2. プロバイダとモデルの抽象: `"provider/model"` の ID と、context window・料金・reasoning を持つモデルの一覧。
3. 型の付いたツール: schema、`execute(signal, onUpdate)`、結果の切り詰め。
4. read・write・edit・bash・grep・glob。上限は 2000 行 / 50KB でほぼ揃う(Mastra は `Workspace` 経由)。
5. Agent Skills 仕様の `SKILL.md` と `AGENTS.md`。
6. MCP クライアント(pi は同梱の拡張、eve は connections、Flue は `useMcpConnection`、Mastra はコア)。
7. 実行中の steer と、次の入力の queue。
8. 閾値で自動に走る compaction(要約+直近を原文で残す)。Mastra は memory の processor の側で扱う。
9. 永続化したセッションのログ。

拡張の側に出しているもの: 独自ツール、system prompt への差し込み、プロバイダに渡す前の context の書き換え、ツール呼び出しの可否、「まだ止まらない」継続の判断、監査用の observer、memory と recall。

割れているところ:

- **フックの強さ**: pi は命令的で、ほぼ何でも書き換え・拒否できる。Mastra は段の決まった型付きの pipeline。eve はモデルに見えるものを宣言で決め、フックは観測だけ。Flue は関数の再評価。
- **承認とサブエージェント**: eve と Mastra はコア。pi は拡張(例として置く)。Flue は承認を持たず、サブエージェント(`task`)はコア。
- **compaction の持ち主**: pi・eve・Flue は harness、Mastra は memory の processor。
- **durable 性**: eve と Flue は土台、Mastra は選べる、pi は実験的な `pi-durable` だけ。pi-coding-agent は代わりに分岐できるセッションの木を持つ。

## 部品ごとの実装の詳細

### 1. ループ

- **並列実行**: 4 つとも既定は並列。[S]
  - pi: `ToolExecutionMode` の既定は `"parallel"`、ツールごとの `executionMode: "sequential"` があると、そのバッチ全体が直列になる(`agent-loop.ts:526-531`)。結果はモデルが出した順に返す。同じファイルへの書き込みは realpath ごとのキューで直列にする(`file-mutation-queue.ts`)。
  - Mastra: `DEFAULT_TOOL_CALL_CONCURRENCY = 10`。承認や suspend がありうるツールが有効なとき(または実際に呼ばれたとき)は 1 にする。モデルのストリームの途中でも、引数が揃ったツールから実行を始める(`loop-builder.ts:290-311`)。
  - eve: すべての呼び出しを `Promise.allSettled`。`edit_file` の中だけパスごとのキュー。
  - Flue: pi の parallel。パスごとのロック(`agent.ts:140-169`)。
- **中断**: 4 つとも AbortSignal を `execute` に渡す。シグナルを無視するツールは待たずに見捨て、「結果は不明」とモデルに伝える。[S]
  - Mastra: `TOOL_ABORT_GRACE_MS = 250` の後に reject。文言は「The tool may still be running and its effects are unknown. Check its status before retrying.」
  - eve: 「It may have partially run; it was not retried.」
  - Flue: 1 macrotask 待ってから見捨てる。ツールごとの `timeoutMs`。
- **反復の上限**: pi と Flue にはない(Flue は自身の継続に 32 回の上限)。eve も上限や同一呼び出しの検出を持たず、代わりに `inputTokens − cacheReadTokens ≥ 40_000_000` で人間に続けるかを聞く(`hitl/budget.ts:14`)。Mastra は `maxSteps` が既定 5。[S]
- **retry**: [S]
  - pi: プロバイダの層で 408/409/429/5xx を `min(0.5·2^i, 8) 秒 × (1 − rand·0.25)` で再試行し、サーバの要求する待ちが 60 秒を超えるとエージェントの層に渡す。エージェントの層は 3 回(2/4/8 秒)。失敗した試行は `context_edit` の記録でモデルの context から隠す。
  - Flue: 3 回、基準 2000ms。**retry の回数を durable なログから数える**ので、再起動しても予算が戻らない。
  - eve: 3 回。エラーを retry・終了・復帰可能(402、overflow、空の応答)の 3 つに分ける。出力の停滞は 10 分で打ち切る。
  - Mastra: p-retry 2 回、`Retry-After` は 30 秒まで。モデルの代替の列。
- **出力の上限(`length`)で止まった場合**: pi・Flue・eve は、その応答のツール呼び出しを**実行せず**、再発行を促すエラー結果を返す。pi の文言は「Tool call "X" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.」(`agent-loop.ts:487-512`)。切れた文章を自動で続けるものはない。Mastra は `length` を終了として扱い、警告を出すだけ。[S]

### 2. プロバイダとモデル

- **モデルの一覧**: [S]
  - pi: `catalog/generate.ts` が models.dev v2 と OpenRouter などから生成する。フィールドは `input`(text/image)、`inputLimits`(画像の大きさ)、`cost{input, output, cacheRead, cacheWrite}`、`reasoning`、`thinkingLevelMap`、`promptCache{short, long}`(秒)、`contextWindow`、`maxTokens` など(`ai/src/types.ts:853-902`)。
  - Mastra: models.dev から生成した `provider-registry.json`。context window と max output はコアにない。
  - eve: AI Gateway の catalog から context window だけ。料金は gateway の metadata から。
- **usage**: pi は `{input, output, cacheRead, cacheWrite, reasoning?, cost{…, total}}`。eve は `{inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens, costUsd}`。[S]
- **thinking level**: pi は `off|minimal|low|medium|high|xhigh|max` を各プロバイダの値に写し、予算型のプロバイダには `{minimal:1024, low:2048, medium:8192, high:16384}` を使う(`simple-options.ts:82-122`)。[S]
- **プロンプトキャッシュと先頭部分の固定**: [S]
  - pi(Anthropic): `cache_control` を system、**最後のツール**、最後の user/system メッセージに置く。ツールや prompt の変更で system prompt は書き換えず、`toolsAdded`/`toolsRemoved` と名前付きの節の差分を、追記の system メッセージとして送る(`agent-loop.ts:333-385`)。対応する Anthropic モデルには `tool_addition`/`tool_removal` のブロックで送り、他のプロバイダには system prompt に畳み戻す。キャッシュの TTL が切れる前に、得なときだけ `maxTokens:1` で温め直す(`cache-warming.ts`)。
  - eve: breakpoint は最後のツール・最後の system・最後のメッセージ・一つ前の assistant の 4 か所。system prompt を固定の `base` と変わる部分に分ける。スキルの一覧・ツールの一覧・`AGENTS.md` は user 側の「announcement」として、sha256 が変わったときだけ追記する。
  - Flue: スキルの一覧とサブエージェントの一覧を durable な基準として固定し、毎回の評価との差を名前・説明・schema の digest で比べ、変化を `<signal type="resources">` として追記する。ツールの schema が変わらないよう、`task` や `activate_skill` は enum でなく自由な文字列の名前を取る(`agent.ts:353-372`)。ただし system prompt に日付の行があり、日付が変わると先頭が変わる。
  - Mastra: 自動の `cache_control` はない。

### 3. ツールの定義

- schema: pi は TypeBox(数値の文字列などを型に合わせて直す)、Mastra は Standard Schema / zod、eve は Standard Schema、Flue は Valibot。[S]
- 実行の形: pi は `execute(toolCallId, params, signal, onUpdate)` → `{content[], details, isError?, terminate?}`。eve は `execute(input, ctx)` が AsyncIterable を返すと、途中の値が経過の表示、最後が結果になる。Mastra は `ctx.writer.custom` で経過を出す。[S]
- ツール入力の部分 JSON: 4 つとも UI のイベントとして流す。pi は途中の JSON を読む parser を持つ(`streaming-json.ts`)。[S]
- 結果の切り詰め: pi は 2000 行 / 50KB を共通で持ち、MCP の出力は 20KB で真ん中を切って全体をファイルに書き出す。eve はツールごとに 2000 行 / 50KB / 1 行 2000 文字。Flue は独自ツールや MCP の出力を切らない。[S]
- ツールが多いとき: pi は `ToolExposure = direct|model-only|codemode|deferred|hidden` と BM25 の `tool_search`(有効にしたツールは記録に残るので fork や再開を越える)。Mastra は `ToolSearchProcessor`(BM25、topK 5)。eve は検索が TypeScript のシグネチャを返し、MCP のツールは常に検索の後ろに隠す。[S]

### 4. read・write・edit・grep・bash

- **read**: pi・Flue・eve は 2000 行 / 50KB。pi は `[Showing lines a-b of T. Use offset=N to continue.]`。画像は pi がモデルの上限に合わせて縮小する(既定 2000×2000)。eve は magic bytes で画像と PDF を見分ける。[S]
- **write**: pi と Flue は黙って上書きする。eve は前に読んだときの sha256 が一致しないと上書きしない。Mastra は選べる mtime の確認。[S]
- **edit**: [S]
  - pi(`edit.ts`・`edit-diff.ts`): `{path, edits:[{oldText,newText}]}`。各編集を**元の**ファイルに当て、それぞれ一意、重なりを禁止し、後ろから適用する。完全一致で失敗したら、NFKC・行末の空白・引用符やダッシュ・特殊な空白を正規化して当て直し、その場合は触れた行だけを書き換え、ほかの行は元のバイトのまま残す。CRLF と BOM を保つ。
  - eve: pi の edit をほぼそのまま移している。
  - Flue: 1 回に 1 編集、完全一致のみ。Mastra: 完全一致と、全部当たるか何も当てないかの `apply_patch`。
- **grep / glob**: pi は ripgrep(100 件)と fd(1000 件)。eve は rg が無ければ `grep -rE`、50〜200 件、出力 64KB まで。Flue は 100 件、1 行 500 文字。いずれも行の内容を返す。[S]
- **bash**: [S]
  - pi: timeout の既定なし、バックグラウンドなし。出力は末尾 2000 行 / 50KB で、必要なときだけファイルに書き出す。
  - eve: 60 秒(`BASH_YIELD_SECONDS`)たっても終わらないコマンドは kill せずにバックグラウンドへ移し、それまでの出力と、続きを読む・待つ・止めるための具体的なコマンドをモデルに返す(`sandbox/bash.ts:15,23`)。
  - Mastra: 既定 30 秒、`background` のフラグ、`| tail` で終わるコマンドには `set -o pipefail` を足す。schema の先頭に `description` を置き、モデルにコマンドより先に要約を書かせる。

### 5. スキルと文脈のファイル

- frontmatter: pi・Flue・Mastra は Agent Skills 仕様に揃う(`name` は 64 文字までの `[a-z0-9-]`、`description` は 1024 文字まで)。Flue は名前とディレクトリ名の一致を要求し、`compatibility`・`metadata` も読む。リソース(`references/`・`scripts/`・`assets/`)を読む口がある。[S]
- 探す場所: pi は `~/.pi/agent/skills`・`.pi/skills`・`~/.agents/skills`・cwd から repo root までの `.agents/skills`[D]。Flue は `<cwd>/.agents/skills` だけ。
- `AGENTS.md`: pi はディレクトリごとに `AGENTS.override.md, AGENTS.md, AGENTS.MD, CLAUDE.md, CLAUDE.MD` の最初の 1 つを、cwd から filesystem root まで読む。eve は workspace root の 1 つだけを user 側に入れ、別のディレクトリで作業する前に近くの `AGENTS.md` を読むよう prompt で指示する。Mastra の `AgentsMDInjector` は、ツールのパス引数が `AGENTS.md` のあるディレクトリに初めて触れたときに、1000 トークンまでの reminder を差し込む。[S]
- project trust: pi は `~/.pi/agent/trust.json` で project の設定・拡張・スキル・`mcp.json` を制限するが、文脈のファイルは制限しない[D]。Flue と eve にはない。

### 6. MCP

| | 転送 | ツールの名前 | 見せ方 | 置き場所 |
|---|---|---|---|---|
| pi | stdio と streamable HTTP | `mcp__<server>__<tool>`。長すぎるか衝突したら 8 桁の sha256 を付ける | 既定は codemode。deferred・direct・hidden も選べる | 同梱の拡張 |
| eve | streamable HTTP、だめなら SSE。stdio なし | `<connection>__<tool>` | 常に検索の後ろ | コア |
| Flue | streamable HTTP / SSE | `mcp__<server>__<tool>` | すべて直接 | コア |
| Mastra | stdio と Streamable HTTP [D] | `serverName_toolName` [D] | ToolSearchProcessor 経由 | `@mastra/mcp` |

### 7. 実行中の入力

4 つとも、ツールの実行中に来たメッセージはツールを止めず、バッチが終わってから次のモデル呼び出しの前に差し込む。[S]

- pi: steer は各ツールバッチの後に、followUp はツール呼び出しも steer も残っていないときだけ流す。どちらも既定は 1 件ずつ。
- eve: `turnPolicy: "steer"`(既定)か `"queue"`。**まだ見える出力が何も出ていなければ、実行中の生成を取り消して捨てる**(reasoning は出力に数えない。ツールが始まったら取り消さない)。
- Mastra: reasoning だけのストリームの途中で入力が来たら、ステップを使わずにリクエストを取り消してやり直す(`llm-execution-step.ts:1385`)。
- Flue: ターン境界で、同じエージェントへの待ち行列の先頭の連続部分を取り込む。取り込みは 2 段で、途中で落ちても引き継ぐか戻すかできる。

### 8. compaction

| | 予約・閾値 | 原文で残す末尾 | 出典 |
|---|---|---|---|
| pi | `reserveTokens: 16384`、`ctx > window − reserve` で発動 | `keepRecentTokens: 20000` | `compaction/compaction.ts:126-130` [S] |
| Flue | `reserveTokens: 20000`(モデルの maxTokens まで) | `keepRecentTokens: 8000` | `compaction.ts:39-60` [S] |
| eve | `0.9·window` で発動(不明なら 100k) | `min(20000, threshold/2)` と直近 10 件 | `compaction/config.ts:8-11` [S] |

- 切る位置: pi と Flue は user か assistant のメッセージで切り、ツール結果の途中では切らない。1 ターンの途中で切れたら、そのターンの前半に別の要約を付ける。[S]
- 要約の形: pi(Flue も同じ)は `## Goal`・`## Constraints & Preferences`・`## Progress`(Done / In Progress / Blocked)・`## Key Decisions`・`## Next Steps`・`## Critical Context`。前回の要約から更新する版もある。長さで切れたり、ツールを呼んだりした要約は捨てる。要約は user メッセージとして戻す。eve は compaction のたびに末尾を、原文 → 文章だけ(ツール結果を落とす)→ 要約だけ、と細くしていく。[S]
- 要約に渡すツール結果は、3 つとも 1 件 2000 文字に切る。[S]
- pi と Flue は、read・write・edit で触れたファイルの一覧を `<read-files>`・`<modified-files>` として compaction を越えて引き継ぐ。[S]
- トークンの見積もり: pi と Flue は、最後の assistant の usage に、その後のメッセージの文字数 / 4 を足す。eve は JSON のバイト数 / 4 に、モデルごとに実測との比で補正をかける(既定 1.6)。[S]
- 手動: pi は `/compact [指示]`、Flue は `harness.compact()`、eve は `compact` の配信。[S]
- Mastra の Observational Memory: Observer が 30000 トークンで観察を作り、Reflector が 40000 で観察の記録を書き直す。プロンプトキャッシュがどうせ失われる時(アイドルが TTL を超えた、プロバイダを替えた)に圧縮する。[D]

### 9. セッションの永続化

- **pi**(`docs/session-format.md`)[D、ソースと整合]:
  - ファイル: `~/.pi/agent/sessions/--<cwd>--/<ts>_<uuid>.jsonl`。先頭は `{type:"session", version:3, id, timestamp, cwd, parentSession?}`。
  - すべての entry が `{type, id(8 桁の hex), parentId|null, timestamp}` を持ち、木になる。
  - entry の種類: `message`・`model_change`・`thinking_level_change`・`usage`・`compaction{summary, firstKeptEntryId, tokensBefore, systemMessage, details}`・`context_edit{targetId, replacement}`・`branch_summary{fromId}`・`custom`・`label`・`session_info` など。
  - 文脈の組み立て: 葉から根へたどり、最新の compaction を取る。その要約と、`firstKeptEntryId` から compaction までの entry、その後の全部を並べ、最後に `context_edit` を当てる。
  - 操作: `/tree` は同じファイルの中で葉を動かす(離れる枝には `branch_summary` を付けられる)。`/fork` は user メッセージから新しいファイルを始める。`/clone` は今の枝を写す。
- **Flue**: 追記だけの記録 `{v:1, id, type, conversationId, submissionId?, turnId?, …}`、約 20 種。メッセージは parentId の木で、`activeLeafId` を持つ。落ちたときに未決着の普通のツール呼び出しは再実行せず、「Tool execution was interrupted before completion. The outcome is unknown.」を返す。[S]
- **eve**: 1 行が 1 つの原子的な遷移の `CommitLine{at, facts[]}`。クライアントは行番号から再開する。始まりが記録されたツール呼び出しは再実行しない。[S]
- **Mastra**: durable な redo はモデルとツールを再実行するが、ID を snapshot から取るので追記でなく upsert になる。[S]

### 10. サブエージェント

| | 置き場所 | 深さ | 同時実行 | 子に渡すもの | 返すもの |
|---|---|---|---|---|---|
| pi | 例の拡張(別プロセス) | 制限なし | 8 タスク・同時 4 | タスクの文と agent の markdown | 最後の assistant の文、50KB まで |
| Flue | コアの `task` | `MAX_DELEGATION_DEPTH = 4` | バッチの並列のみ | 親の履歴なし、`AGENTS.md` とスキルは探し直す | 最後の文 |
| eve | コア、非同期 | 宣言したものは制限なし | `MAX_WORKING_TASKS = 32` | `{message, taskId?}` | 後から `<task_result>`、50KB / 2000 行まで。結果が来るまで親のターンを開けておける |
| Mastra | コア(ツールとしてのエージェント) | 制限なし | ツールの並列 10 | 親のメッセージからツール部分を除いたもの | 文と `[ref:id]` |

[S]

## Horizon の今との対照

Horizon 側は、05fbbebb(2026-11-23)の上でサブエージェントが読んだもの。そのあと main(11247752)にプロバイダまわりの変更が入った(896f6be8「Reach providers as rig vendors and read model facts from rig's catalog」、394b4ccb・f9f41439・54ff83c8 の reasoning effort、11247752 の Responses API)。2 番の行のうち、モデルの情報・reasoning・usage の ACP への写し・`cache_control` の有無は 11247752 で読み直した。それ以外は 05fbbebb の時点のもの。

| # | 部品 | Horizon | 4 フレームワークの収束点 |
|---|---|---|---|
| 1 | ツールの並列実行 | 同期ツールは agentd のセッションのスレッドで順に実行する。bash はセッションごとに 1 本の FIFO(バックグラウンドのジョブも同じ列)。web だけ並行。ツールごとの設定はない | 既定は並列、同じファイルへの書き込みだけ直列 |
| 1 | 中断 | プロセスグループを kill し、`/proc` をたどって逃げたものも SIGKILL する。結果は Cancelled | 待たずに見捨て、結果は不明と伝える |
| 1 | 反復の上限 | 連続 100 ラウンド、同一の (ツール, 引数, 出力) が 5 回で止める | なし(eve はトークン予算) |
| 1 | retry | 429/5xx と転送のエラー。5xx は 3 回、429 は取り消されるまで無制限。1 秒から倍、30 秒まで。ツール呼び出しを出した後は retry しない | 3 回前後。pi は失敗した試行を context から隠す。Flue は回数をログから数える |
| 1 | `length` で停止 | 失敗のターンにしたうえで、「簡潔に」と促して自動で続ける(連続 3 回まで) | 切れたツール呼び出しを実行せず再発行を促す。文章は続けない |
| 2 | モデルの情報 | rig の catalog(models.dev を手で見直したもの)に新しい models.dev を重ねたもの。OpenAI の wire は先に `/models` を読む。Anthropic の wire は catalog に直接聞く。`max_tokens` は 32768 で固定 | 生成した一覧が context・max output・料金・reasoning・cache の TTL を持つ |
| 2 | reasoning | セッションごとの reasoning effort を、catalog が示す段階から選んで ACP の設定で切り替えられる | thinking level を各プロバイダの値に写す |
| 2 | usage・料金 | `ProviderRequestUsage` を JSONL と DuckDB の events に記録するだけで、ACP への写し(`hub/mapping.rs:490`)は捨てる。料金の計算はない | usage に cache read/write と料金を含める |
| 2 | プロンプトキャッシュ | `cache_control` はどこにもない。system prompt はセッションの開始時に作った材料から毎回組み立て、ツールの一覧は毎回作り直す(絞り込みに環境変数も読む) | system とツールを固定し、変更は追記で伝える。Anthropic には breakpoint を置く |
| 3 | ツールの定義 | `Tool` trait はなく、`catalog.rs` の `Definition` と閉じた match。同期ツールは中断のシグナルを受け取らない。途中経過は `task` だけ。rig の provider は部分 JSON を捨てる | `execute(signal, onUpdate)`、部分入力を UI へ |
| 4 | read | 既定 500 行・最大 2000 行、5 万文字、1 行 2000 文字。UTF-8 だけで、画像は読めない | 2000 行 / 50KB、画像も |
| 4 | write | 前に読んだときの mtime が一致しないと上書きしない | eve は sha256 で同じ確認。pi と Flue はしない |
| 4 | edit | 前から順に当て、後の編集は前の結果に当たる。最初の失敗で止まり、戻さない。fuzzy なし | 元のファイルに当て、重なりを禁止、正規化して当て直す(pi・eve) |
| 4 | grep | 位置だけで、行の内容は返さない。既定 100 件 | 行の内容を返す |
| 4 | bash | 既定 300 秒、最大 1800 秒で kill。出力は 3 万文字(前 1/3・後 2/3)、全体は常にファイルに書き出す。バックグラウンドの指定あり | eve は 60 秒でバックグラウンドへ移して続きの読み方を伝える |
| 5 | スキル | frontmatter は手書きの parser で `name`・`description` だけ。本文は `skill_read` で読む。リソースのファイルはない。探すのは git root から cwd までの `.horizon/skills` | Agent Skills 仕様、リソース、`.agents/skills` |
| 5 | `AGENTS.md` | git root から cwd まで、ディレクトリごとに `AGENTS.md`(なければ `CLAUDE.md`)。合計 24,000 文字。project trust が要る | ほぼ同じ |
| 6 | MCP | ない | 4 つともある |
| 7 | 実行中の入力 | 次のラウンドの境界で差し込む。宛先が違えば別のターンとして待たせる | 同じ。eve と Mastra は見える出力が出る前なら生成を取り消す |
| 8 | compaction | Tier 1 で古いツール結果の本文だけを消す(context の 60% で発動、末尾 16,384 トークンは残す)。要約と手動の compact はない。`docs/agent-compaction-design.md` は Tier 2 を保留としている | 決まった節立ての要約+直近 8k〜20k を原文で残す、触れたファイルの一覧を引き継ぐ |
| 9 | 永続化 | 全セッションで 1 つの JSONL と DuckDB の projection。再起動後に再開できる。開いたままのターンは Cancelled として閉じる。分岐はない | pi は entry の木で fork と clone。Flue と eve は未決着のツール呼び出しを再実行しない |
| 10 | サブエージェント | `task`(read は explore、write は task-write の role)。深さ 1、同時実行の上限なし、非同期で通知する | 深さ 4、同時実行 8〜32 |

### 05fbbebb の時点の記述の訂正

05fbbebb では `model_limits` が Anthropic の種別に対して何も問い合わせずに `None` を返し、窓のない Tier 1 は発動しなかった(「a `None` window means Tier 1 clearing never fires」)。11247752 では Anthropic の wire も catalog に聞くので、catalog が知っているモデルには窓が付く。オーナーの config(2026-11-24 時点)の `[[providers]]` は ollama と deepinfra(どちらも OpenAI の wire)で、Anthropic の entry はない。
