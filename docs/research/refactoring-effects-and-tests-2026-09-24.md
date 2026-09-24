# 第十一巡：副作用・判断・テストの責務

boardを除くagent・daemon・runtime・workspace・設定・terminal・previewを、
本体とテストの両側から9つの責務に分けて確認した。基準は `6e8efdfa`、実装は `0fe5d2be`。
[確認範囲と判断](../../scripts/refactor-audit/coverage-effects-and-tests.json)には49ファイルの根拠を記録した。
全行の再監査を意味しない。

| 対象 | 変更と効果 |
| --- | --- |
| daemonの共通fixture | ホストの環境変数を読まず、明示的な設定を生成。重複する初期化も共通化し、テストが実際のAPI設定・保存先に左右される問題を解消 |
| 一時ディレクトリ | 共通fixtureをテスト専用モジュールへ移し、`TempDir`で所有。途中失敗時も片づく。非repositoryの確認は実ファイルシステムで継続し、組込みskillのテストは探索を不要にした |
| providerの再試行 | 本体の分類・待機境界を維持し、Tokioの仮想時計で検証。待機途中のキャンセルと、期限到達との競合を直接再現 |
| terminalの通信路 | terminal-coreが10種類の送受信ペアを生成し、receiverの内部を非公開化。daemonと10個のテストから個別の組立てを除去。入力所有者の終了でコアも停止することを検証 |
| previewの更新待機 | 定数の大小だけを確認するテストを、GPUIの仮想時計で実際の連続更新・再更新・終了時の取消しを確認する3テストへ置換 |

復元の「候補選択→接続→モデル反映」、runtimeの要求・経路・再接続、設定の値解決とI/O、
永続化の確認後に公開する順序は維持した。既存の失敗注入・起動時バリア・rollback検証があるため、
テスト専用の汎用I/O層や時計インターフェースは追加していない。
実OS・PTY・socket・signalの検証は引き続き実時間を使う。SIGTERMの既存テストも
750 msの起動待機を使用しており、決定的なスケジューリングの証明ではない。

検証はfmt、Clippy、sandboxed nextest **2,063件**、統合フックのdefault nextest **2,209件**、
wire schema、preview WASM、workspace buildを通過。環境変数依存の回帰テストは修正前の失敗も確認した。
隔離GUIで端末の文字・色・リンクと、2タブ・分割・同じ3セッションの復元を確認した。
利用者のアプリは再起動していない。pixel表示・物理キー・IME入力の検証は含まない。

同じ設定・固定ツールでproduction/testsを別々に再解析した。
[本体1組](../../scripts/refactor-audit/correspondence-effects-and-tests.json)と
[テスト5組](../../scripts/refactor-audit/correspondence-effects-and-tests-tests.json)の対応に、移動先・補助関数・置換した検証を含めている。

| 集計 | 基準 → 完了時 |
| --- | --- |
| production：ファイル / 関数 | 328 / 2,373 → 329 / 2,374 |
| production：重複ペア / グループ | 220 / 129 → 219 / 128 |
| tests：ファイル / 関数 | 251 / 2,692 → 254 / 2,700 |
| tests：重複ペア / グループ | 1,025 / 239 → 989 / 237 |

数値の減少を成果条件にはしていない。対応不明の追加・削除は両区分とも0。
同名実装の曖昧さはproduction 10組・tests 1組ともハッシュ不変を確認した。
board除外33構文片と通信形式は不変。変更したRust 26ファイルはすべて確認記録に含めた。
既存の維持判断14件のうち3件に今回の根拠を追記し、元の判断を保持した。
追加依存はdev用の既存`tempfile`とTokio `test-util`のみで、既存パッケージの更新はない。

抽出ツールの変更は不要で、21検証も通過した。最終mainの生データは
`target/refactor-audit/eleventh-final{,-tests}/`、対応比較はそれぞれの`-comparison/`、
整合性確認は`eleventh-final/verification.json`に置く。
