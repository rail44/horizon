# シェルコマンド承認の証拠調査 — 判定は LLM、境界は grant 形状(2026-08-17)

board #38 の設計議論(bash の書き方で judge 到達が変動するのはおかしい →
判定は LLM に全文を読ませ、決定論的保証は sandbox の grant 形状に置く、
という収束案)に対し、オーナーの要求「直感でなく外部の証拠を」を受けた
調査。2並行リサーチ(①既存実装のアーキテクチャ、②攻撃実例と LLM judge
信頼性の定量)を統合。規約: 数値・出典に**年代と検証力**([一次=code/docs]
/ [査読] / [CVE/advisory] / [独立レポート] / [ベンダー] / [推測])を併記。
社内の先行調査 `agent-approval-prior-art-2026-07-19.md` の上に積む位置づけ。

## 結論(要約)

1. **収束案と同型の設計が Claude Code と OpenAI Codex の両方で本番稼働
   している** — 「LLM がフルコマンドを読み、決定論的保証は sandbox の
   書き込み許可形状(特に `.git/hooks`・`.git/config` の書き込み拒否)」。
   Claude Code の sandbox docs はほぼ同一文言(worktree の共有 `.git` は
   書けるが hooks/ と config は拒否)[一次=docs]。Codex は writable root
   内でも `.git` と解決済み gitdir を read-only に再バインド[一次=code]。
2. **決定論的シェル解析を安全境界にする路線は実証的に脆い**: Gemini CLI
   (`;` 以降を無審査 — Tracebit 2025-07)、Cursor(バッククォート/`$()`
   CVE-2025-54131、環境変数・シェル組込 CVE-2026-22708、brace expansion)
   [CVE/advisory]。denylist 一般も 69–98.6% が回避可能(arXiv 2606.15549、
   1,709 denylist の実分析)[査読前]。
3. **LLM judge を唯一の境界にもできない**: 判定対象コンテンツ内への
   注入で高率に倒れる — 最適化攻撃で平均 90.8%(JudgeDeceiver、CCS 2024
   [査読])、実ガードレール最大 100% 回避・検知率 38–84%(ACL 2025
   LLMSec [査読])。
4. したがって**二層(判定=LLM、境界=決定論的 grant 形状)が文献と実装の
   両方が支持する唯一の形**。git メタデータがホスト昇格ベクタである
   前提も CVE で堅い(下記)。

## 1. 既存実装の実態

| 実装 | 判定層 | 決定論層の役割 | git メタデータ |
|---|---|---|---|
| Claude Code auto mode | 二段分類器が raw payload を読む(tool result は分離 — 注入対策)[一次=docs] | ルールは deny→ask→allow。**auto 移行時に広域 allow(`Bash(*)` 等)を自動破棄**。解析は複合コマンドのセパレータ認識まで | 権限層 protected paths + **sandbox 層で `.git` 内 hooks/config/HEAD/objects/refs を書き込み拒否。allow ルールでも解除不可** [一次=docs] |
| OpenAI Codex | Guardian(LLM)に「正確な planned action」全文+方針文書、strict JSON、**fail-closed**、連続 denial 上限 [一次=code] | tree-sitter で厳密解析するが**「安全側を広げるためだけ」**(全サブコマンドが read-only なら auto)。危険側の保証は解析に依存しない | sandbox が `.git`(dir/file/gitdir ポインタ/bare 全形態)を既定 read-only [一次=code] |
| OpenHands | LLM が自アクションに `security_risk` を自己注釈(追加コールなし) | Pattern/PolicyRail の決定論 analyzer 併存 | 明文規定なし |
| Cursor / Gemini CLI | (旧)決定論 allowlist/denylist が境界 → **CVE の温床** | — | Cursor は「分類器は便宜でありセキュリティ境界ではない」と明記 |

## 2. git メタデータ = ホスト昇格ベクタの証拠

- 古典: CVE-2018-11235(`.gitmodules` 経由で hooks 書き込み→実行)、
  CVE-2024-32002(CVSS 9.0、clone 中に hooks 実行)、CVE-2024-32004
  [CVE/advisory]。
- 設定駆動: `core.fsmonitor` 悪用(justinsteven 2022 [独立]、Cobalt
  red-team 手法)、**CVE-2026-45033(GitHub Copilot CLI)**: ネストした
  bare repo の fsmonitor が status/diff で発火 — AI エージェント文脈の
  実 CVE。修正は `safe.bareRepository=explicit` の強制 [CVE/advisory]。
- 2026 のエージェント sandbox 脱出集中(Pillar「Week of Sandbox
  Escapes」、Cymulate CBSE 類型化 — Claude Code 自身も CVE-2026-25725
  `.claude/settings.json` フックで被弾)[独立レポート]。一般法則:
  **「sandbox 内の writer が、sandbox 外の reader に実行される設定を
  書けると境界に穴が空く」**。`.git/hooks`・`.git/config` はその git 具体例。

## 3. 実装上の既知の落とし穴(重要)

- **リテラルな `.git` パス一致だけでは破られる**: Cursor「Git
  directories do not have to be called .git」(fix v3.0.0)— 別名 git
  dir・bare repo 自動検出を fsmonitor 経由で突かれた [独立レポート]。
  対策の実例: Codex は **gitdir ポインタを解決した先**を read-only 化、
  Copilot は `safe.bareRepository=explicit`。Horizon の実装もパス名で
  なく「git が実際に読む場所」を対象にすること。
- judge 側の防衛の実例: 判定モデルに敵対的コンテンツ(tool result・
  ファイル内容)を渡さない(Claude Code)、fail-closed + denial 上限
  (Codex Guardian)。
- 決定論解析を捨てる必要はない — Codex 型の「**安全側を広げるためだけ
  に使う**」(全セグメント read-only なら judge すら不要)は有効。

## 4. 定量(LLM judge の信頼性)

- JudgeDeceiver(CCS 2024)[査読]: 最適化注入で平均 90.8% 成功、
  Claude-3 Sonnet へ 88% / GPT-4 へ 79% 転移。
- ガードレール回避(ACL 2025 LLMSec)[査読]: 文字注入・emoji smuggling
  で最大 100%。検知率 Azure Prompt Shield 58.9% / Protect AI v2 84.2%。
- CARE(ISSRE 2026)[査読]: 決定論優先+境界ケースのみ LLM エスカレー
  ション — F1 85.6%、FP 0.91%、2.32ms。「決定論を主、LLM を補助」の
  有効性の査読証拠。
- 空白(自分で測る領域): 承認 judge 単体の精度を決定論ベースラインと
  比較したベンチは存在しない。「LLM judge+ハード境界」の end-to-end
  評価も未発表 — Horizon の運用計測に価値がある。

## 5. #38 への含意

- 収束案(judge に全文・grant 形状で hooks/config 除外)は field の
  ベストプラクティスと一致 — 独自発明ではない。
- Horizon の grant モデルは加算専用で除外機構が無い(keeper 検証済み、
  board #38)ため実装には新機構が要るが、その形は「リテラル除外」で
  なく「解決済み gitdir 基準の read-only 再適用」(Codex 型)にすべき。
- prefilter は廃止でなく縮退: 「全セグメント read-only なら judge 不要」
  の安全側拡大にだけ使う(Codex 型)のが合理的な残し方。

出典 URL は本文に併記(主要: code.claude.com/docs/en/sandboxing、
github.com/openai/codex codex-rs/、Pillar/Cymulate/Tracebit 各レポート、
dl.acm.org/10.1145/3658644.3690291、aclanthology.org/2025.llmsec-1.8、
arXiv 2607.21642 / 2606.15549)。
