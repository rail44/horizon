# サブエージェントの実測と先行実装調査(2026-11-20)

`task`(explore 役割)の作りを見直すための記録。Horizon の実ログでの実測、
先行実装のソース・公式文書の調査、ハーネス単位のベンチマークの状況、
オーナーが Claude Code で書ける子をどう使っていたかの集計をまとめる。
決定は [サブエージェントの設計](../agent-subagent-design.md) にある。

先行調査との関係: 2026-07-26 の
[文脈削減の先行実装調査](agent-context-reduction-prior-art-2026-07-26.md)
§3–4 が OpenCode(`743f6410`)と Hermes(`d9165d7a`)の委譲と上限到達時の
挙動を調べている。本記録はその後の変化と、未調査だった実装・観点を扱う。

根拠の種類: 〔ソース〕= clone したソースを直接読んだ、〔公式〕= 公式文書・
changelog・公式ブログ、〔観察〕= このセッションで Claude Code から見えた
ツール定義、〔第三者〕= リーク・逆解析・第三者記事。調査は Opus の
サブエージェント 5 本への委譲で行い、本記録はその報告の統合。

## 1. Horizon の実測(2026-11-13〜20、`agent-events.jsonl`)

- explore 子セッション 14 件のうち、正常終了は 0 件。13 件が上限 25
  リクエストに達して `HaltedByIterationCap`、1 件が 120 秒の stream timeout。
- 上限到達でも報告は返っている(`summarize_on_cap`)。11 件とも 8〜20k 字。
  ただし 6 件は「Partial report」「I ran out of turns」と自己申告。
- 上限到達時の子のコンテキストは中央値 53k・最大 121k トークンで、窓には
  余裕がある。上限はコンテキストではなく回数で止めていた。
- 依頼は広い: 1 件に問いが 4〜5 個、「exhaustive inventory」など。
  `task` の説明文にはすでに「Prefer several narrowly scoped tasks launched
  in parallel in one response over a single broad one.」がある。
- 設計(`agent-explore-design.md` 決定 7)が想定した「上限に当たったら依頼側が
  絞って出し直す」は 0 件。
- 子に bash が無く、コミット履歴を確かめられなかったと書いた報告がある
  (2e73fdcc)。
- 打ち切りは結果の `capped: true` にしか出ず、オーナーは気付いていなかった。

## 2. explore / サブエージェントの作り(版はいずれも 2026-11-19〜20)

### 予算

| 実装 | 上限 | 根拠 |
|---|---|---|
| Horizon(当時) | 25 リクエスト固定 | — |
| Gemini CLI `codebase_investigator` | 50 ターン + 10 分(汎用既定 30 ターン/10 分) | 〔ソース〕`codebase-investigator.ts:112-115`, `agents/types.ts:51,56` |
| Claude Code | `maxTurns` は任意、Explore の既定値は記載なし。子は自分のコンテキストを自動圧縮 | 〔公式〕 |
| OpenCode V2 | `steps` は任意、組み込みエージェントは未設定 = 無制限 | 〔ソース〕`session/runner/llm.ts:261` |
| Hermes | 250 回(7 月は 50)。時間上限は撤廃し無反応検知に | 〔ソース〕`delegate_tool.py:108`, `delegate_tool_config.py:27-30` |
| Codex・Crush・Cursor・Amp | 回数上限なし。Crush はループ検知とコンテキスト残量 | 〔ソース〕〔公式〕 |
| Windsurf Fast Context | 4 ターン × 8 並列(専用学習モデル、出力はファイルと行範囲) | 〔公式〕 |

Hermes の時間上限撤廃の理由(`delegate_tool_config.py:27-30`):
"No default wall-clock cap on children: legitimate heavy work (deep reviews,
research fan-outs, slow reasoning models) was being killed mid-task."

### 上限・停止時の扱いと、未完了の伝え方

- 強制要約で部分成果を残すのは共通(OpenCode は `toolChoice: "none"`、
  Hermes と Gemini はプロンプトのみでツール定義は残す)。
- Hermes〔ソース〕が最も明示的: 結果に `exit_reason: max_iterations` と
  `truncated: true`(コメント "A budget-exhausted child still returns a
  summary (status stays 'completed'), so the parent needs this explicit
  flag")。親には "[TRUNCATED — subagent hit its iteration cap; the summary
  below may be incomplete. Verify before relying on it, or re-dispatch the
  unfinished part.]"、ユーザーには "Subagent Task Incomplete"。
- Claude Code〔公式〕: "returns its output marked as partial, and Claude can
  resume it to continue"(v2.1.246)。
- Gemini〔ソース〕: 猶予ターンで `complete_task` できると `GOAL` に上書きされ、
  UI も成功表示になる(`local-executor.ts:783-786`)。
- OpenCode〔ソース〕: capped の状態は無く、モデルの文面だけが手掛かり。

### 続きを頼めるか

Claude Code(組み込み Explore は除く)、OpenCode(`sessionID`)、Amp、Codex、
Hermes(`action`)は同じ子に追加の依頼ができる。Crush の `agent` は一度きり
(`agent_dispatch` は可)。

### いつ使うかの指示

- 広く使わせる: Gemini("primary mechanism for initial discovery when the
  task involves complex refactoring, codebase exploration or system-wide
  analysis"、`prompts/snippets.ts:748`)、OpenCode のモデル別プロンプトの一部
  (`meta.txt:36-39`)、Amp finder〔第三者、2025-09〕。
- 条件付きで絞る: Claude Code〔観察〕("Reach for this when you have
  independent work to run in parallel … or when answering would mean reading
  across several files"、"Do the work yourself when it is a handful of tool
  calls or a lookup whose target you already know"、"When in doubt, don't
  spawn")、Hermes(USE FOR: "reasoning-heavy subtasks, work that would flood
  your context, or independent parallel workstreams")、Crush。
- 頼まれたときだけ: Codex("Do not spawn sub-agents unless the user or
  applicable AGENTS.md/skill instructions explicitly ask …",
  `multi_agents_spec.rs:738-739`)、OpenCode の GPT 向けプロンプト、
  Claude Code の Opus 5 向けの一行〔公式〕。
- 依頼側に深さを指定させる: Claude Code と OpenCode の explore
  ("quick" / "medium" / "very thorough")。

### explore と通常のサブエージェントの差

| 実装 | 差 |
|---|---|
| Claude Code〔公式・観察〕 | 権限(Explore は Write/Edit 禁止)、モデル、再開可否 |
| OpenCode〔ソース〕 | プロンプトとツール権限だけ |
| Codex〔ソース〕 | `explorer` の設定ファイルは 0 バイト。説明文だけ |
| Hermes〔ソース〕 | explore 役割なし。`delegate_task` 1 種 |
| Crush〔ソース〕 | `access: read / write` 引数 |
| Amp〔公式〕 | 通常の Task サブエージェント + 読み取り専用の skill に統合、finder 等はプラグインへ |
| Gemini CLI〔ソース〕 | 専用プロンプト・JSON 出力・Flash・50 ターン/10 分 |

子のシェル: Claude Code の Explore(`git log` 等の読み取りを許可)、OpenCode、
Crush、Hermes はあり。Gemini、Amp finder、Fast Context はなし。モデルは
Claude Code が Explore を Haiku から親の継承に戻し(v2.1.198)、Amp は finder を
Haiku 4.5 に(2025-10)と分かれる。

## 3. 書ける子の作業場所と承認

| 実装 | 既定の作業場所 | worktree | 子の承認 |
|---|---|---|---|
| Claude Code〔公式〕 | 親と同じ | 任意 `isolation: worktree`(既定ブランチから) | 親の画面に「どの子か」付きで出す、親の許可モードを継ぐ |
| Codex〔ソース〕 | 親と同じ("All agents share the same directory") | なし | 親の方針を継ぐ、TUI に "Thread: <子>" 付きで出す |
| OpenCode〔ソース〕 | 親と同じ | なし | 子の ask を親の画面に統合 |
| Gemini CLI〔ソース〕 | 親と同じ | なし | 親の確認キューへ再発行(待ち時間は時間上限から除外) |
| Hermes〔ソース〕 | 親と同じ | 任意(親の HEAD から、"the parent agent will review and merge your branch") | CLI では既定で拒否 |
| Crush〔ソース〕 | 親と同じ | なし | 親の自動承認を継ぐ、それ以外は親の画面で順に |
| Cursor〔公式〕 | 親と同じ | 任意("Changes stay on each subagent's branch until the parent agent merges") | 親の方針を継ぐ |

ぶつかり合いへの対策はほぼプロンプト(Codex の "disjoint write set"、
"not alone in the codebase"、Crush の "clobber each other")。仕組みは Hermes
のファイル単位ロックと兄弟の書き換え警告、Crush の読んでから書く + 更新時刻
照合程度。

書き込み範囲: サブエージェント起動ツールの引数に書き込み範囲を取るものは
無かった(Claude Code・Codex・OpenCode・Crush・Hermes・Gemini・Amp を引数定義で
確認、Cursor は未確認)。Gemini の `workspaceDirectories` は範囲を足すもので
絞るものではない(`local-executor.ts:555-558`)。理由を述べた記述は見つから
なかった。

## 4. ハーネス単位のベンチマーク

- Terminal-Bench 4.1(tbench.ai 既定、2026-11-17 更新)は各社が自社ハーネスで
  自社モデルを走らせた表で、ハーネスの比較にならない。上位は Copilot CLI、
  Claude Code、Antigravity CLI、Codex。
- モデル固定の比較では成功率の差は小さく一貫しない。差はコストに出る:
  Arena「HarnessTax」(2026-09、±2〜5%、Claude Code は Pi の約 1.5〜2 倍の
  コスト)、arXiv 2610.04433(2026-10/11、"No harness leads consistently
  across models")、arXiv 2609.11987(2026-09)、FrontierHarness(2026-08 は
  Kimi K3 固定で Codex 首位、11 月 v1.1 は Pi・Codex・ZCode 同率首位で内訳
  未確認)。
- Terminal-Bench 2.0 の公開投稿(2026-08 スナップショット)は同一モデルで
  11〜18 ポイントの差があるが、大半は未検証の自己申告。
- 固定モデル比較で安定して上位なのは Codex と Pi(Pi はサブエージェントを
  持たない)。

## 5. オーナーの Claude Code での使い方(`~/.claude/projects`、2026-10〜11)

子の meta(`agentType`, `spawnedWithWorktree`)と transcript の Edit/Write から。

| 子 | worktree | 体数 | 編集 | 自分の根の外への編集 |
|---|---|---|---|---|
| worker | 全件 | 49 | 1250 | `/tmp` の作業スクリプト 33 のみ。主チェックアウト・兄弟 worktree は 0 |
| general-purpose | 引数で指定 | 4 | 64 | `/tmp` 11 のみ |
| general-purpose | なし | 58 | 44 | 作業ツリーのコードを編集したのは 1 体(親の worktree の 13 ファイル)。他は `~/.claude` 18、`/tmp` 1 |
| Explore | なし | 13 | 0 | — |
| fork | なし | 2 | 0 | — |

worker が全件 worktree だったのはリポジトリの定義(`.claude/agents/worker.md`、
`isolation: worktree`、b6442fc1 で導入、2026-11-03 に廃止)による。同じ作業
ツリーで書く子が同時に動いた例は 0。書かせる作業はほぼ全て worktree で
分けられていた。
