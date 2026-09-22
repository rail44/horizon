# 実行基盤のリファクタリング第二巡

基準: `02fea626`。board関連は対象外。3領域の実装・回帰検証・main反映・
再ビルド・再解析を完了。権限・承認・停止・復元の仕様は維持した。

| 対象 | 整理する責務 | 状態 |
| --- | --- | --- |
| sandbox helper | 実行環境の取得、探索の優先順位、Cargo成果物の選別 | main反映済み (`689e2ae8`) |
| bash実行 | 起動準備、待機と出力回収、結果判定と共通情報の付与 | main反映済み (`34ec49fb`) |
| agent実行環境 | セッション情報、環境構築、環境切り替え | main反映済み (`4fe9f4fd`) |

helperの探索順は、明示指定→隣接ファイル→Cargoのprofile直下→workspaceの
uplift先→protocol markerを持つ最新のhashed成果物→PATH。存在しない候補は飛ばす。
リポジトリ既定の共有build-dirは廃止済みだが、外部設定による分離配置まで不要とは
確認できないためuplift探索を維持。分離配置・custom profileをファイル配置テストで確認し、
現行構成は通常ビルドと実helperを使う全体テストで検証する。

bashは`exec/plain.rs`に通常実行、`exec/sandboxed/`に起動準備・出力回収・
結果判定を分離。sandbox情報・拒否の診断・出力回収打ち切りの情報を共通化した。
filesystem→macOS mach service→domainの再試行優先順位、タイムアウト時の扱い、
子孫プロセスの停止と回収中のキャンセル登録を維持。拒否と終了状態の組合せを
3テスト追加し、bash関連のsandboxed profile対象71テストが通過。

agentは`session/environment.rs`に環境構築と切り替えを集約。セッション識別情報と
起動時設定は`SessionEnvironment`、配置・信頼状態は`EnvironmentLocation`、
構築結果は実行環境と保存用contextの組として渡す。イベント処理の順序は維持する。
実Git・イベントログを使うテストで、保存後の応答、保存不能時の環境維持とworktree回収、
二重切り替えの拒否を確認。既存の追加grant保持テストに重複除去も加え、関連33テストが通過。

再解析の比較（上記3領域、bashはディレクトリ全体を含む）:

| 観測点 | 変更前 | 変更後 |
| --- | --- | --- |
| helper探索のcognitive complexity | `resolve`: 49 | `resolve`: 0、分離先の最大: 3 |
| sandbox bashのcognitive complexity | `run_sandboxed`: 51 | `run_sandboxed`: 5、分離先の最大: 13 |
| agent環境の引数 | 構築10・切り替え10 | セッション情報を共有し、構築2・切り替え5（`self`除外） |

agent環境の構築・切り替えのcomplexityは12・15のまま。ここでの改善は責務と
受け渡す情報の整理であり、分岐削減ではない。全範囲のexact一致は2→0組、
正規化一致は7→6組。異なる拒否種別や探索段階の類似形は維持し、Cargo/Gitの既存一致は今回変更しない。
指標はclosureを含み、macroは展開しない。数値だけでは責務の適切さを判定できない。

固定版Tree-sitterが`#[cfg]`付きパターンフィールドを読めなかったため、
macOS専用フィールドを先に取り出す等価な構文へ変更。解析器の失敗を無視せず、
15ファイル・106関数を解析完了。統合後も同じソースで再確認した。

検証: 全体nextest 2,145件成功（既存の14件skip）、workspace build、fmt、Clippy、
wire schema、preview WASMチェックが成功。実行環境はLinuxで、macOS実機検証は未実施。
helperの分離build-dirは配置テストで検証し、実際の別Cargo構成でのビルドまでは行っていない。
