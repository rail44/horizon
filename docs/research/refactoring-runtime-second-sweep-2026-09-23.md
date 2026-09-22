# 実行基盤のリファクタリング第二巡

基準: `02fea626`。board関連は対象外。下記3領域の実装・回帰検証・main反映・
再解析を完了条件とし、権限・承認・停止・復元の仕様を維持する。

| 対象 | 整理する責務 | 状態 |
| --- | --- | --- |
| sandbox helper | 実行環境の取得、探索の優先順位、Cargo成果物の選別 | main反映済み (`689e2ae8`) |
| bash実行 | 起動準備、待機と出力回収、結果判定と共通情報の付与 | main反映済み (`34ec49fb`) |
| agent実行環境 | セッション情報、環境構築、環境切り替え | 実装済み |

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
