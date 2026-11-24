# エージェントのコアと拡張の境界、会話とセッション

Status: 設計(未実装)。調査は
`docs/research/agent-frameworks-core-and-extension-2026-11-23.md`(pi・Mastra・
eve・Flue の部品ごとの実装と、Horizon との対照)。

## 目的と範囲

組み込みエージェントのコードを、二つに分けた形に整える。

- **コア**: 多くのエージェントのフレームワークに共通する部分。ループ、プロバイダとモデル、ツールの実行、会話の記録、compaction、中断と入力の待ち行列がこれに当たる。
- **カスタマイズのモジュール**: 挙動を変える部分。承認の方針と judge、継続の判断、role と skill、knowledge、board と wiki のツールがこれに当たる。

さしあたりは同じ crate の中のモジュールとして分ければよい。ただし、境界は後で別プロセスや wasm へ切り出せる形にしておく。あわせて、コアの部品には、調査で確かめたフレームワーク間の共通のやり方を取り入れる。

会話の分岐は、エージェントセッションという抽象を「会話」と「セッション」に分けることで実現する。用途は二つある。過去のメッセージを書き直して再実行することと、ある時点から別の試みを並べて走らせることである。

MCP は範囲外とする。ツールの口(下の「ツール」)が、後から足せる形になっていればよい。

## 境界の条件

コアとモジュールの間を通るものは、次の条件を満たす。

- **データだけを渡す。** 値は所有権を持ち、serde で送れるものに限る。`Rc`・クロージャ・`&'static`・`SessionLoopState` への参照は渡さない。
- **判定は保留できる。** 可否の判定は非同期に返る(人間の承認を待てる)。
- **組み立てはセッションごとに行う。** モジュールの一覧は、セッションを作るときに組み立てて渡す。今の `roles::register_external`・`skills::register_external_skill_sources`・`tools::register_external_tools` のようなグローバル static への登録はやめる。

## 段

コアは、ループの決まった位置でモジュールを呼ぶ。段の型は Rust の trait で表し、モジュールは使う段だけを実装する。段は、今あるカスタマイズが実際に使う位置から決めた。

| 段 | いつ | モジュールが返すもの | 今の場所 |
|---|---|---|---|
| 解決 | モデル呼び出しの前に毎回 | system prompt の名前付きの節、提供するツールの定義 | `session_prompt::session_extra_sections`、`completion::rig_tool_definitions` の絞り込み |
| context | プロバイダに渡す直前 | 送るメッセージ列の置き換え(元の記録は変えない) | `clearing::history_for_provider_request` |
| 可否 | tool call ごと | 許可 / 拒否(理由つき)/ 人間に回す。人間に回す場合は保留する | `policy::plan_prepared_call`、`judge::start_approval_gate` |
| 継続 | ラウンドがツール呼び出しなしで終わったとき | 終える / 続ける(追記するメッセージつき) | `session/continuation.rs`、`turn.rs` の truncation 回復 |
| 観測 | イベントごと | なし(副作用のみ) | `persistence`、ACP への写し |

「解決」の段では、モジュールは節とツールを宣言するだけで、system prompt やツール一覧を直接書き換えることはしない。コアは前回のモデル呼び出しとの差分を取る。最初に送った system prompt とツール一覧はそのまま送り続け、変わった分は会話の後ろに追記するメッセージとして伝える。こうすると送る先頭部分が毎回同じになり、プロバイダのプロンプトキャッシュが効く(調査の §2「プロンプトキャッシュと先頭部分の固定」。pi・eve・Flue が同じ形をとる)。Anthropic の wire では、system・最後のツール・最後のメッセージに `cache_control` を置く。

「可否」の段は保留できる形にする。承認の tier(`policy.rs`)と judge は、この段に付くモジュールになる。

## ツール

閉じた match(`tools::synchronous::SynchronousTool`、`execution::execute_synchronous` の分岐の連鎖)を、一つの trait に置き換える。ツールは次の四つを持つ。

- 定義(id・説明・入力の schema)
- 承認の分類(Contained / BoundaryCrossing / AlwaysAsk)
- 実行。入力・中断のシグナル・途中経過の送り先を受け取り、出力を返す。
- 出力の上限と、上限を超えたときの扱い

board のツールが `BoardHost` という別の口で入っている形もこの trait にそろえ、外部のツールと同じ一覧で扱う。MCP のツールも、後から同じ一覧に加えられる。

## 会話とセッション

### 会話

会話は entry の木である。

- **entry の識別**: 各 entry は `id` と `parent_id` を持つ。
- **entry の種類**: 入力、応答、ツールの告知と結果、clearing、モデルと reasoning effort の変更を entry として持つ。モデルと effort は今は保存されず、起動のたびに role から決め直している。
- **組み立て**: 一つの葉から根へたどった列が、その枝の会話である。プロバイダに送る履歴は、この列から組み立てる。
- **保存**: 記録は今と同じ JSONL に追記する。entry が親を指すので、event log の版は上がる(v5)。親が無い record(judge の verdict)と、親が読めなかった record(読み飛ばした行を指すもの)は、同じセッションの sequence で一つ前の record に続くものとして読む。DuckDB の projection も親を持ち(projection の schema 3)、incremental resume は projection から同じ枝をたどる。

### セッション

セッションは、会話の一つの葉に付いて動く実行である。持つものは worktree、ツールの状態、入力の待ち行列、承認待ち、モデルの選択。セッション ID と worktree の名前の結びつきは今のまま保つ。

### 二つの操作

- **葉を移す(書き直して再実行)**: 同じセッションが付いている葉を、過去の節から生やした新しい枝へ移す。元の枝は木に残る。
- **新しいセッションを付ける(並べて試す)**: 指定した節に新しいセッションを付ける。元のセッションはそのまま動く。セッション同士の系譜(`docs/session-relationship-design.md`)には、どの節から生まれたかを加える。

ACP では、v2 の session cursor(entry を指す位置)と `session/fork` が、この二つに当たる。どちらも今使っている crate(`agent-client-protocol-schema` 1.19.0)では unstable feature で、有効にしていない。

### 分岐できる位置

分岐できるのは、決着した位置だけとする。user メッセージの直前か、ターンの終わりである。

途中の tool call や承認待ちを写さないので、写した履歴の末尾が結果のない tool call で終わることがない。以前の探索の子への履歴の写し(`docs/agent-explore-design.md` の Fork seeding)は、この形の末尾が chat template を壊して撤去された。

### ファイル

決着した位置ごとに、worktree の状態を git に記録する。

- **記録の作り方**: 一時的な index に `add -A` し、`write-tree` と `commit-tree` で commit を作る。commit は Horizon 専用の ref(`refs/horizon/checkpoints/<会話>/<entry>`)に置く。利用者のブランチ・index・作業中のファイルには触れない。
- **葉を移すとき**: 「会話だけ戻す」か「ファイルも戻す」かを選べる。ファイルも戻す場合は、移り先の節の tree を worktree に展開する。
- **新しいセッションを付けるとき**: その節の commit から worktree を作る。

戻らないものもある。worktree の外への副作用と、`.gitignore` に入っている物(`target/` など)である。また、git の管理下にないディレクトリでは、ファイルは分岐しない。

## コアの部品に取り入れるもの

調査の対照表(11247752 時点)と照らして、コアの部品に入れるものは次のとおり。

- **プロンプトキャッシュ**: 上の「解決」の段の形と、Anthropic の `cache_control`。
- **ツール**: 中断のシグナルを全ツールに渡す。途中経過を UI に流す。tool call の入力の部分 JSON を UI に流す(今は rig のプロバイダが捨てている)。
- **出力の上限で止まった場合**: その応答の tool call は実行せず、再発行を促すエラー結果を返す。
- **モデルの情報**: catalog から max output と料金を読み、`max_tokens` を固定値(32768)からモデルの値にする。
- **usage と料金**: ACP に載せて UI に出す。今は記録するだけで、ACP への写しで捨てている。
- **edit**: 各編集を元のファイルに当て、編集どうしの重なりを禁止する。完全一致で失敗したら、正規化して当て直す(pi と eve の形)。
- **read**: 画像を読めるようにする。
- **grep**: 位置だけでなく、行の内容も返す。
- **skill**: Agent Skills 仕様の frontmatter を YAML として読む。リソースのファイルを読めるようにする。`.agents/skills` も探す。

compaction の要約(Tier 2)は、`docs/agent-compaction-design.md` が保留としているので、ここには入れない。会話の木に compaction の entry を置ける形にしておく。

## 進める順序

1. ツールの trait を入れ、今のツールをその上へ移す。board のツールも同じ一覧に入れる。
2. 段の trait を入れ、承認・judge・継続・clearing・prompt の節をその上のモジュールにする。グローバル static への登録を、セッションごとの組み立てに替える。
3. 先頭部分の固定と `cache_control` を入れる。
4. 会話の木とセッションを分ける(event log v5、ACP の cursor)。
5. ファイルの記録と、二つの分岐の操作を入れる。
6. 上の「コアの部品に取り入れるもの」の残りを入れる。

## 未決

- 分岐を操作する UI(エージェントの view のどこで、どの節を選ぶか)。
- ファイルの記録の ref をいつ消すか。
- ACP の unstable feature(`unstable_session_fork`・`unstable_session_cursors`)を有効にするか、`_horizon/*` の拡張で持つか。
- agentd の `frame` が streaming の delta を item として持ち続けるか。確定したメッセージを tool call の前に置く並び順をこの item が決めているが、agentd 側にその順序に頼る読み手がいるかは確かめていない。
