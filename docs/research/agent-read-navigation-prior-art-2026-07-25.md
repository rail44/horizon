# Agent read/navigation の実測と先行実装調査（2026-07-25）

`agent-tool-output-and-read-routing-2026-07-24.md` の続き。あちらは「read の
既定と上限をどう決めるか」を調べた。こちらは **prune 撤回後（`162967f`）に
残る消費が何によるものかを実測し**、その支配項に対して先行実装が何を
決定論的に、何をプロンプトで扱っているかを調べた記録。

参照した revision / 情報源:

- OpenCode: `743f6410f2e5002723fc5e893039ac49fbfe0de8`（実装を直接参照）
- Hermes Agent: `d9165d7a678d4105f42921a7fc1886df3804531b`（実装を直接参照）
- Crush: <https://deepwiki.com/charmbracelet/crush/6.2-file-system-tools>
- Aider repo map: <https://aider.chat/docs/repomap.html>

## 1. 実測 — 支配項は fs.read

オーナーの event log（19 日分）から。

**再送重み**（tool 結果の chars × その後に残るリクエスト数）の内訳:

| session | reqs | fs.read | bash | fs.grep | fs.edit |
|---|---:|---:|---:|---:|---:|
| 9087e6ce（小さな実装タスク） | 7 | 61% | 0% | 38% | 0% |
| 4479eadd | 54 | 92% | 2% | 0% | — |
| 90bccadd | 217 | 55% | 22% | 20% | 0% |
| 018f6a74 | 199 | 86% | 6% | 4% | 1% |
| f5f5c4d6 | 469 | 55% | 30% | 10% | 1% |

`fs.edit` は 0〜1%。改変そのものは安く、その前後の読みが支配的。コードを
改変したセッションは**書いたバイト数の 5.4〜59.5 倍を読んでいる**（中央値
約 8 倍）。

bash は主因ではない。全 676 件の bash 結果のうち 388 件が 1k chars 未満、
平均 2,072。`cargo` 出力が効いているという仮説は棄却された。

### fs.read の内訳（950 件）

| 経路 | 件数 | 中央値 | 平均 | p90 | 全 read バイト比 |
|---|---:|---:|---:|---:|---:|
| grep 済 → 窓あり | 468 | 4,568 | 5,812 | 11,910 | 30% |
| grep 済 → 窓なし | 156 | 11,649 | 15,619 | 33,545 | 27% |
| cold → 窓あり | 92 | 8,698 | 9,252 | 17,393 | 9% |
| cold → 窓なし | 201 | 7,140 | 14,631 | 39,551 | 32% |

- **窓なしの 2 経路は件数で 38%、バイトで 59%。**
- 全 read の 50%（461/917）が**ファイル全体**を返している。うち 355 件は
  `offset`/`limit` を一切渡していない。
- **読まれたファイルの 65% が 500 行未満**。つまり既定 500 行は大半の
  ケースで拘束していない。「引数なし」が事実上「全文」になっている。
- 結果サイズ: median 5,316 / mean 9,426 / p90 20,889 chars。上位 10%
  （20k 超）が全 read バイトの 41%。

### 読み直しではない

同一 path・同一 `content_version` の行範囲を union して比較すると、重複は
**1.27 倍（重複バイトは全体の 21%）**。残り 79% は別内容。overlap dedup を
入れても 2 割しか取れない。**広く 1 回ずつ読んでいる**のが実態。

### preamble

正確に測ると 28,797 chars ≒ **7,000 tokens/リクエスト**。

| | chars | 割合 |
|---|---:|---:|
| AGENTS.md（repository instructions） | 16,894 | 59% |
| tool schema 17 個 | 10,369 | 36% |
| system prompt 本体 | 1,534 | 5% |

tool schema には `mock.approval_required` / `mock.boundary_crossing` という
**テスト用 fixture 2 個が production catalog に載っている**（`rig_tool_
definitions(None)` は catalog 全件を advertise する）。加えて 19 日間の
使用回数は `recall.search` 1 / `recall.read` 0 / `config.write` 0、
`fs.patch` は導入（2026-07-22）以降 1,139 tool call 中 **0 回**。

## 2. 決定論的な read 上限 — Horizon は既に同等以上

| | read 既定 | 最大行 | 行あたり | 出力総量 |
|---|---|---|---|---|
| OpenCode | 2,000 行 | — | 2,000 chars | 50 KB |
| Hermes | 500 行 | 2,000 | 2,000 chars | 50,000 |
| Crush | 200 行 | — | 2,000 chars | 200 KB |
| **Horizon** | **500 行** | **2,000** | **2,000 chars** | **50,000 chars** |

Hermes とは数値が完全一致、OpenCode より既定は厳しい。**「Horizon の上限が
緩い」という調査中の仮説は、先行実装に対しては成立しない。** Crush の
200 行だけが Horizon より厳しく、そこが前例のある唯一の締め代。

## 3. routing はどこも例外なくプロンプト

OpenCode `src/tool/read.txt` の実物（抜粋）:

```
- Use the grep tool to find specific content in large files or files with long lines.
- If you are unsure of the correct file path, use the glob tool to look up filenames
- Call this tool in parallel when you know there are multiple files you want to read.
- Avoid tiny repeated slices (30 line chunks). If you need more context, read a larger window.
```

Horizon の `fs.read` description は実質同じことを言っている。**「grep して
から窓つきで read」を決定論的に強制している実装は、調べた範囲に一つも
ない。** どの実装もここをモデル側の判断として受け入れている。

Horizon の現行文面には一点、方向が逆の箇所がある:

> Pass offset/limit to **continue through a file**

窓指定を「続きを読む」＝ページングとして提示しており、「狙って読む」とは
言っていない。ただしこれはプロンプトの問題なので、直しても効果は非決定論的。

## 4. 決定論的で、Horizon に無いもの

先行実装が決定論的にやっているのは上限ではなく、**バイトを会話に載せずに
済ませる別経路**だった。

**(a) シンボル単位の到達**

- OpenCode: `lsp` ツール（`documentSymbol` / `workspaceSymbol` /
  `goToDefinition` / `findReferences` / callHierarchy）。「X はどこで定義され
  誰が呼ぶか」を本文なしで答えられる。
- Crush: view/edit の結果に LSP diagnostics を注入。
- **Aider: repo map。** リポジトリ全体の重要なクラス・関数をシグネチャ付きで
  ランク付けした地図を、**毎リクエスト自動で**添付する。参照グラフ上の
  graph ranking で選び、`--map-tokens` で予算を切る（既定 **1k tokens**）。
  モデルが要求するのではなく、ハーネスが決定論的に構築する。

これは実測の **cold → 窓なし read（201 件・read バイトの 32%）** の動機、
すなわち「このファイルに何があるか分からないから全部読む」に直接当たる。

**(b) 探索を本会話から隔離する**

- OpenCode `grep.txt` 末尾: 複数ラウンドの glob/grep が要る open-ended な
  探索は **Task ツール（サブエージェント）へ回せ**。中間バイトは親の
  context に入らない。
- Hermes `tools/code_execution_tool.py`（Programmatic Tool Calling）:
  モデルが Python スクリプトを書き、RPC 経由で Hermes のツールを呼ぶ。
  多段のツール連鎖が **1 推論ターンに畳まれ**、スクリプトの結果だけが
  context に入る。

**(c) ツールスキーマの遅延開示**

- Hermes `tools/tool_search.py`: core 以外のツール定義を 3 つの bridge
  ツール（`tool_search` / `tool_describe` / `tool_call`）に置き換え、
  要求時に開示。**deferrable なツールがモデルの context window の 10% を
  超えるときだけ**発動する決定論的ゲート付き。core ツールは決して defer
  しない。

Horizon の preamble（7,000 tokens、うち 36% が tool schema）に直接効く。

## 5. 選択肢と性格

| | 決定論的か | 実測支配項に当たるか | 実装コスト |
|---|---|---|---|
| read 既定を Crush 並み（200 行）に締める | ○ | 窓なし read のみ、部分的 | 極小 |
| description を「狙って読む」に書き換え | ✗ | 間接的 | 極小 |
| シンボル/アウトライン経路 | ○ | cold 全文読み（32%）に直接 | 中〜大 |
| 探索の隔離（サブエージェント / PTC） | ○ | 中間バイト全般 | 大 |
| tool schema の遅延開示 | ○ | preamble（7,000 tok/req） | 小 |
| `mock.*` を production catalog から外す | ○ | 微小（237 chars） | 極小・単なる不具合 |

シンボル/アウトライン経路には二つの性格がある。Aider 型（毎回自動添付・
予算固定）は preamble を増やす方向なので、Horizon の 7,000 tokens に何を
足すかの設計が要る。LSP 型（モデルが問い合わせる）は往復を増やす方向。
どちらを取るかは未決。

## 6. 測定のベースライン

以後の変更は同一プロンプトで n≥3 取って比較する。`162967f` 時点の値:

**調査タスク**（read-only、対象 6 ファイル・85KB）

| | input | cached | uncached | reqs |
|---|---:|---:|---:|---:|
| cdeb4a0f | 113,459 | 63% | 41,011 | 4 |
| cb1a9b17 | 190,808 | 83% | 30,616 | 7 |
| ad41374a | 242,142 | 85% | 34,718 | 9 |
| 5930e264 | 259,337 | 82% | 44,425 | 9 |

**実装タスク**（catalog.rs の説明文 2 箇所に追記 + `cargo check`）

| | input | cached | uncached | reqs | calls |
|---|---:|---:|---:|---:|---:|
| 9087e6ce | 179,230 | 85% | 27,550 | 7 | 11 |

参考: prune 有効時（`87dc479` 以前）の同一調査タスクは input 98,804 /
123,455 / 141,306、uncached 48,436 / 56,826 / 69,183。撤回により **raw は
約 1.75 倍に増え、uncached は約 33% 減った**（cached 率 43〜59% →
63〜85%）。往復回数と再読の頻度は変わっていない。

注意: 同一ビルド・同一プロンプトでも `c35ffe15`（47 リクエスト /
944,023 input / 同一ファイル 14 回読み）のような外れ値が 5 本に 1 本出た。
1 本の観測から傾向を語らないこと。
