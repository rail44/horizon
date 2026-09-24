# 第十二巡：層をまたぐデータの解釈・変換

boardを除く主要経路を9つに分けて確認した。基準は `515e610c`、実装完了は `aa263604`。
[確認範囲と判断](../../scripts/refactor-audit/coverage-data-interpretation.json)に84ファイルの根拠を記録した。全行の再監査ではない。

| 対象 | 変更と効果 |
| --- | --- |
| ツール結果 | 結果の型・再試行の識別情報をcontractへ集約。実行側から表示層への依存を除去。DuckDBが本文JSONから失敗を再判定する問題を修正し、型の明示的な結果を保存 |
| ツール表示 | 分類結果に既知・未知を明示。表示文言からの推測と重複した分類を除去。未知ツールのJSON表示や既存の要約は維持 |
| provider履歴 | 呼び出しIDの再利用でも実行したツール名を保持。再試行の途中結果や重複した回答をprovider履歴へ混入させず、未回答の過去の呼び出しも個別に補修 |
| コマンド応答 | セッション種別をDebug文字列から生成せず、既存の`SessionKind.label()`を使用。出力は同じ |
| terminalの色 | 描画とOSC問い合わせ応答の固定パレット計算を共有。テーマ色・上書き優先順位・dim表示の違いは維持 |

providerには1回の要求に対する最終結果を渡し、表示・監査には各実行試行を残す。
復元時は未完了の同ID・同入力の再発行を同じprovider呼び出しに対応づけ、
新しいuser入力・provider要求・turn終了でその関係を切る。実行識別子付きの結果を別の呼び出しへ代替対応させない。
隣接assistantメッセージの結合など既存の復元補修は維持した。
並列結果を別のassistantメッセージ越しに並べ直す処理は従来どおり行わない。

イベントのlive/replay共通処理、保存確認後の公開順序、runtimeの受付・完了・接続失敗の区別、
設定の優先順位と実行中セッションの設定世代、previewの共有型とテーマ解決は維持した。
通信形式・保存形式・依存関係は変更していない。wire artifactの差分は説明文1か所のみ。
既存DuckDB行の一括修正や利用者のアプリの再起動は行っていない。

検証はfmt、Clippy、sandboxed nextest **2,069件**、統合フックのdefault nextest **2,215件**、
wire schema、preview WASM、workspace buildを通過。新しい回帰6件は修正前の失敗も確認した。
再試行履歴はDuckDBとイベント履歴の両経路で検証。隔離GUIでは端末の文字・色・リンクと、
2タブ・分割・同じ3セッションの復元を確認した。pixel表示・物理キー・IMEの確認は含まない。

同じ固定ツール・設定でproduction/testsを再解析し、[本体4組](../../scripts/refactor-audit/correspondence-data-interpretation.json)と
[テスト3組](../../scripts/refactor-audit/correspondence-data-interpretation-tests.json)に移動先・補助関数・回帰検証を含めた。
productionは **329ファイル / 2,374関数 → 331 / 2,376**、testsは **254 / 2,700 → 255 / 2,708**。
重複ペア・グループ数はproduction **219 / 128**、tests **989 / 237**で不変。数値の減少を成果条件にはしていない。
対応不明の追加・削除は0。同名実装の曖昧さはproduction 10組・tests 1組ともハッシュ不変、
board除外33構文片も不変。変更したRust 26ファイルをすべて確認記録に含めた。
維持判断14件のうち2件の根拠を更新し、元の判断を保持した。

抽出ツールの変更は不要で、21検証も通過した。最終mainの生データは
`target/refactor-audit/twelfth-final{,-tests}/`、対応比較は各`-comparison/`、
整合性確認は`twelfth-final/verification.json`に置く。
