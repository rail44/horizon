# Compaction 手法の証拠調査 — 「LLM 要約は最良か」への答え（2026-07-28）

オーナーの問い「そもそも要約の手法として優れているのか。ベストプラク
ティス・ベンチマーク・研究レポートを調査したい」への回答。Opus worker
3 系統（vendor 一次資料 / OSS 実装・実測 / Anthropic・Factory・Letta 系
+ 統合）による web 調査。全主張に URL・アクセス日（2026-07-27/28）・
証拠強度ラベル付きで収集済み（詳細は各報告、work notes は job tmp・
揮発）。本 doc は決定に効く核心のみを固定する。

## 1. 統制された実測は「要約 ≒ 単純マスキング」を示す（最重要）

**arXiv 2508.21433**（"The Complexity Trap"、DL4C @ NeurIPS'25、
SWE-bench Verified 500 問・5 モデル構成）【査読あり（workshop）】:
- 「古い観測を placeholder に置換するだけ」の observation masking が、
  LLM 要約と**解決率で同等・コスト同等以下**。「pure LLM summarization
  へのトレンドに懸念を提起する」と明記
- 唯一明確に分離した構成（Gemini 2.5 Flash thinking）では**両手法とも
  無管理より悪く、LLM 要約が最悪**（40.4 → 31.4、−22.3%）
- 彼らの LLM-Summary 腕は「要約 + 直近 10 turn 原文維持」— まさに
  検討中だった OpenCode 型そのもの

**OpenHands の vendor 実測**（blog 2025-04-09 + PR #6597）【vendor 実測・
仕様記載弱い】: condenser は解決率ほぼ不変（200 vs 203/500）。**本当の
効果はレイテンシ**（往復ごとの二次関数的増加 → 平坦化、iter 100 で
8s vs 16s）。

**Anthropic 自身の公表数値は全て「機械的 clearing + memory」のもの**
（context editing 単独 +29%、memory 併用 +39%、100-turn eval で消費
−84%。claude.com/blog/context-management 2025-09-29）。**LLM 要約
compaction の実測は一切公表していない**。公式 doc の順序も「古い tool
出力の clearing が先、要約は必要なら後」。

## 2. 要約が確実に失うのは、コーディングで最も重要な情報クラス

- **Factory.ai の 3 実装比較**（factory.ai/news/evaluating-compression、
  2025-12-16。実運用 36,611 メッセージ、盲検 LLM judge）【vendor 実測、
  自社優位の COI・下流タスク成功率なし】: Factory 3.70 / Anthropic 3.44 /
  OpenAI 3.35。ただし決定的なのは **artifact trail（どのファイルを
  どう変えたか）が全実装 2.19〜2.45/5** — 誰も要約で解けていない
- Anthropic 自身の cookbook probe: 「high-level 3/3 保持、obscure 0/3」
- **Governance Decay**（arXiv 2606.22528、1,323 エピソード×7 モデル族）
  【査読なし preprint】: 制約が要約に**残れば違反 0%、落ちれば 38%**
  （平均 30%、最大 59%）。失敗は理解でなく**eviction**。訓練不要の
  Constraint Pinning（保護領域の隔離）で 0% に完全回復

## 3. field の収斂形: 「復元可能な削減が先、lossy 要約は最後」

- **Manus**: 「どの観測が 10 手先で重要になるか予測できない。不可逆
  圧縮は論理的に必ずリスク」→ 圧縮は常に復元可能（URL/path を残して
  本文を落とす）、lossy 要約は限界後の二段目（webinar 経由・二次資料）
- **Claude Code**: clearing → 要約の順を公式明記。保護は要約への信頼
  でなく**ディスクからの再注入**（CLAUDE.md・memory・skill 本文）
- **LangChain Deep Agents**: >20k tool 結果は「ファイル参照 + 先頭
  10 行」に置換、85% で古い tool call を pointer 化、要約は fallback
- **Amp**（68 回 compaction の実運用）: read_thread に「compaction は
  方向付けのみ。正確な要件・コード・時系列は原文を見に行け」
- Letta 現行 docs: sliding_window 30% を安価な別モデルで要約 + recall
  併用（「retrieval **instead of** summarization」ではない）。
  なお letta.md の DMR 数値 93.4%/35.3% は arXiv 原文 Table 2 で確認
  できた（留保解除）。ただし DMR はセッション横断の逐語想起課題であり
  mid-task 継続の証拠ではない、という regime 限定は維持

## 4. 失敗モードのカタログ（防御設計の輸入元）

- Gemini CLI issue 履歴: 要約が**膨張**（+15%）、**毎ターン圧縮ループ**、
  **一番必要な時に失敗**（63% 時点で maxOutputTokens エラー）、heap OOM。
  防御: 膨張したら破棄・失敗ラッチ後は LLM を呼ばず切り捨てのみ・
  2 パス自己検証（probe）・ペア分断禁止
- Codex issue #14589: 反復 compaction で 13.7% → 6.9% 保持、「バグ修正の
  実体やテスト失敗の根因が消えた」。Hermes #499 も「summaries of
  summaries」を既知問題として記録
- 閾値はどこも無根拠（Gemini は 0.7→0.2→0.5 と迷走。churn を tag 遡行で
  実証）。Claude Code の公式確認値は「1M 窓で ~967K で発火」のみ
- **cache 経済の重要な訂正**: 要約リクエストが**会話と同一 prefix
  （同一 system prompt・同一モデル）を共有すれば cache read で安い**
  （Anthropic 公式）。異なる prompt で要約すると全履歴を非 cache 単価で
  払う。Manus の KV 論は「別 prompt での書き換え」に刺さるのであって、
  prefix 共有型 compaction には bounded cost

## 5. 証拠の空白（自分で測るしかない領域）

- compaction vs 新セッション handoff の下流成功率比較: **皆無**
- **参照付き要約 + fetch 可能な生ログ** vs 要約単独: 4 実装が収斂する
  形なのに**比較測定ゼロ** — Horizon が recall を持つ以上、ここは
  自前で測る価値がある
- 反復 compaction の品質推移: 逸話のみ
- 測定手法の借用元: LangChain は評価時に**閾値を 10〜20% に強制**して
  compaction を意図的に多発させる

## 6. 証拠の重み（統合結論）

LLM 要約は「安全で普通の**最終段**」としては支持される（全 vendor が
出荷、セッションは生き続ける）。**一次手段としては支持されない**:
in-regime の統制比較は単純機構との parity（+1 明確な負け）、実測数値の
あるのは clearing+memory 側、要約が落とすのはコーディングで最重要の
artifact trail と制約、そして pinning が完全に効く。

実証的に最も支持される構成要素は要約そのものではなく:
**(1) 要約前の機械的 tool 出力 clearing、(2) 原文維持の直近 tail、
(3) 保護領域はディスク再注入/pinning で要約を信頼しない、(4) user
メッセージの保存、(5) 再要約でなく iterative 更新、(6) 要約呼び出しの
prefix 共有**。
