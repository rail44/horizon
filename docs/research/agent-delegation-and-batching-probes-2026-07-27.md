# 並列 tool call と委譲採用の統制プローブ（2026-07-27）

`agent-ceiling-death-autopsy-2026-07-26.md` が残した疑問 —「なぜ実セッション
は ~1 call/往復で、explore を使わないのか」— を、Horizon を介さない直接
API プローブで切り分けた記録。オーナーの仮説「rig の用い方・API 呼び出し・
ツールスキーマ・プロンプトの問題では」を起点に、セルを追加しながら
231 リクエストを実行した。実行系・生データ・fixture は job tmp（揮発）に
あり、本 doc が恒久記録。プローブは Opus worker への委譲で実施。

- endpoint: `https://api.synthetic.new/openai/v1/chat/completions`
- モデル: `hf:moonshotai/Kimi-K2.7-Code` / `hf:MiniMaxAI/MiniMax-M3`
- 各セル n=3〜5（**率の推定ではなく仮説の白黒用**。残差 1/5 級の差は
  未解決として明記）
- 測定は原則「1 応答の最初の tool call(s)」。task 報告返却後の挙動は
  未測定（唯一の follow-through 測定は C6）

## 1. 白になったもの（並列 emit の機構系）

- **serving 層**: 両モデル・stream/非 stream とも並列 call を正常 emit
  （2 call を 20/20、4 call も成功）。stream の delta 形式も健全
  （index 0 起点・昇順・id 一意・finish_reason "tool_calls"）
- **Horizon の request 形**: 実物スキーマ 5 個 + 実 system prompt（B1）、
  フルカタログ 16 個 + 実 AGENTS.md 16k（E1/E4）でも並列 emit は
  20/20・12/12 — ドット付きツール名・スキーマ形式・preamble は
  少なくとも短文脈の並列 emit を阻害しない
- `parallel_tool_calls` フラグはこの serving 層では**両方向とも無視**
  （false でも 6/6 バッチ）。Horizon が送る true は無害な no-op
- 非 stream 時のみ Kimi の tool_call index が会話通算になる quirk
  あり（16/16）。stream では 0 起点で、Horizon の経路には乗らない

## 2. ベースラインの支配要因はタスクの形

- 「独立した複数ターゲットが明示された」応答だけがバッチする。
  ファイル名なしの現実的タスク（G1）は 6/6 で単発 call
- **grep が複数ロケーションを返した直後**（H1、短い履歴）は両モデル
  とも見事にバッチ: Kimi 5/5 で 4 call、M3 4/5。各 call は報告行の
  周囲に offset/limit を絞った理想形。grep 結果への案内文（H2）は
  効果を検出できず（Kimi は無しで飽和）

## 3. 実セッションが逐次に沈む機構 = 履歴の前例の自己模倣

偽の履歴（~14k tokens）を作り、同一の grep 直後状態（H1 tail）を
置いて比較した。

| セル | 履歴 | Kimi | MiniMax(M1m/M2m, path 修正後) |
|---|---|---|---|
| H1 | ほぼ無し | 5/5 バッチ | 4/5 |
| M1/M1m | 単発 call ×12 ターン | **2/5** | **0/5** |
| MI | M1 + 命令調並列指示（MUST…very important） | **0/5** | — |
| M2/M2m | M1 と同長、序盤にバッチ 2 ターン植込み | **5/5** | **5/5** |

- **同じ長さの履歴で、バッチの前例が有るか無いかだけで完全反転**
  （M3 は 0/5 ↔ 5/5）。長文脈劣化説は M2 で棄却
- **命令文は前例に勝てない**（MI 0/5 — 回復ゼロ）
- M3 は「Four call sites; reading each in parallel.」と**宣言した直後に
  単発 read を 1 個だけ emit** した記録あり。発話・意図より履歴パターン
  が優先される
- 当初の M1/M2（M3 分）は fixture の欠陥で無効だった: 履歴は本物らしい
  path なのに tail の grep 結果だけ `/repo/crates/a.rs` 級の雑な path
  だったため、M3 が実在を疑って read 自体を拒否。path を履歴と整合させ
  て解消（測定装置の穴は結果より先に疑うこと）

**含意**: 長セッションの逐次化は序盤に積もる「1 call ずつ」の前例の
自己模倣であり、文言では直らない。前例そのものを変える（委譲 first 化
で序盤の軌跡を作り替える／転写形式の少数例。後者は未検証）しかない。

## 4. 委譲（task/explore）採用のレバー

探索ゴール（C 系）と実装ゴール（C7 系）の両入口で、フルカタログ
16 ツール下の「最初の一手」を測定。

| セル | 変更点 | Kimi | MiniMax |
|---|---|---|---|
| C1 | 3 ツールのみ + 現行文言 | explore 5/5 | 4/5 |
| E3 | 16 ツール（AGENTS.md 無し） | 2/3 | **1/3** |
| E2 | 16 ツール + AGENTS.md | 5/5 | 2/5 |
| E2i | + 命令調（MUST/FIRST/エラー宣言） | 3/3 | **3/3** |
| C3 | `task` 名 + 一般的記述に差し替え | **4/5** | **0/5** |
| C4 | + bash 記述に否定 routing（M3 訓練方言） | 1/3 | **0/5（効果ゼロ）** |
| C5 | + **前置き禁止条項**（探索入口） | **3/3** | **5/5** |
| C7 | 実装入口・条件付き（"in an unfamiliar area"） | 5/5 | **2/5** |
| C7b | 実装入口・**無条件 + 既知に見えても** | 3/3 | **4/5**（task 同ターン 5/5） |

- **採用劣化の犯人はカタログサイズ**（bash 等との競合）。AGENTS.md は
  無罪（E3 ≒ E2）
- M3 は複数セルで「委譲すべき」と reasoning/content で言明した直後に
  bash を呼ぶ（**意図と行動の乖離**）。C6 の follow-through 測定では
  bash → bash と orientation を連鎖し、遅延採用ですらない
- **効いたのは唯一「前置き手順の明示的封鎖」**: 「task を FIRST action
  にせよ。orient のための bash/ls/read を先に走らせるな — task 側が
  自前で orientation する」。探索入口（C5）で両モデル満点、実装入口は
  条件句 "in an unfamiliar area" が逃げ道になり（C7）、無条件化 +
  「対象が既知に見えても」で回復（C7b。残る 1/5 も bash+task を同一
  応答で並記しており、委譲自体は同ターンに成立）
- `task` への改名は Kimi のみ有効（C3: 4/5）、M3 には中立。命令調は
  両モデル有効（E2i）。**否定 routing（C4）は無効**
- 当選文言（実装に verbatim 移植すべきもの）:
  - 探索入口（C5）: "For an open-ended exploration goal, calling task
    must be your FIRST action — do not run bash/ls or read files to
    orient yourself first; the task agent does its own orientation
    inside its own session, which costs you nothing."
  - 実装入口（C7b）: "For any implementation task, your FIRST action
    must be to delegate the up-front investigation and planning to the
    task tool — even when the change targets look already known or the
    task statement names concrete components. … Do not grep/read/bash
    to orient yourself first; the task agent does its own orientation
    in its own session. Start implementing only after its report
    returns."

## 5. 未解決（次の観測ポイント）

- C7b の 2 変更（条件句削除／既知条項追加）のどちらが効いたかは未分離
- M3 の残差 1/5（orientation 衝動の同ターン並記）が抑制可能かは n=5
  では判定不能
- task 報告が**返った後**の挙動（報告を信じて実装に進むか、再探索する
  か）は全セル未測定 — in vivo の dogfood で観測する
- M3 の意図・行動乖離の機構そのものは未説明
- system prompt 内の転写形式 few-shot がバッチ前例として機能するかは
  未検証（momentum の文言的対策の最後の候補）

## 6. 関連する既測定（別 doc）

- read/grep/read 系の消費実測: `agent-ceiling-death-autopsy-2026-07-26.md`
- vendor 方言 diff（ドット名の Moonshot 正規表現違反、MFJS、テキスト型
  tool 結果、相対 path、命令調 native prompt、両 vendor の並列 call
  native サポート）: 同日の vendor-dialect 調査（統合前。本 doc と同じ
  結論群を支える背景資料）
