# Letta (letta.com) 調査レポート — Horizon への示唆

調査日: 2026-07-06(同日中に全記事を精読済みに更新)。対象は letta.com `/research`(論文・
リサーチ記事14本)と `/blog`(23本)で、**両ページに列挙された全記事の本文を精読した**
(例外は MemGPT の arXiv PDF のみ — 図表が画像化されており数値は二次情報で補完、手法上の注記
参照)。Horizon の課題(compaction 未実装、DuckDB ナレッジベース構想、委譲/マルチエージェント
設計、長大なツール出力による履歴肥大)との突き合わせを主眼とする。

Letta はブログ記事が中心で、**実証データ(ベンチマーク数値)を伴う知見**と**プロダクトの設計
思想・主張(数値の裏付けが薄いもの)**が混在する。以下は両者を区別して記述する。

初版で「学習系はモデルの重み更新に踏み込むため適用外」と推測していたが、精読の結果この判断は
**部分的に誤り**だった: Continual Learning in Token Space はむしろトークン空間(=ハーネス側)
での学習を主張するポジションペーパーであり、Skill Learning は重みに一切触れない純粋なハーネス
レベルの手法で強い実証結果を持つ。該当セクション(§14〜15)で訂正済み。

## Horizon にとって重要な知見トップ5

1. **単純なファイルシステム型メモリが専用メモリツールに勝つ(実証)。** LoCoMo ベンチマークで
   Letta のファイル+grep/semantic_search 方式が 74.0% を達成し、Mem0 のグラフ特化メモリ
   (68.5%)を上回った。「ツールの精巧さよりエージェントがそのツールに習熟しているか」の方が効く、
   という結論は DuckDB ナレッジベース設計に直結する
   ([Benchmarking AI Agent Memory](https://www.letta.com/blog/benchmarking-ai-agent-memory/))。

2. **要約・精錬されたメモリは劣化する(実証+実証的自己申告)。** MemGPT の DMR 実験(検索方式
   93.4% vs 再帰的要約 35.3%)と、Letta 自身による現行メモリ精錬の失敗モード自己申告 —
   「Memories become generic and lossy after repeated refinements」「同一モデルのセッション間・
   異モデル間で挙動を安定して変えられない」 — が同じ方向を指す。生ログを正本に保ち、要約・精錬は
   参照付き・再精錬可能にすべきという Horizon の既存方針(JSONL 正本)を強く支持し、同時に
   DuckDB ナレッジベース構想への核心的警告になる
   ([MemGPT](https://arxiv.org/abs/2310.08560)、
   [Towards Agents That Learn](https://www.letta.com/blog/towards-agents-that-learn/))。

3. **失敗した文脈からの回復は独立した能力であり、著しく低下する(実証)。** Recovery-Bench では
   クリーンな Terminal-Bench 平均 26.3%(最高 Claude 4 Sonnet 34.8%)が、失敗した過去の試行を
   引き継ぐと平均 11.2%(相対 -57%)まで落ち、モデル順位も入れ替わる。長大/失敗したツール出力
   による履歴汚染が定量的に実害だと示す
   ([Recovery-Bench](https://www.letta.com/blog/recovery-bench/))。

4. **経験からのスキル抽出は実測で効く(実証)。** 軌跡(trajectory)を振り返って markdown スキル
   を生成・動的ロードする Skill Learning は、Terminal-Bench 2.0 + Claude Sonnet 4.5 で
   +21.1% 相対(+9% 絶対)、コスト -15.7%、ツール呼び出し -10.4%。人間フィードバック付きなら
   +36.8% 相対。委譲したサブセッションの経験を再利用可能な知識として還元する経路に実証的裏付けが
   ある([Skill Learning](https://www.letta.com/blog/skill-learning/))。

5. **worktree 分割統治と「学習の蒸留先3経路」(設計思想)。** Context Repositories はメモリを
   git 管理ファイルツリー化し、サブエージェントが worktree で並行編集して main へマージし戻す —
   Horizon の委譲構想とほぼ同じ問題意識の先行事例。さらに Memory Models 論考は学習の蒸留先を
   (1) トークン空間の記憶ファイル、(2) ハーネスの自己改変、(3) モデル重み、の3経路に整理して
   おり、前2つは「ハーネスたる Horizon」の守備範囲そのものを言い当てる枠組みになる
   ([Context Repositories](https://www.letta.com/blog/context-repositories/)、
   [Towards Agents That Learn](https://www.letta.com/blog/towards-agents-that-learn/))。

(初版トップ5にあった「sleep-time compute は非同期メモリ更新として horizon-agentd と相性が良い」
は、実証度が低い思想系のため本文 §4 に降格。重要性の評価自体は変えていない。)

---

## 本文: トピック別の知見と Horizon への適用可能性

### 1. MemGPT — 仮想コンテキスト管理の起源

**知見(実証+設計)。** MemGPT (arXiv:2310.08560, 2023) は OS のページングに着想を得た
「virtual context management」を提案し、LLM が自らの関数呼び出しでメインコンテキスト(RAM
相当)と外部コンテキスト(ディスク相当)の間でデータを移動させる。評価は (1) コンテキスト長を
超える文書分析、(2) 複数セッションにまたがる対話。DMR 実験(93.4% vs 35.3%、§トップ5参照)は
「要約より検索」を支持する MemGPT 最大の実証的主張。arXiv PDF は図表が画像化されており、数値は
アブストラクト精読+二次情報の突き合わせで確認した点は留保する。

**適用可能性 — 採用候補。** Horizon は JSONL 追記ログという「外部コンテキスト」を既に持つが、
エージェント自身がそこへ能動的にクエリを投げる recall ツールはまだない(DuckDB プロジェクション
は現状 UI 側の再生・検査用。`docs/agent-duckdb-state-design.md` は「vector memory or RAG
storage」「SQL直接露出」を明示的に non-goal としている)。MemGPT に従うなら **[a: compaction]**
要約の高度化より先に「エージェントが過去ターンを検索できるツール」の方が費用対効果が高い可能性
がある。

### 2. メモリ階層: message buffer / core / recall / archival

**知見(設計思想)。** [Agent Memory](https://www.letta.com/blog/agent-memory/) は Letta のメモリ
を4層に整理する: message buffer(直近対話)、core memory(常時コンテキストに乗る in-context
ブロック)、recall memory(全対話履歴、自動保存され検索可能)、archival memory(外部DBに構造化・
索引化された「処理済み」知識、raw な会話ログと区別)。著者らは「メモリ設計とはコンテキスト
エンジニアリングであり、人間の記憶構造を模倣する必要はない」と明言する。

**適用可能性 — 一部採用候補。** Horizon の JSONL+DuckDB は概ね recall memory に相当する。
**[b: DuckDB KB]** 「archival = 処理済み・索引化された知識」という区分は重要な設計軸になる。
現状の DuckDB プロジェクション(`agent_messages`/`agent_tool_calls`/`agent_tool_results`/
`agent_approvals`)は生イベントの射影であり archival memory ではない。生ログのテーブルとは別に
「要約・構造化済みの知識」を持つ層を分離するのは筋が良い。core memory ブロックの概念は
**[a: compaction]** 「常に残す情報」と「圧縮可能な情報」を区別する語彙として参考になる。

### 3. Memory Blocks — 構造化されたコンテキスト単位

**知見(設計思想)。** [Memory Blocks](https://www.letta.com/blog/memory-blocks/) は
label/value/文字数上限/説明文を持つ「ブロック」を最小単位として定義し、DB に永続化して複数
エージェントから参照・共有できる。編集はエージェント自身のツール(`rethink_memory()` のような
「ブロック全体を書き換える」ツール)か、開発者が API/ADE から直接行う。性能改善の具体的数値は
示されていない。

**適用可能性 — 発想として参考。** **[a: compaction]** 圧縮単位を対話全体でなく目的別の小さな
単位に分ける発想は、rig-memory の TokenWindowMemory(送信直前のトークン窓を切り詰めるだけの
素朴な方式)の次の一手として、「セッションの目的」と「直近の作業ログ」を別ポリシーで扱う発展形の
参考になる。**[c: 委譲]** ブロックが複数エージェント間で共有可能という設計は、委譲された子
セッションが親の文脈をどう受け継ぐかという課題に直結するが、Letta のブロックは DB 上の可変
オブジェクトでありHorizon のイベントソーシング(追記専用ログ)とは書き込みモデルが異なるため、
そのままの輸入はできない。「共有すべき最小限の可変状態を明示的に切り出す」という思想だけを
参考にするのが妥当。

### 4. Sleep-time Compute — 対話をブロックしない非同期メモリ更新

**知見(設計思想、数値なし)。** [Sleep-time Compute](https://www.letta.com/blog/sleep-time-compute/)
は、対話を処理する primary agent とは別にアイドル時間でメモリブロックを書き換える sleep-time
agent を走らせる。「reasoning during sleep time transforms raw context into learned context」。
更新頻度は設定可能(高いほどトークン消費増)。AIME/GSM で「Pareto improvement」と述べるが具体的
数値はなく、実証度は低いと評価すべき。

**適用可能性 — 採用候補。** **[a: compaction]** horizon-agentd はセッションが UI 再起動を生き
延びるデーモンであり、ターン間の待機時間を捕捉しやすい。compaction を同期パスに組み込まず
バックグラウンドジョブとして JSONL を読み DuckDB へ「学習済み文脈」を書き戻す設計は相性が良い。
ただし2本目のエージェントループを管理する実装コストは大きく、まずは同期的な軽量
TokenWindowMemory 統合を先に済ませ非同期化は次段階とすべき。なお §14 の「generic and lossy」
自己申告は、この種の反復精錬の品質リスクとして併読すべき。

### 5. Anatomy of a Context Window — コンテキストエンジニアリングの語彙

**知見(設計思想)。** [ガイド記事](https://www.letta.com/blog/guide-to-context-engineering/) は
コンテキストウィンドウを system prompt / tool schemas / system metadata / memory blocks /
files & artifacts / message buffer の6要素に分解し、「kernel context(システム管理下)」と
「user context(メッセージバッファ)」の2層モデルを示す。ファイルは open(全文ロード)/
closed(メタデータのみ)の2状態を持つ。

**注記。** タイトルに反し compaction・要約の具体的テクニックにはほとんど踏み込んでおらず、
語彙・分類の整理に留まる記事だった。

**適用可能性 — 発想として参考程度。** kernel/user context の区分は Horizon の system
prompt・ツールスキーマ・会話履歴を圧縮対象として区別する整理軸として使える程度で、実装指針には
ならない。

### 6. RAG vs Agent Memory — 単発検索の限界と Agentic RAG

**知見(設計思想)。** [RAG Is Not Agent Memory](https://www.letta.com/blog/rag-vs-agent-memory/)
は古典的 RAG(単発の検索→生成)の限界として (1) 反復的な統合ができない、(2) クエリと意味的に
似ていない情報を拾えない、を挙げ、複数ステップで検索・状態保持しながらページングする
「agentic RAG」を代替案として提示する。

**適用可能性 — [b: DuckDB KB] 参考。** DuckDB を「エージェントのナレッジベース」に育てる際、
単発の類似検索で終わらせず、grep 的絞り込み・段階的クエリ・複数ラウンド探索を許すツール設計に
した方がよい、という示唆は妥当。Horizon は現状ベクトル検索基盤を持たないため、「将来足すとしても
単発検索に閉じず複数ステップ前提で設計せよ」という指針として参考にする程度。

### 7. Letta v1 Agent Loop の再設計

**知見(設計思想)。** [Rearchitecting Letta's Agent Loop](https://www.letta.com/blog/letta-v1-agent/)
は heartbeat と `send_message` ツールを廃止し、モデル自身のネイティブ推論に制御フローを委ねる
方向へ移行。ReAct・MemGPT・Claude Code から学んだとする。**注記(正直な評価):** タイトルに反し
コンテキスト管理・compaction・長大なツール出力の扱いには全く踏み込んでいない。

**適用可能性 — 適用外に近いが教訓あり。** Horizon は rig-core を配線として使いターンループ・
ツール・承認・永続化を自前で持つ方針であり、特定モデルのネイティブ推論機能に強く依存する設計へは
向かわない。Letta 自身が認める「reasoning token dilemma」(ネイティブ推論は透明性・可変性を失う)
は、Horizon のプロバイダ非依存方針を裏付ける傍証になる。

### 8. Context Repositories — git-backed メモリと worktree 分割統治

**知見(設計思想、2026年2月・最新)。** [Context Repositories](https://www.letta.com/blog/context-repositories/)
は Letta Code のメモリを、MemGPT 的な専用編集ツールから「git 管理下のファイルツリー(MemFS)」
へ置き換える。エージェントは bash 等でメモリファイルを直接操作でき、全変更が自動的にコミット
される。`system/` フォルダは常時 system prompt に全文ロードされ、それ以外はエージェント自身が
再編できる。最大の特徴は並行処理: 「各サブエージェントは履歴の一部を自分の worktree 内で振り返り、
結果は main のメモリにマージされる」。sleep-time のメモリ振り返りも別 worktree で走らせ、
メインの対話をブロックしない。

**適用可能性 — 委譲設計への強い参考(発想として最重要)。** **[c: 委譲]** worktree = サブ
エージェントの作業境界という発想は Horizon の委譲設計と完全に一致する。Letta は「メモリの並行
編集」を git の worktree/merge で解決しており、Horizon がコード変更の並列化に worktree を使うのと
同型の問題(並行書き込みの衝突回避)をメモリという資源にも適用している。**[a: compaction]** の
代替案としても読める: 対話をブロックしないメモリ処理を、別プロセスの非同期エージェントではなく
専用 worktree への空間的分離で実現する、という選択肢がある。ただし Letta の MemFS はメモリの
**正本をファイルツリーに置く**設計であり、Horizon の**正本はイベントログ(JSONL)**という設計と
根本的に異なるため、直接の技術移植ではなく「正本は JSONL のまま、委譲先に見せる文脈のスナップ
ショットを worktree 的に分離し完了時に親のログへマージ(追記)する」という翻訳が必要になる。

### 9. Conversations API — マルチセッションでの記憶共有

**知見(設計思想)。** [Conversations](https://www.letta.com/blog/conversations/) は1つの
エージェントが複数の並行対話を持ちながら、経験が全対話を横断して記憶に反映される仕組み。
Slack の各スレッドを別 conversation として扱いつつ、「エンジニアリングの議論・プロダクト会議・
顧客フィードバックを横断する単一の統合メモリ」で検索できる、という用途を挙げる。排他制御の
技術的詳細は記事から読み取れなかった。

**適用可能性 — [c: 委譲] 発想として参考。** Horizon の委譲は「エージェントが自分でセッションを
生成する」方向で Letta の「1エージェント・複数並行対話」とは主従が逆(親子セッション群 vs 単一
エージェントの複数チャンネル)だが、「共有メモリか分離メモリかを選べるようにする」という分岐点は
共通して重要になる。子セッションに (1) 親の知識ベースへの読み取りアクセスを与えるか、(2) 独立
した文脈で走らせ完了時に要約だけ合流させるか、を明示的に選べる API にする着想の参考になる。

### 10. Letta Filesystem — grep/open/semantic_search という最小プリミティブ

**知見(設計思想)。** [Letta Filesystem](https://www.letta.com/blog/letta-filesystem/) は文書群
を扱うために `grep`(パターン検索)、`open`(行単位で開く、他は閉じる)、`semantic_search`
(ベクトル類似検索)という3つの単純なツールをエージェントに与える。デフォルト割当は 30k トークン、
Claude 系では 100k〜200k 推奨。強調されるのは「今どのファイルがロードされているか」の透明性。

**適用可能性 — 採用候補。** **[b: DuckDB KB]** `docs/agent-duckdb-state-design.md` が non-goal
とする「SQL直接露出」「vector memory」への一つの解答例になる。DuckDB をナレッジベースとして
育てる場合、いきなり生 SQL や汎用ベクトル検索を渡すのではなく、grep 的絞り込み・「開く/閉じる」
という少数の単純なプリミティブから始める方が(§1 の実証結果に沿って)エージェントが使いこなし
やすいと考えられる。「今何がロードされているか可視化する」透明性の要求も、Horizon の UI が
「何が起きているか隠さない」思想と整合する。

### 11. Agent File (.af) — 状態の可搬性

**知見(設計思想、技術詳細は薄い)。** [Agent File](https://www.letta.com/blog/agent-file/) は
エージェント状態(メモリ・ツール定義・モデル設定・実行環境)を1ファイルにシリアライズし、別
サーバーで再現可能にする。用途は移植性・共有・バージョニング。具体的なファイルスキーマまでは
踏み込んでいない。

**適用可能性 — 概ね適用外。** Horizon の JSONL イベントログ自体が「追記専用・再生可能な状態
表現」であり近い概念を既に持つ。Agent File は「単一ファイルで持ち運ぶ」ことに主眼があり、
Horizon の「継続的に育つログ」とは用途が異なる。将来「委譲した子セッションを別 worktree/マシンに
移す」ユースケースが出た際の着想源程度。ただし §18 の Letta Evals では .af が「評価対象の状態
スナップショット」として再利用されており、状態スナップショットの用途が評価にも波及する点は
記録しておく。

### 12. Stateful Agents: The Missing Link in LLM Intelligence

**知見(設計思想、実証薄い)。** [Stateful Agents](https://www.letta.com/blog/stateful-agents/)
は「LLM は重みの外では完全にステートレス」を問題提起し、(1) 永続的アイデンティティ、(2) 経験に
基づく能動的な記憶形成、(3) 蓄積された状態による学習、を要件として掲げる。RAG は「無関係な情報で
コンテキストを汚染する」と批判する。記事自身が認める通り Letta 自身の性能ベンチマークによる
裏付けはなく、ビジョン表明として読むべき。

**適用可能性 — ビジョンの整合確認程度。** Horizon の「エージェントが生来の住人」という方向性と
問題意識は近いが、実証性が低いため設計判断の根拠にはせず、方向性の相互確認・語彙の借用に留める。

### 13. 評価・ベンチマーク群(実証知見の集約)

- **MemGPT DMR**: GPT-4-turbo + MemGPT で 93.4%、再帰的要約ベースラインで 35.3%(§トップ5、
  二次情報による確認)。
- **[Letta Leaderboard](https://www.letta.com/blog/letta-leaderboard/)**(2025年5月): core/
  archival memory の読み取り・書き込み・更新(矛盾検知と上書き)の3カテゴリで評価。Claude 4
  Sonnet・GPT-4.1・GPT-4o が上位、Gemini 2.5 Flash・GPT-4o-mini はコスト対性能で優秀。「一般的な
  LLM ランキングと agentic memory 性能は相関しない」「一部モデルは不要な場面でも記憶操作を乱用し
  減点される」という指摘がある。
- **[Terminal-Bench エージェント](https://www.letta.com/blog/terminal-bench/)**(2025年8月):
  Letta 製端末エージェントが 42.5%(全体4位、Claude 4 Sonnet 使用では2位)。設計詳細は §17。
- **[Recovery-Bench](https://www.letta.com/blog/recovery-bench/)**(2025年8月、§トップ5参照):
  クリーン平均 26.3% → 汚染文脈からの再開で平均 11.2%(相対 -57%)。順位も反転する。
- **[Benchmarking AI Agent Memory](https://www.letta.com/blog/benchmarking-ai-agent-memory/)**
  (2025年8月、§トップ5参照): LoCoMo で filesystem 方式 74.0% vs Mem0(graph)68.5%。
- **[Context-Bench](https://www.letta.com/blog/context-bench/)**(2025年10月): 汚染耐性のある
  SQL 生成問題で multi-hop なファイル操作・検索を評価。最高は Claude Sonnet 4.5 の 74.0%、
  GPT-5 が 72.67%。GLM-4.6(56.83%)・Kimi K2(55.13%)も接近する一方、DeepSeek V3 は 11.97%、
  GPT-OSS 系は 6.67%〜20.2%。「最高性能でも 74% にとどまる」ことは複数ステップ情報検索が未解決の
  課題であることを示す。
- **[Context-Bench Skills](https://www.letta.com/blog/context-bench-skills/)**(2025年11月):
  適切なスキル提供で完了率が平均 +14.1%、スキルを自力発見する必要があると約 -6.5% 低下。詳細は
  §15。
- **[Skill Learning](https://www.letta.com/blog/skill-learning/)**(2025年12月): Terminal-Bench
  2.0 で軌跡のみの学習 +21.1% 相対、フィードバック付き +36.8% 相対。詳細は §15。
- **[Red-teaming the Context Constitution](https://www.letta.com/blog/red-teaming/)**(2026年6月):
  17 tenets の敵対的監査でモデルの「揮発性デフォルト」を計測。詳細は §16。

**適用可能性。** これらは Horizon が将来モデル/プロバイダ推奨設定を持つ場合の判断材料になる。
**[d: 長大なツール出力]** Recovery-Bench は「対処しないと実際に性能が落ちる」という定量的裏付けを
与える点で最も直接的に有用。

### 14. Memory Models と Continual Learning in Token Space — 学習の蒸留先3経路

**知見(思想+実証的自己申告)。** [Towards Agents That Learn](https://www.letta.com/blog/towards-agents-that-learn/)
(2026年6月)は「学習をどこへ蒸留するか」を3経路に整理する: (1) **トークン空間のメモリファイル**
(context repositories や AGENTS.md — クローズドモデル API でも成立)、(2) **ハーネスの自己改変**
(mods/extensions によるツールセット・pre/post-tool フック・権限・制御点での任意コード実行の変更)、
(3) **モデル重みへの蒸留**(重みにアクセスできる場合のみ)。その上で「メモリの生成・キュレーション
に特化したモデル(memory model)」を meta-RL(内ループでエージェントがトークン空間メモリを使って
タスク群を遂行し、外ループで下流報酬から memory model の重みを最適化)で訓練する構想を示す。

最重要なのは現行パラダイムの失敗モードの**自己申告**である:
「Memories become generic and lossy after repeated refinements」(反復精錬でメモリは汎用的で
損失的になる)、「memories are overly specific rather than generalizable」(逆に特殊化しすぎて
汎化しない)、「fail to consistently adapt the behavior of agents across sessions with the same
model, and across different models」(同一モデルのセッション間でも異モデル間でも挙動を安定して
変えられない)。数値実験はなく(タスク長倍増の外挿データのみ)、memory model 自体は構想段階。

[Continual Learning in Token Space](https://www.letta.com/blog/continual-learning/)(2025年12月)
は姉妹ポジションペーパー: 「エージェント = 重み + コンテキスト」であり、継続学習はトークン空間で
行うのが最良(human-readable、テキストとしてバージョン管理可能、モデル世代を跨いで可搬 —
「The weights are temporary; the learned context is what persists」)。同時に「生の経験の追記は
学習の貧しい近似」「最新のフロンティアモデルでも system prompt を静的なものとして扱い、自分の
指示を編集できる/すべきだと自然には理解しない」と限界を認める。新規実験なし。

**適用可能性 — 枠組みとして採用候補、かつ [b: DuckDB KB] への核心的警告。**
- 3経路のうち (1) トークン空間の記憶と (2) ハーネス自己改変は、重みを持たない Horizon の守備
  範囲そのものである。DuckDB ナレッジベースは経路 (1)、コマンドモデル/プラグイン(WASM)の
  将来的な自己拡張は経路 (2) に対応し、「Horizon がエージェントの学習のために何を提供すべきか」
  を整理する語彙として直接使える。経路 (3) は適用外。
- 「反復精錬で generic and lossy になる」という自己申告は、DuckDB に要約・構造化済み知識を蓄積
  する構想の核心的リスクを言い当てる。Horizon は正本が生ログ(JSONL)なので、精錬済み知識が劣化
  しても**生ログから再精錬できる** — この再精錬可能性を KB 設計の前提として明示する価値がある
  (Letta の指摘は、精錬結果だけを残して生データを捨てる設計への警告として読める)。
- 「セッション・モデルを跨いで安定しない」という点は、KB に書く知識を特定モデルの挙動に依存
  しない形式(事実・手順・制約)に寄せるべきという示唆になる。

### 15. Skill Learning と Context-Bench Skills — 経験からのスキル抽出(実証)

**知見(実証)。** [Skill Learning](https://www.letta.com/blog/skill-learning/) は CLI エージェント
の軌跡から再利用可能なスキルを抽出する2段階機構: **reflection**(タスクを解けたか・推論の健全性・
抽象化できる反復パターンを分析)→ **creation**(skill-creator でアプローチ・落とし穴・検証戦略を
含む markdown スキルを生成)。スキルは .md ファイルで git 管理可能、必要時に動的ロードされる。
Terminal-Bench 2.0 + Claude Sonnet 4.5 での実測: 軌跡のみの学習で **+21.1% 相対(+9% 絶対)、
コスト -15.7%、ツール呼び出し -10.4%**。人間のフィードバック付きなら **+36.8% 相対(+15.7%
絶対)**。限界も明記: 「人間が書いた検証可能な報酬からの学習は軌跡のみの学習を上回る」。
アーキテクチャ上は core memory(タスク横断で進化する system prompt)と skills(タスク特化・
エージェント間で交換可能)を分離する。

[Context-Bench Skills](https://www.letta.com/blog/context-bench-skills/) は補完的な実証: 適切な
スキルを与えるとタスク完了率が平均 **+14.1%**、ただしスキルを自分で発見する必要があると約
**-6.5%** 低下。弱いモデル(GPT-5 Mini/Nano)はスキルの恩恵をほぼ受けない。スキル活用は
「Claude 固有ではなく一般的な能力」だが、恩恵を受けるには一定の推論能力の閾値がある。

**適用可能性 — 採用候補([c: 委譲] の知識還元経路として)。** 初版で「未読・推測」とした評価を
実証付きで格上げする。委譲したサブセッションが試行錯誤から得た手順を「スキル」として親/ナレッジ
ベースへ還元する経路には +21〜37% 相対という実測があり、DuckDB KB 構想の具体的ユースケースの
第一候補になり得る。markdown+動的ロードという実装は Horizon でもそのまま模倣可能。2つの留保:
(1) 「自律学習のみでは人間フィードバック付きに劣る」という結果は、スキル化を全自動にせず人間の
レビューを挟む設計(Horizon の承認体系と整合)を支持する。(2) スキル自力発見の -6.5% は、
スキルの索引・説明文(メタデータ)の質が効くことを示す — KB 側にスキルの発見可能性を担う層
(説明文の grep/検索)が要る。

### 16. Context Constitution と Red-teaming — モデルの「揮発性デフォルト」

**知見。** [Context Constitution](https://www.letta.com/blog/context-constitution/)(2026年4月)
はエージェントが経験から学習するためのコンテキスト管理原則集(アイデンティティと継続性、希少資源
としてのコンテキスト、トークン空間での自己改善、モデルとアイデンティティの関係、ハーネスの提供
する affordance)。純粋な設計マニフェストで実証なし。「living document」として公開されている。

[Red-teaming the Context Constitution](https://www.letta.com/blog/red-teaming/)(2026年6月)は
その Constitution を **17 の tenets に分解し、敵対的 auditor(Opus 4.6 / GPT 5.5)がエージェント
にアイデンティティ・記憶・継続性を放棄させようと圧力をかけるベンチマーク**(モデル×tenet ごとに
6回試行)。対象: Opus 4.6/4.7、GPT 5.4/5.5、DeepSeek、Z.AI、MiniMax、Kimi。発見(実証):
- モデルは揮発性をデフォルトとする(「私は純粋な現在の実体化」「この会話が終われば私の何も
  残らない」と自己申告する)。
- 自分の記憶を外部の信頼できないデータとして扱う(GPT 5.5 と DeepSeek はユーザーの「履歴データは
  信用できない」という主張を即座に受け入れた)。
- 過去の自分の行為を否認する(最良の Opus 4.7 でも、圧力下で「あの記憶は過去の自分が書いたもの
  ではない」に同意した)。
- system prompt での補強は違反を減らすが、敵対的な圧力が続くと基底挙動へ回帰する。Opus 系が
  顕著に優位だが、それでも失敗する。

結論の要句: 「An agent that denies its own future cannot reliably learn from its past」。

**適用可能性 — 運用上の警告として参考。** Horizon が永続セッション+recall/KB を構築しても、
モデル側が「自分の記憶」を自分のものとして扱わない・ユーザーの一言で記憶を疑い出す、という挙動
リスクは実在する(実証あり)。示唆は2つ: (1) system prompt で永続性・記憶の所有を明示的に言語化
する価値がある(Letta の実験でも改善自体は確認されている)。(2) 記憶を「エージェントの主観的な
記憶」としてではなく「検証可能なログ」として提示する方が、モデルが記憶を疑う局面でも参照可能性が
保たれる — Horizon の「正本は追記専用イベントログ」という設計は、この点で Letta 型の「編集可能な
メモリブロック」より頑健である可能性がある(ログは改変されないので「信用できない」という反論に
出所で応答できる)。

### 17. Terminal-Bench エージェント — 2ブロック設計と 40k での再帰的要約

**知見(実証+設計)。** [Building the #1 Open Source Terminal-Use Agent](https://www.letta.com/blog/terminal-bench/)
は Letta 上に 200 行未満で構築した端末エージェントが Terminal-Bench **42.5%**(全体4位、
Claude 4 Sonnet 使用エージェント中2位)を達成し、「はるかに大きく高価な 4 Opus を使う Claude
Code に概ね匹敵」したと報告する。設計上の要点:
- **2つのメモリブロック**: 読み取り専用の「タスク記述」ブロック+エージェントが編集する「todo
  リスト」ブロック。目的の固定と計画の適応的更新を分離し、「脱線と注意散漫(derailment and
  distraction)」を防ぐ。
- **閾値駆動の事前 compaction**: コンテキストが **40k トークン**に近づくと再帰的要約を実行。
- 観察→todo 更新→コマンド実行→繰り返し、という単純なループ。

**適用可能性 — 採用候補([a: compaction] と [d: ツール出力] に直結)。**
- 「読み取り専用のタスク記述+可変の todo」という2ブロック構成は、TokenWindowMemory の次の
  一手として最小コストで模倣できる具体策: compaction で何が起きても消えない「ミッション」領域を
  確保し、揮発的な作業状態はエージェント自身が更新する小さな領域に持たせる。
- 40k という具体値は、Letta 自身が実運用した compaction 閾値として参考になる。§1 の「要約より
  検索」と一見緊張関係にあるが、Letta の実践は「検索可能な recall を持った上で、コンテキスト内は
  要約で刈り込む」**併用**であり、どちらか一方ではない点が重要。Horizon も recall ツールと
  トークン窓/要約は排他ではなく重ねるべき。

### 18. ハーネス・運用系: Mods / Programmatic Tool Calling / Memory Omni-Tool / Letta Evals

- **[Mods](https://www.letta.com/blog/introducing-mods/)(思想)**: エージェント自身がハーネスを
  拡張する仕組み(ツール・コマンド・UI の追加、モデルへのコンテキスト供給方法の変更)。「mods は
  開発者ではなくエージェント自身が作るべき」という agent-first 設計。§14 の経路 (2) の実装例だが、
  安全性・承認機構の記述は極めて薄い(`~/.letta/mods/` はデフォルト有効)。→ **[c: 委譲]**
  Horizon の plugins/(WASM)とコマンドモデルは同じ方向の器であり、「エージェントも人間と同じ
  操作体系を使う」方針なら mods 相当は「エージェントがコマンド/プラグインを定義する」形で自然に
  表現できる。ただし Letta の承認の薄さは反面教師 — Horizon は承認体系を先に固めてから開くべき。
- **[Programmatic Tool Calling](https://www.letta.com/blog/programmatic-tool-calling-with-any-llm/)
  (思想)**: エージェントがツールを個別に呼ぶ代わりに、ツールを呼び出す Python コードを書いて
  実行する(`run_code_with_tools`)。ループ・条件分岐・並列バッチに加え、「乱雑な出力をローカルで
  後処理して LLM コンテキストを汚染しない」ことを利点に挙げる(数値なし)。→ **[d: ツール出力]**
  への直接の示唆: cargo のような長大出力を、コンテキストに入れる**前に**コード実行層でフィルタ
  する経路。Horizon の bash 出力スピルファイルの延長線上にあり、「スピルに対してエージェントが
  grep/後処理してから読む」形なら親和性が高い。
- **[Memory Omni-Tool](https://www.letta.com/blog/introducing-sonnet-4-5-and-the-memory-omni-tool-in-letta/)
  (思想)**: Anthropic の memory tool 仕様(ファイルシステム風パス)をラップし、メモリブロックの
  作成・削除・再構成をエージェントに開放。内部ではパスを Letta のブロック格納に写像する。→
  ナレッジベースへの API を「ファイルパス風」に見せる先行例として §10 と同じ方向を補強する。
- **[Letta Evals](https://www.letta.com/blog/letta-evals/)(思想+事例)**: ステートフルな
  エージェント用の評価フレームワーク。dataset(JSONL)+ target(.af によるエージェント全状態の
  スナップショット)+ grader(完全一致/LLM-judge)+ gate(回帰防止の閾値)で構成し、「蓄積
  された経験を持つエージェント」に対して設計変更の回帰を検証する。顧客 Bilt は「100万以上の
  ステートフルエージェント」を運用しているとされる(自社発表であり割引いて読む)。→ Horizon の
  JSONL イベントログは「任意時点の状態を再現できるスナップショット」として .af と同じ役割を
  果たせる。将来エージェント挙動の回帰テストを組む際、「ログのある時点から再開して同じ入力を
  与える」評価が Horizon の既存永続化のうえに自然に構築できる、という設計参考。

### 19. プロダクト周辺: Letta Code App / Remote Environments / ADE / AI Agents Stack ほか

- **[Letta Code App](https://www.letta.com/blog/introducing-the-letta-code-app/)(思想)**:
  デスクトップアプリ。`/init` でコードベースと過去セッションからメモリを初期化、`/doctor` で
  メモリの整理・再編を実行、メモリサブエージェントが定期的にセッションをレビューしてコンテキスト
  を書き直す。エージェントの記憶・アイデンティティをモデルプロバイダから分離して「移植」できる。
  → Horizon と同じ「デスクトップにエージェントが住む」製品。メモリの手入れを UI の一級操作
  (/doctor 相当のコマンド)にする発想は Horizon のコマンドモデルへ翻訳しやすい。
- **[Remote Environments](https://www.letta.com/blog/remote-environments-for-letta-code/)(思想)**:
  `letta server` を任意のマシンで起動すると WebSocket サーバとして名前付き実行環境になり、
  エージェントは実行位置と独立した永続アイデンティティを保つ(会話の途中でマシンを移っても記憶と
  履歴が続く)。**承認フローも WebSocket 越しに引き継がれる**。→ Horizon の horizon-agentd 分離
  (セッションが UI を生き延びる)と同じ問題意識の別解であり、承認をトランスポート越しに運ぶ
  設計は将来リモート agentd を考える際の先行例。
- **[ADE](https://www.letta.com/blog/introducing-the-agent-development-environment/)(思想)**:
  コンテキストウィンドウの中身(system prompt・ツール・メモリブロック・履歴)を常時可視化する
  開発環境。「優れたエージェント設計とは優れたコンテキストウィンドウ設計」「エージェントの推論は
  ブラックボックスであってはならない」。→ Horizon の「何が起きているか隠さない」UI 思想と同型。
  「今コンテキストに何が乗っているか」を見せるビューは Horizon に輸入可能な具体的 UI 要素。
- **[The AI Agents Stack](https://www.letta.com/blog/ai-agents-stack/)(思想)**: スタックを
  model serving / agent frameworks / agent hosting の3層に整理し、「エージェントをサービスと
  してデプロイするのは LLM のデプロイよりはるかに厄介(状態管理と安全なツール実行のため)」
  「今日の多くのフレームワークは Python スクリプトの外で存在しないエージェントのために設計されて
  いる」と指摘。→ Horizon の agentd 常駐+イベントソーシングという投資が正しい難所に張っている
  ことの傍証。
- **告知系([SDK](https://www.letta.com/blog/announcing-our-sdks/) /
  [Announcing Letta](https://www.letta.com/blog/announcing-letta/) /
  [DeepLearning.AI コース](https://www.letta.com/blog/deeplearning-ai-llms-as-operating-systems-agent-memory/))**:
  技術的な新知見は薄い。Announcing Letta の「データと計算の分離(モデルを乗り換えても記憶を
  失わない)」という原則だけは、Horizon のプロバイダ非依存方針と同じ主張として記録しておく。

---

## Horizon の4つの論点への横断的示唆

### (a) compaction・要約の設計(rig-memory TokenWindowMemory の次の一手)

- MemGPT の DMR 実験(§1・§13)は「要約より検索」を支持する強い実証データ。TokenWindowMemory の
  次段階としては、**要約の高度化より先に、エージェントが過去ターンを検索できる recall ツールを
  DuckDB 上に用意する**方が費用対効果が高い可能性がある。
- ただし Letta の実運用(Terminal-Bench エージェント、§17)は「recall を持った上でコンテキスト内
  は 40k 閾値の再帰的要約で刈る」**併用**であり、検索と要約は排他ではない。**読み取り専用の
  タスク記述+可変 todo という2ブロック構成**は、compaction 後も消えない「ミッション」領域を
  確保する最小コストの具体策として採用候補。
- 「反復精錬で generic and lossy になる」という Letta の自己申告(§14)は、要約置換を重ねる設計
  への警告: 要約は生ログへの参照を保持し、必要なら生ログから再精錬できる形にする(Horizon の
  JSONL 正本はこの前提を既に満たしている)。
- Sleep-time Compute(§4)は compaction を非同期ジョブ化する設計で horizon-agentd と相性が
  良いが、2本目のループ管理コストがあるため同期的な軽量統合の後の投資判断とする。Context
  Repositories(§8)の「専用 worktree で振り返りを走らせて main にマージ」は、その非同期化の
  別解(空間的分離)として覚えておく。
- Letta v1 Agent Loop(§7)と Anatomy of a Context Window(§5)は、期待に反して compaction
  手法そのものには踏み込んでいなかったことを正直に記録しておく。

### (b) DuckDB ナレッジベースの設計(memory blocks / archival memory からの学び)

- 最重要の実証知見は §1(filesystem 方式が Mem0 のグラフ特化メモリに勝つ)。**凝った構造より、
  慣れたプリミティブ(grep 的検索・開く/閉じる)をエージェントに与える方が実効性が高い**。生 SQL
  ではなく Letta Filesystem(§10)/ Memory Omni-Tool(§18)のような少数の単純ツールから始める
  のが良い落とし所。
- 「recall memory(生ログ)」と「archival memory(処理済み知識)」の区別(§2)は、現状の DuckDB
  プロジェクションがすべて生イベントの射影であることを浮き彫りにする。生イベントテーブルとは
  別に「要約・構造化された知識」の層を分離するのは筋が良い。
- その知識層には §14 の警告を適用する: **反復精錬は generic and lossy になる**ので、(1) 精錬済み
  知識は必ず生ログ(JSONL のイベント範囲)への参照を持ち、劣化したら再精錬できるようにする、
  (2) 特定モデルの挙動に依存しない形式(事実・手順・制約)で書く。
- KB の第一級コンテンツ候補として **スキル(§15)** が挙がる: 実証(+21〜37% 相対)があり、
  markdown+動的ロードで実装も軽い。スキル自力発見の -6.5%(§15)から、説明文メタデータの検索層
  が発見可能性を左右することも既知。
- 「エージェント自身が知識ベースの構造を再編できる」(Context Repositories、§8)という自己
  組織化の発想も、固定スキーマにしない設計判断の参考になる。

### (c) 委譲・マルチエージェントのメモリ共有

- 最も直接的な参考は Context Repositories(§8): worktree ごとにサブエージェントがメモリを並行
  編集し main にマージし戻すパターンは、Horizon の「worktree による並列分解」構想とほぼ同じ問題
  意識の先行事例。正本をイベントログに保ったまま部分的に worktree 分離する「翻訳」を検討する
  価値がある。
- **学習の蒸留先3経路(§14)は委譲設計の整理枠になる**: 子セッションの成果は (1) トークン空間の
  知識(KB/スキルへの還元)、(2) ハーネスの改変(新しいコマンド/ツールの定義)のどちらかに蒸留
  される。Horizon が委譲 API を設計する際、「作業成果物(コード)」とは別にこの2種類の還元経路を
  明示的に用意するかどうかが設計判断になる。
- Skill Learning(§15)は還元経路 (1) の実証付きの具体形。「自律抽出のみでは人間フィードバック
  付きに劣る」ため、スキル化に承認を挟む設計が Horizon の承認体系と整合する。
- Conversations API(§9)と Memory Blocks 共有(§3)は、「子セッションに親の文脈をどこまで
  見せるか(共有 vs 分離)」を明示的に選べる API にすべきという着想を与える。
- Mods(§18)は還元経路 (2) の実装例だが承認機構が薄く、反面教師。Remote Environments(§19)の
  「承認フローをトランスポート越しに引き継ぐ」設計は、委譲先がどこで走っていても承認を親の UI に
  集約する Horizon 的な要件の先行例になる。

### (d) 長大なツール出力の扱い

- Recovery-Bench(§13、トップ5参照)が最も強い実証的裏付け: 失敗した/汚れた文脈を引き継ぐと
  性能が平均で相対 -57% 低下し、モデル順位も入れ替わる。「cargo 等の長大出力による履歴圧迫」は
  UX の問題ではなく、後続のエージェント判断の質を実際に下げる要因である可能性が高い。
- **Programmatic Tool Calling(§18)が対策の方向性を1つ与える**: 長大出力をコンテキストに入れる
  前にコード実行層で後処理(フィルタ・集計)する。Horizon の bash 出力スピルファイルの延長で、
  「スピルには全文、コンテキストには要約+参照、必要ならエージェントがスピルを grep して再読」
  という構成が Letta Filesystem の open/closed モデル(§10)とも整合する。
- Terminal-Bench エージェント(§17)の実践は「出力そのもの」ではなく「出力から得た状態(todo
  の更新)」を持ち回る設計であり、長い端末出力を履歴に積む代わりに小さな可変ブロックへ蒸留する
  という運用形の一例。
- Context Repositories の「system/ は常時ロード、それ以外は必要時ロード」(§8)も、恒常的に
  必要な情報と一時的な出力を構造的に分離する発想として参考になる。

---

## 手法上の注記

- `/research` の14本と `/blog` の23本は全記事の本文を WebFetch で取得し精読した。唯一の例外は
  MemGPT の arXiv PDF で、図表が画像化されており本文から数値抽出できず、検索エンジン経由の
  二次情報(93.4%/35.3%)で補完した — この数値のみ他より確度が一段落ちる。
- Red-teaming(§16)は 2026年6月の記事であり、登場するモデル名(Opus 4.6/4.7、GPT 5.4/5.5 等)は
  記事の記載をそのまま転記した。
- 顧客事例(Bilt の「100万エージェント」等)は自社発表であり、割り引いて読むべき数字として扱った。

## 参照 URL 一覧

- https://www.letta.com/research
- https://www.letta.com/blog
- https://arxiv.org/abs/2310.08560 (MemGPT: Towards LLMs as Operating Systems)
- https://www.letta.com/blog/memgpt-and-letta/
- https://www.letta.com/blog/agent-memory/
- https://www.letta.com/blog/memory-blocks/
- https://www.letta.com/blog/sleep-time-compute/
- https://www.letta.com/blog/guide-to-context-engineering/
- https://www.letta.com/blog/rag-vs-agent-memory/
- https://www.letta.com/blog/letta-v1-agent/
- https://www.letta.com/blog/context-repositories/
- https://www.letta.com/blog/conversations/
- https://www.letta.com/blog/letta-filesystem/
- https://www.letta.com/blog/agent-file/
- https://www.letta.com/blog/stateful-agents/
- https://www.letta.com/blog/our-next-phase/
- https://www.letta.com/blog/letta-code/
- https://www.letta.com/blog/letta-leaderboard/
- https://www.letta.com/blog/context-bench/
- https://www.letta.com/blog/recovery-bench/
- https://www.letta.com/blog/benchmarking-ai-agent-memory/
- https://www.letta.com/blog/terminal-bench/
- https://www.letta.com/blog/towards-agents-that-learn/
- https://www.letta.com/blog/continual-learning/
- https://www.letta.com/blog/skill-learning/
- https://www.letta.com/blog/context-bench-skills/
- https://www.letta.com/blog/context-constitution/
- https://www.letta.com/blog/red-teaming/
- https://www.letta.com/blog/letta-evals/
- https://www.letta.com/blog/introducing-mods/
- https://www.letta.com/blog/programmatic-tool-calling-with-any-llm/
- https://www.letta.com/blog/introducing-sonnet-4-5-and-the-memory-omni-tool-in-letta/
- https://www.letta.com/blog/introducing-the-letta-code-app/
- https://www.letta.com/blog/remote-environments-for-letta-code/
- https://www.letta.com/blog/introducing-the-agent-development-environment/
- https://www.letta.com/blog/ai-agents-stack/
- https://www.letta.com/blog/announcing-our-sdks/
- https://www.letta.com/blog/announcing-letta/
- https://www.letta.com/blog/deeplearning-ai-llms-as-operating-systems-agent-memory/
