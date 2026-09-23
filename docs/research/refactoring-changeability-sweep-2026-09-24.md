# 変更容易性のリファクタリング第七巡

基準 `40848c94` から、board固有実装を除く320 productionファイルを37責務グループで確認した。
主要経路・呼び出し側・同種実装を確認し、変更後の追加3ファイルを含む323ファイルまで判断を記録した。
[対象と判断](../../scripts/refactor-audit/coverage-changeability.json)の未確認グループは0。

| 領域 | 主な変更 | 維持した境界 |
| --- | --- | --- |
| ツール・role・skill | knowledgeの文書形式を分離。bashのFIFO状態、終了結果、Git/Cargoのコマンド走査を共通化 | 承認・実行権限、role/skill選択、子taskとMoAの中断方針 |
| provider・モデル設定 | provider初期値・登録処理・履歴fallbackを共有。retryを分離し、要約時のツール無効化を修正 | セッション開始時の設定、429/5xxの異なる再試行規則 |
| イベント・履歴・永続化 | 入力の永続化・配送を分離。ProviderEvent初期値、現在ターンの承認照会、grep/globの表示規則を共有 | flush後の配送、重複排除、JSONL正本とDuckDB派生、occurrence単位の履歴 |
| コマンド・workspace・画面 | channel橋渡し、引数検証、分割処理、modal終了時のfocus、agent表示の投影を共有。テーマ補正・保存を整理 | close/detach/terminate、確認とキャンセル、画面ごとの状態所有 |
| runtime・端末・preview | host tool受信に既存pumpを使用。PTY副作用配送、CSI-uのmodifier/suffix、live/history背景描画を共有 | daemon別の寿命、PTY入力優先・同期更新期限、IME、描画cache、previewの再読込時キャンセル |
| sandbox・ファイル・通信 | 保持中と適用時のfilesystem grant再検証を共有 | 拒否条件とエラーの違い、再検証の時点、syscall別の処理、proxyとWeb取得の権限境界 |

発見した不具合は回帰テストで修正前の失敗も確認した。
要約専用のprovider要求にツールが広告されていた問題を直し、セッション設定を変えずに無効化する。
テーマ保存は有効なTOMLインラインテーブルでpanicしていた。通常/インライン両形式を扱い、
不正な型は既存ファイルを変更せずエラーとして返す。
端末の同期更新テストも、初期の空フレームで成功しないよう強化し、タイマー経由の副作用を確認した。

同条件の比較では、2,338→2,351関数、clone pair 241→220、group 138→129。
[27組の対応表](../../scripts/refactor-audit/correspondence-changeability.json)に移動先と補助関数を含めた。
未対応の追加/削除関数は0。既存の曖昧な同名10組と、board除外33ノードはhash不変。
認知的複雑度の最大値は入力配送29→17、UI channel橋渡し15→10、CSI-u 13→6、背景描画34→29。
grep/globの統合は表示名・引用符の違いを残すため9→12、9→10となるが、重複した結果解釈を削減した。
数値だけで採否を決めず、protocol表・幾何描画・イベントfold・色導出など14件の維持判断を再確認した。

検証と反映:

- 7段階すべてでfmt、Clippy、nextest、wire schema、WASM previewを通し、main統合後にworkspaceを再ビルド。
  最終コードは `9041ff90`。全体nextest 2,177件成功・14件skip、sandboxed 2,032件成功・83件skip。
- 実HTTP要求で要約のツール無効化、実JSONLで入力のflush/重複排除/保存失敗時の非配送を確認。
  テーマ・sandboxの対象テスト166件成功。Linuxの実helperによる権限境界は全体gateで確認。
- 隔離Xvfbでmarker・256色・truecolor・OSC 8と、UI再起動後の2タブ・2ペイン分割・同じ3端末を確認。
  frame/描画入力の検証でありpixelの目視確認ではない。macOS固有経路はソース確認のみ。
- 抽出ツールの20 fixtureが成功。今回ツール本体の変更は不要だった。最終mainのproduction/testsを別々に解析し、
  ソース一致・設定/実装/実行バイナリの比較互換性を確認。維持判断の根拠hashも更新した。

生成物: [production解析](../../target/refactor-audit/seventh-final/summary.md)、
[tests解析](../../target/refactor-audit/seventh-final-tests/summary.md)、
[前後比較](../../target/refactor-audit/seventh-final-comparison/summary.md)。
次回へ引き継ぐ根拠は[レビュー記録](../../scripts/refactor-audit/reviews.json)に保持する。
