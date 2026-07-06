# OpenUI (thesysdev/openui) 調査 — Horizon への適用可能性

調査日: 2026-07-06。対象は <https://github.com/thesysdev/openui>(MIT
license、"The Open Standard for Generative UI")。README・仕様 mdx 一式
(`docs/content/docs/openui-lang/*`, `docs/content/docs/agent/*`)・
`packages/lang-core` の実装ソース・blog 記事・GitHub API(スター数/
コミット統計/contributors)を一次情報として読んだ。リポジトリはローカルに
`git clone --depth 1` 済み、ソースは `packages/lang-core/src/{parser,runtime}`
を実際に読んで検証している。

**名前の衝突に関する注意。** "OpenUI" という名前は Weights & Biases の
`wandb/openui`(自然言語プロンプトからその場で HTML/UI を生成するプロトタ
イピングツール、2024年3月公開)にも使われているが、**別プロジェクト**で
ある。本稿が扱うのは thesysdev 版のみで、wandb 版とは無関係。

---

## Part 1: OpenUI とは何か(事実)

### 1.1 正体 — 2つの層 + 1つの商用層

OpenUI は単一のプロダクトではなく、独立した3層で構成される。

1. **OpenUI Lang** — LLM が出力する、UI を宣言的に記述するための compact な
   DSL + それを解釈するパーサ/評価器("ランタイム")。`@openuidev/lang-core`
   がフレームワーク非依存の実装(パーサ・プロンプト生成・AST 評価器、React
   依存なし)、`@openuidev/react-lang`(および `vue-lang`/`svelte-lang`/
   `browser-bundle`)がその上に薄く乗るレンダラバインディング。**これが
   「仕様(スキーマ)+ レンダラ」の実体**であり、本稿 Part 2 で最も重要な
   調査対象。
2. **AgentInterface**(`@openuidev/react-ui`)— スレッド一覧・composer・
   ストリーミング・artifact ブラウザまで含む「完成品のチャット UI」。
   バックエンドとは **AG-UI**(`@ag-ui/core`, 外部の独立プロジェクト、
   確認したバージョンは `^0.0.53` — これ自体が pre-alpha)というイベント
   プロトコルで通信し、OpenAI/LangGraph 等プロバイダごとの stream adapter
   がその形式へ正規化する。
3. **OpenUI Cloud** — thesys 社が運営する商用マネージドバックエンド
   (`THESYS_API_KEY`、`console.thesys.dev`)。OSS 版は「基本的なコンポー
   ネント」「エラー検出・補正なし」「テーマ/ホワイトラベルなし」で、
   Cloud 版がそれらを有償で埋める、という明確な open-core モデル
   (`docs/agent/getting-started/openui-cloud.mdx` の比較表で公式に明示)。

Part 2 で Horizon にとって意味があるのは主に (1) であり、(2)(3) は
React/商用エコシステム前提でHorizon(native/floem)には転用先がない。

### 1.2 中身 — OpenUI Lang の実体

**構文(事実、仕様 mdx より)。** 1行1文の代入形式 `identifier = Expression`。
`root = Root([...])` を entry point とし、コンポーネント呼び出しの引数は
Zod スキーマのキー順で位置引数として解決される(名前付き引数は書けない
— `Stack([children], "row", "l")` であって `direction: "row"` ではない)。
式は文字列・数値・真偽・null・配列・オブジェクト・参照・三項演算・二項
演算・メンバアクセス(配列に対する列 pluck を含む)のみのクローズドな部分
集合。

**進化(事実)。** v0.1(静的生成、データは出力に焼き込み)→ v0.5(現行、
`$variable` によるリアクティブ状態、`Query`/`Mutation` によるツール
接続データ、`@Count`/`@Filter`/`@Sort`/`@Each` 等 `@` プレフィックスの
組み込み関数、`Action([...])` によるアクション合成)。仕様は既に**少なくとも
2回の破壊的世代交代**を経ている(v0.1→v0.5に加え、その前身として JSON
ベースの "Thesys V1" を丸ごと廃棄した経緯が blog に明記されている)。

**ストリーミング対応(事実)。** 前方参照(hoisting)を許可し、未定義
識別子への参照はスケルトン/プレースホルダとして描画される(v0.1 仕様
より)。パーサは6段パイプライン(autocloser → lexer → splitter → parser
→ resolver → mapper)で、確定済み文(depth-0 改行で終端)は不変とみなし
AST をキャッシュ、ストリーム中は末尾の未完成文だけ再パースする漸進
キャッシュ方式(O(N²) → O(N))。**インクリメンタル編集**: 2ターン目以降
LLM は変更した文だけを再出力し、パーサは同名の代入で「最後の定義が勝つ」
形でマージする(未言及の識別子はそのまま保持)。

**インタラクション(事実、ソース確認済み)。** `Action([...])` の中身は
5種類の "ActionStep" に閉じている: `@Run(ref)`(Query の再取得/Mutation
の実行)、`@Set($var, value)`、`@Reset($var...)`、`@ToAssistant("msg")`
(LLM へメッセージを返す=会話継続)、`@OpenUrl("url")`。`Query`/`Mutation`
は文字列のツール名+引数オブジェクトを保持するだけのノードで、実行は
ホスト側が渡す `toolProvider`(関数マップ or MCP クライアント)が担う。
**`packages/lang-core/src/runtime/evaluator.ts` を読んだ限り、AST 評価器
は `eval`/`Function`/`child_process` を一切使わないクローズドな
tree-walking interpreter**(ノード種は Literal/StateRef/Ref/Arr/Obj/Comp/
BinOp/UnaryOp/Ternary/Member/Index/Assign の12種に限定)。`eval`/`Function`/
`child_process` は `packages/openui-cli`(スキャフォールド用 CLI、LLM
出力とは無関係)にのみ存在する。

**レンダラ実装(事実)。** 公式ドキュメントの「Rewriting our Rust WASM
Parser in TypeScript」(2026-03-13)によれば、パーサは元々 Rust で書かれ
WASM にコンパイルされていたが、**JS↔WASM 境界のコピー/シリアライズ
コストがボトルネックになり、TypeScript への全面書き換えで per-call
2.2〜4.6倍・ストリーム総コスト2.6〜3.3倍高速化した**という理由で置き換
えられた。リポジトリ内に `.rs` ファイルは現存しない(`find . -iname
'*.rs'` で0件、確認済み)。react-lang が唯一の「フル機能」レンダラで、
vue-lang/svelte-lang は同じ `lang-core` の上に乗る薄いバインディング、
browser-bundle はビルドレス埋め込み用スクリプトタグ版。

**コンポーネント語彙(事実、`packages/react-ui/src` から抽出)。** レイアウト
(Stack/Grid/Section/Card/CardHeader/Tabs/Accordion/Carousel/Separator)、
コンテンツ(TextContent/MarkDownRenderer/Image/ImageGallery/CodeBlock/
Tag/ListBlock)、チャート(Bar/Line/Pie/Area/Scatter/Radar/Radial/
HorizontalBar/SingleStackedBar + Series/Point/Slice)、フォーム
(Form/FormControl/Input/Select/CheckBoxGroup/RadioGroup/SwitchGroup/
DatePicker/Slider/TextArea)、フィードバック(Callout/FollowUpBlock)、
Steps、Modal、Table/Col。拡張は `defineComponent({name, description,
props: z.object(...), component})` + `createLibrary(...)` で、システム
プロンプトはこのスキーマ定義から自動生成される(`library.prompt(...)`)。

### 1.3 思想(主張として区別)

- **「JSON は言語のふりをしたデータ形式」**(blog "Stop making AI write
  JSON")。JSON でインタラクティビティ(状態束縛・条件分岐・アクション
  連鎖)を表現しようとすると `$bindState`/`$cond`/`$then`/`$else` 等の
  ネストしたラッパーオブジェクトが爆発する、という具体例(json-render・
  Google A2UI の実例引用)は検証可能な形で提示されている。
- **「制約こそが契約」**: フルの JavaScript を使わない理由として「SQL が
  Python より DB クエリに向くのと同じ理由で、固定された原始集合(コンポ
  ーネント・状態・クエリ・ミューテーション・組み込み関数)に抜け道を
  作らない」ことを明言("no escape hatches" — これは 1.2 のソース調査で
  裏付けが取れた数少ない「主張かつ検証済みの事実」)。
- **トークン効率は「理由ではなく結果」**という自己申告(blog 内で明言)。
  最大67%のトークン削減・比較表(json-render比56.5%減、A2UI/CopilotKit
  との比較で「Latency 4.9s vs 14.2s/20s」等)は**自社ベンチマーク
  (`benchmarks/` 配下、tiktoken 計測)であり第三者検証ではない**。手法は
  リポジトリ内に公開されているが、7シナリオという限定的なサンプル。
  比較表の「Security risk: Minimal」列も自己評価。

### 1.4 成熟度(事実、GitHub API 実測)

| 指標 | 値 |
|---|---|
| リポジトリ作成日 | 2024-12-02 |
| スター数 / フォーク数 | 7,781 / 568 |
| Open issues / watchers | 90 / 31 |
| 総コミット数(概算) | 約613 |
| contributors(1ページ目) | 41人。上位はほぼ全員 `*-thesys`/thesys 所属を示す
  ユーザー名(`i-subham23` 162 commits が最多、以下 `ankit-thesys` 91、
  `abhithesys` 89 等) |
| git tag / GitHub Releases | 0件(npm 版管理のみ) |
| 各パッケージの npm version | 全パッケージ pre-1.0(`react-ui` 0.12.1 が
  最高、`lang-core` は 0.2.7) |
| ADOPTERS.md | 外部組織4件を自己申告(第三者検証なし) |
| ライセンス | MIT |

まとめると、**スター数・トレンド(Trendshift 掲載)は大きいが、開発は
実質的に単一ベンダー(thesys)主導のOSSで、仕様自体が短期間に複数回の
破壊的改訂(JSONベース v1 全廃 → Lang v0.1 → v0.5)を経ている発展途上の
プロジェクト**。全パッケージが pre-1.0 であることも安定度の低さを裏付ける。
商用の OpenUI Cloud が本体のロードマップ(Document exports, Live
dashboards, Continual learning 等)を牽引しており、OSS 単体の将来像は
「Cloud の下位互換」という位置づけに読める。

---

## Part 2: Horizon への適用分析(4観点)

前提として `docs/roadmap.md`、`docs/research/agent-ui.md`(既存2回の
transcript UI 調査)、`docs/trust-boundaries.md`、
`docs/agent-provider-contract.md` を踏まえる。

### (1) エージェント出力 UI(進行中の再設計の材料として)

`agent-ui.md` が模倣ファーストで収束させた方向(ツール呼び出しは「状態+
動詞+対象+要約」の1行、edit は専用レンダラで生 output の経路を持たない、
承認 UI はプレビュー+質問+三択)と、OpenUI の設計は**前提が異なる**点を
まず区別する必要がある。OpenUI は「**LLM 自身が毎ターン UI 記述コードを
書く**」モデルであり、Horizon が今向かっているのは「**Horizon 自身の
view コードが、既存の provider event(`agent::contract::Event`)を解釈
して描画する**」モデルである。前者を丸ごと輸入すると、エージェントに
新しい DSL をシステムプロンプトで教え込む必要が生まれ、これは transcript
描画の再設計の範囲を超えた製品判断になる。

輸入する価値があるのはコード/構文ではなく発想:

- **ツール結果の構造化描画**: 現状 Horizon はツール出力をほぼ生テキスト/
  markdown として transcript に流し込む。ツールが返す表形式データを
  `Table`/`Col` 相当の Horizon 製コンポーネントで描画すれば、`agent-ui.md`
  が指摘する「縦密度が低い」「生 raw の経路を持たない」という2つの痛点
  に同時に効く可能性がある。これは**エージェントが UI を書く**のではなく
  **Horizon が既知のツール出力スキーマから構造化ビューを組む**話であり、
  低リスクに輸入できる。
- **診断メッセージを LLM へのフィードバックとして設計する**という
  パターン(`unknown-component`/`missing-required` 等、`statementId`+
  `hint` 付きの構造化エラーを "eslint --fix" のように LLM へ返す設計)は、
  Horizon が将来ホスト定義の構造化フォーマットをエージェントに渡す場面
  (例: 設定エージェントのツール引数検証)で参考になる汎用パターン。
- **過剰と判断できるもの**: リアクティブ状態(`$variable`)・
  `Query`/`Mutation` の自動再フェッチ・フォームバリデーションといった
  「エージェントが手を離れた後もUIが自走するアプリ」を作る仕組みは、
  Horizon の transcript(会話の記録)という文脈には過剰。OpenUI のこの層
  は「チャットの中に小さな独立アプリを埋め込む」ためのもので、Horizon
  が今解こうとしている「読みやすい実況ログ」とはゴールが違う。

### (2) 信頼境界との相性 — 検証結果

`docs/trust-boundaries.md` の第1層(エージェント産プラグイン=信頼しない
=wasm)は「エージェントが実行可能コードを書く」ことを前提に wasm を選んで
いる。OpenUI Lang が**本当にコード実行を伴わないか**をソースで検証した
結果:

- **事実**: `evaluator.ts` はクローズドな12種のASTノードしか評価せず、
  `eval`/`Function`/動的 import 等は存在しない。`Action` の5ステップも
  固定 enum。`Query`/`Mutation` はホスト側 `toolProvider` が実行するため、
  実行そのものはエージェント/DSL の外側にあり、Horizon の
  `ToolCallRequested` と同じ「モデルは提案するだけ、実行の可否と実体は
  ホストが握る」構造と一致する(`docs/agent-provider-contract.md` の
  Permission Boundary と同型)。
- **事実として残る穴**: `@OpenUrl` はドキュメント上、ホスト確認なしで
  任意 URL を新規タブで開く仕様として書かれている(承認ゲートの記述が
  ない)。Horizon がこの種の「UI 経由の副作用」を持ち込む場合、
  `ApproveToolCall`/`DenyToolCall` と同じ許可フローに載せる必要がある
  ("Permission checks happen in Horizon before a requested operation is
  executed" という既存原則の再確認であり、OpenUI 側の設計にこの配慮は
  ない)。
- **含意**: 「宣言的 UI スペックはコード実行を伴わない」という本調査の
  仮説は、少なくとも OpenUI Lang の実装においては**成立している**。
  したがって、将来 Horizon が「エージェントが UI(構造化ビュー)を提案
  できる」機能を検討する場合、そのビュー記述が(a) クローズドな式評価器
  で解釈され、(b) 副作用は既存のツール許可境界を必ず通る、という2条件
  を満たす限り、`docs/trust-boundaries.md` 第1層の wasm ほど重い隔離は
  不要という主張には根拠がある。ただし「対象がエージェント*生成コード*
  ではなく Horizon 自身が組み立てるビュー」である現状の適用先(上記(1))
  では、この論点は目下 wasm 判断とは無関係(第1層の対象はまだ「プラグイン
  としての実行コード」であり、transcript のツール結果描画とは別の階層)。

### (3) floem での実現性

**事実**: OpenUI Lang の「仕様」と「レンダラ」は元々分離されたアーキテク
チャになっている。`lang-core`(パーサ・プロンプト生成・評価器、フレーム
ワーク非依存の純粋 TypeScript)の上に `react-lang`/`vue-lang`/
`svelte-lang`/`browser-bundle` という複数のレンダラバインディングが乗る
構成が実在する。ただし**これはすべて TypeScript** であり、Horizon
(Rust/floem)がそのまま呼び出せるコードは存在しない。かつて存在した
Rust 実装は WASM 化を含めて完全に削除済み(`.rs` ファイル 0件)。

**重要な留保**: blog "Rewriting our Rust WASM Parser in TypeScript" の
結論(「TypeScript の方が速かった」)は、**ブラウザの JS↔WASM 境界のコピー
/シリアライズコストに起因する**もので、この前提は Horizon には存在しない
(floem はネイティブ Rust プロセス内で完結し、JS ヒープと WASM 線形メモリ
の往復は発生しない)。したがって同記事を「Rust でこの種のパーサを書くのは
不利」という一般則として読むのは誤りで、Horizon がネイティブ Rust で
パーサ+評価器+レンダラを実装する場合にこの記事の教訓は適用されない。

**実現性の見積り**: 移植するとすれば「仕様の発想」のみで、コードは流用
できない。参考にした TS 実装のファイル規模は evaluator.ts 483行、
parser.ts 683行、builtins.ts 228行(lexer/statements/merge/serialize等
別ファイル多数)— 六段パイプライン+評価器を Rust で再実装する規模は
Horizon 既存の `src/agent/view/markdown.rs` の複数倍程度と見積れる、
決して小さくないが「多段の設計相談が要る大投資」というほどでもない
中規模の実装作業。Zod スキーマ駆動のプロンプト自動生成という仕組み自体
は Rust では素直に持ち込めず(`serde`/`schemars` 等で類似の「スキーマ
からプロンプトを生成する」層を自作する必要がある)、ここは「発想
(スキーマ=API契約=プロンプト源泉の単一化)」だけを借りる形になる。

### (4) プロバイダ契約への含意(概念レベル)

OpenUI の AG-UI イベント語彙(`TEXT_MESSAGE_START/CONTENT/END`,
`TOOL_CALL_START/ARGS/END/RESULT`, `RUN_ERROR`)は、Horizon の
`agent::contract::Event`(`MessageCommitted`, `ToolCallRequested`,
`ToolCallStarted/Finished` 等)と同じ高さの「プロバイダ↔UI 正規化イベント」
として設計されている点で構造的に近い。ただし OpenUI には「UI 記述」専用
のイベント種別は**存在しない** — 生成された OpenUI Lang テキストは通常の
アシスタントテキストの一部としてストリームに乗り、レンダラがクライアント
側で事後的にパースする。この事実は Horizon にとって示唆的で、もし
将来「構造化 UI ブロック」を扱う場合でも**新しい `Event` variant を追加
する必要はないかもしれない**という選択肢を裏付ける。
`docs/agent-provider-contract.md` が `tool_call_progress` を既存
`ProviderEvent` エンベロープに便乗させた理由(新しい `Event` variant は
永続ログスキーマへの確定コミットになるため)と同じ理由付けが、UI ブロック
にも転用できる可能性がある。逆に、Horizon 自身が(1)で述べた「ツール結果
の構造化描画」を選ぶなら、これは `ToolCallFinished`/`MessageCommitted`
の**解釈(=view 層の仕事)**でしかなく、契約そのものには触れずに済む
— 「契約に触れる変更」と「view 層だけの変更」のどちらの高さで着手する
かは、この2つの事実を踏まえてドメインセッションが選ぶ論点になる。設計
そのものは本稿の範囲外。

---

## Part 3: 判定

**「そのまま採る」は事実上不成立。** npm パッケージ/AgentInterface は
React・AG-UI・OpenUI Cloud という周辺一式込みの製品であり、Rust/floem
ネイティブの Horizon に持ち込む経路(FFI・埋め込みブラウザ等)がない。
仮に持ち込んでも、Horizon の JSONL 正本+イベントソーシングとは異なる
スレッド/ストレージモデルや、pre-1.0 の依存(AG-UI 0.0.x)、商用アップ
セル導線(OpenUI Cloud)を抱え込むことになり、割に合わない。

**現実的な選択肢は「仕様の発想だけ借りる」。** 借りる価値が高い順に:
(a) 「宣言的スペック(閉じた評価器+ホスト管理の副作用実行)はコード実行
たり得ない」という設計とその実装(ソース検証済み)— trust-boundaries.md
の第1層判断に転用できる具体的根拠、(b) ツール結果を構造化コンポーネント
として描画する発想 — agent-ui.md の縦密度/edit可読性の痛点に直接効く
可能性、(c) 構造化エラーを LLM フィードバック用に設計するパターン。
借りないもの: 構文そのもの、npm パッケージ、Vue/Svelte 相当のマルチ
フレームワーク対応(Horizon は floem 一本)、リアクティブ状態/自走アプリ
としてのチャット内 UI というゴール設定(Horizon の transcript は記録で
あり、独立アプリの器ではない)。

判断はドメインセッションの段階2設計相談に委ねる。選択肢と得失は上の
Part 2 各節に事実ベースで記載した — 特に(2)信頼境界の検証結果と(3)
floem 実現性の「Rust 不利」誤読の訂正が、今後の設計判断で誤って前提に
されないよう明示した。

---

## 参照 URL 一覧

- https://github.com/thesysdev/openui (README, ADOPTERS.md, LICENSE)
- https://openui.com (公開ドキュメントサイト、リポジトリ内 mdx ソースから直接読了)
- https://www.letta.com/blog/... 等は本調査の対象外(参照は `docs/research/letta.md`)
- リポジトリ内主要参照ファイル:
  - `docs/content/docs/openui-lang/{specification-v01,specification-v05,how-it-works,evolution-guide,renderer,interactivity,queries-mutations,standard-library,comparison,troubleshooting}.mdx`
  - `docs/content/docs/agent/core-concepts/{generative-ui,tools,artifacts}.mdx`
  - `docs/content/docs/agent/reference/{adapters-and-formats,self-hosting}.mdx`
  - `docs/content/docs/agent/getting-started/{introduction,openui-cloud}.mdx`
  - `docs/content/blog/{rust-wasm-parser,stop-making-ai-write-json,beyond-the-chatbar}.mdx`
  - `packages/lang-core/src/runtime/evaluator.ts`(ソース検証: eval/Function 不在の確認)
  - `packages/react-ui/src`(コンポーネント語彙の抽出)
  - GitHub API: `repos/thesysdev/openui`, `.../contributors`, `.../tags`, `.../commits`(スター数・contributors・コミット数・作成日)
