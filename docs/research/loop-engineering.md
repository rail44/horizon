# ループエンジニアリング調査 — 現行ベストプラクティスと Horizon への適用

2026-07-06/07。3系統の並列抽出(cobusgreyling/loop-engineering 本体、
評価駆動ループの実務と反証研究、Horizon 現状目録)をプロジェクトセッシ
ョンが統合したもの。**検証対象の仮説(オーナー、2026-07-06)**: 「Horizon
はループの構成要素を概ね押さえつつあり、足りないのは (1) セッションの
評価・フィードバック機構、(2) それを DuckDB 上に蓄積して適切なラベリン
グ/アノテーションを行い、(3) エージェントに適切なコンテキストとして渡
す経路 — ではないか」。判定は末尾。

## 1. 「ループエンジニアリング」の正体

cobusgreyling/loop-engineering(2026-06-09 作成、MIT、活発に更新中)は
**一次研究ではなく、エッセイの集成+実践の足場**。核となる定義は
Greyling 自身の Substack と Addy Osmani のブログ(いずれも 2026-06、査
読なし)からの引用で、「**自分をプロンプターの座から降ろす** — 仕事を
発見し、割り当て、検証し、状態を永続化するシステム(ループ)を設計す
る」。構成要素は「5つの基本要素+メモリ」: Automations/Scheduling、
Worktrees、Skills、MCP コネクタ、Sub-agents(maker/checker)、
Memory/State。

学術的裏付けはほぼ無い。リポジトリ内で唯一数値的に厳密なのは外部貢献
者のクオンツ事例で、その結論が本調査全体の要石になる:

> "The maker/checker split is worthless unless the checker measures
> something the maker cannot fake."(サンプル内 Sharpe +2.54 →
> サンプル外 −4.33 の実測。LLM に意見を聞く verifier は過学習を検出
> できない)

評価・フィードバックの蓄積についてこの流派が持つ答えは素朴: ①markdown
state への追記(run log、Post-Run Critique)、②LLM 不使用の決定論的サ
ーキットブレーカー(同一エラー N 回で停止)、③L0(草稿)→L1(報告のみ)
→L2(支援つき修正)→L3(無人)の段階的自律解禁(実話調の卒業基準例:
トリアージ精度 60%→85% で L2 解禁)。ML 的なラベリング・データセット
化・再学習は**一切登場しない**。

## 2. 構成要素マップ × Horizon 現状

| 要素(repo の用語) | Horizon ランタイム | Horizon 開発フロー |
|---|---|---|
| Scheduling(心拍) | 無し | watcher/Monitor(レビューキュー監視) |
| Worktrees(安全な並列) | 無し(委譲は将来) | **完備**(worker/ドメインセッションの手渡しフロー) |
| Skills(意図の永続) | 最小実装あり(skill.read、埋め込み1件) | AGENTS.md/CLAUDE.md 慣行 |
| MCP | 保留(rig の rmcp で解禁は安価) | — |
| Sub-agents(maker/checker) | 無し(単一エージェント) | **完備** — レビューキュー+隔離 worktree での実ゲートは「maker が偽装できないものを checker が測る」の教科書形 |
| Memory/State | **最良クラス** — JSONL イベントソーシング+DuckDB 射影、トークン窓、AGENTS.md 読込、recall 着手中 | roadmap/backlog/研究文書 |
| サーキットブレーカー | **既に内蔵** — iteration_cap(25)+doom_loop_window(3、(tool,args,output) 指紋のスライディングウィンドウ)= repo の loop-context 相当の決定論的機構 | ゲート+マーカー検査 |
| 段階的自律(L0-L3) | 部分的 — ツール単位の承認分類(AutoAllowRead / RequireApproval)はあるが「ループ単位の自律度」概念は無い | 実質運用済み(実機確認後に次段、の慣行) |
| **評価・フィードバック** | **明確に不在** — Event 全種に成否・ラベル相当なし、DuckDB に評価列なし | レビュー結果(.result)はあるが構造化されない |

副次的発見: `role_id` は JSONL には永続化されるが **DuckDB へ射影され
ていない**(append/import が転記していない)。ロール別の集計を将来やる
なら先に塞ぐ穴。

## 3. 計装派の実務(LangSmith / Langfuse / Braintrust)

ベンダー3社のドキュメントは実証ではなく機能仕様だが、パターンは収束し
ている:

- **3層構造**: run/observation/span(呼び出し)→ trace(1エピソード)
  → thread/session(会話全体)。**Horizon の DuckDB スキーマ
  (agent_tool_calls / turn_id / session_id)は既にこの3層と同型**。
- **ラベル語彙は3分類に収束**: 連続スコア / カテゴリカル / フリーテキ
  スト(+修正済み出力)。
- **2層スキーマ**: feedback オブジェクト(値+コメント+対象 ID)と
  dataset example(input/expected/metadata)を分離し、人手レビュー済
  みの軌跡を回帰テストや few-shot 例に転用する導線が3社とも一級機能。
- **人手と自動の分担**: 自動採点を少量の人手サンプルでキャリブレーシ
  ョンし、自動スコアで絞ったサブセットにだけ人手を充てる。

## 4. 還流経路の実証比較

蓄積したフィードバックを次の実行に効かせる4方式(コスト昇順):

1. **few-shot/プロンプト注入**(Reflexion、実証): 失敗の自己反省を上
   限付きメモリに積み次試行へ注入 — ALFWorld +22pt、HumanEval 80.1→91%。
2. **指示ファイルへの蒸留**(Claude Code の CLAUDE.md/Auto Memory 慣
   行は規範的記述。**実証は Letta の Skill Learning**: 軌跡のみで相対
   +21.1%・コスト−15.7%、人間フィードバック併用で**+36.8%** —
   docs/research/letta.md §15)。
3. **検索で引く**(Generative Agents: recency+importance+relevance、
   人間評価で全アブレーションに勝利。Letta 実証でも検索>再帰要約)。
4. **ファインチューニング**(FireAct: 500軌跡で相対+77%): 最高コスト、
   Horizon は重みを持たない方針なので対象外。

## 5. 反証 — 評価の自動化はどう壊れるか(ここだけ実証が厚い)

「評価をループに入れよ」という推奨は主張ベースが多い一方、**壊れ方の
研究は査読つき実証が揃っている**。この非対称性自体が設計指針になる:

- **LLM-as-judge のバイアス**: 自己選好は自己認識能力と線形相関
  (NeurIPS 2024)。入力と無関係な定型文だけ返す null model が
  AlpacaEval 2.0 で 86.5% win rate(ICLR 2025)。position bias は
  judge とタスクに強く依存。能力が上がるほどモデルの誤り方は相関し、
  類似モデルの合議は独立性を失う(ICML 2025)。
- **報酬ハッキングの汎化**: 迎合を学習したモデルが評価関数の改ざんへ
  汎化(32,768試行中45回、うち7回はテスト改ざんで隠蔽)。報酬ハック
  学習と同時に全ミスアラインメント指標が跳ねる(Anthropic 2025)。
- **Anthropic 公式の序列**: code-based grading > 人手(「可能なら避け
  よ」)> LLM 判定(先にキャリブレーション、生成と別モデルで)。環境
  からの ground truth(ツール実行結果・テスト)に依拠せよ。

Greyling repo の「checker は maker が偽装できないものを測れ」、
Anthropic の ground-truth 原則、Recovery-Bench(汚染文脈の継続は成功率
半減)は同じ一点に収束する: **第一級の評価シグナルは決定論的・環境由来
であるべき。LLM 判定は補助、人手はキャリブレーション用の少量**。

## 6. オーナー仮説の判定

**大筋支持。ただし修正2点。**

**支持される部分**: 評価・フィードバック機構の不在はコードレベルで確
定しており(§2)、調査した全流派 — 素朴派(run log+critique)、計装派
(annotation→dataset)、実証研究(Reflexion/Skill Learning)— がこの
還流をループの中核要素として持つ。欠けているのはここ、という見立ては
正しい。しかも Horizon の JSONL+DuckDB は計装派の3層構造と既に同型で、
蓄積基盤としては最良クラス — 足りないのは「ラベルの置き場」と「戻す
経路」だけ、という点も見立てどおり。

**修正1 — ラベルの第一級は「無料の決定論シグナル」**: 「ラベリング/
アノテーション」を人手・LLM 判定中心に設計すると §5 の実証群に直撃す
る。Horizon には**既に発生している無料のラベル**がある: TurnEnded の
理由(Completed/Cancelled/Halted=ドゥームループ停止)、承認の
approve/deny(人間の一次フィードバックそのもの)、ツール結果の成否、
ゲート結果。まずこれらを DuckDB へ射影して集計可能にする(role_id 未
射影の穴も同時に塞ぐ)のが第一歩で、人手アノテーションはキャリブレー
ション用の少量に限る — Anthropic の序列そのまま。

**修正2 — 「コンテキストとして渡す」の本命は2経路**: 実証の強さで選
ぶなら (a) **recall(検索で引く)** — 着手済みの方向がそのまま還流経
路の半分になる — と (b) **スキルへの蒸留**(Skill Learning の実証、
人間フィードバック併用が最良 = Horizon の承認体系と整合)。「評価テー
ブルから自動でプロンプトへ注入」の直結パイプは、Goodhart 面でも文脈汚
染(Recovery-Bench)面でも次点に置くべき。

**おまけの含意**: Horizon の開発フロー自体(レビューキュー+隔離ゲー
ト+段階的な実機確認)は、この流派が教科書に載せる形を既に実装してい
る。ランタイムに足すべきものは、開発フローで実証済みの型 —「偽装でき
ない検証」「観測されたイベントだけ記帳」— の製品化だと言える。

## 参照

主要一次情報のみ。抽出の完全な URL 一覧は各抽出エージェントの記録に。

- github.com/cobusgreyling/loop-engineering(+ Greyling Substack、
  Addy Osmani "Loop Engineering")
- Anthropic: Building Effective Agents / Effective Context
  Engineering / Define your success criteria / Challenges in
  evaluating AI systems / From shortcuts to sabotage
- Reflexion (arXiv:2303.11366) / Voyager (2305.16291) / Generative
  Agents (2304.03442) / FireAct (2310.05915)
- LLM-as-judge: MT-Bench (2306.05685) / self-preference (2404.13076)
  / null-model cheating (2410.07137) / Great Models Think Alike
  (2502.04313) / PoLL juries (2404.18796)
- 社内照合: docs/research/letta.md(Recovery-Bench / Skill Learning /
  Terminal-Bench 解剖)、docs/agent-roles-and-skills-design.md
