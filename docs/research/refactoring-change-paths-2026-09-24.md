# 変更経路を追うリファクタリング第八巡

基準 `796043ab` から、boardを除く6経路を入口・呼び出し側・同種実装・関連テストまで確認した。
[経路ごとの想定変更と判断](../../scripts/refactor-audit/coverage-change-paths.json)を記録した。
今回は経路を横断するレビューであり、全ファイルを再び逐行確認したという意味ではない。

| 経路 | 変更・結果 | 維持した境界 |
| --- | --- | --- |
| ツール定義→承認→実行→表示 | 要求と結果の対応を共有し、本文を対応済みの位置から取得。行の展開状態も実行ごとに独立 | ツール権限、承認判断、実行処理、既存の承認コマンド |
| agent起動→入力→中断→再開 | 共通の対応付けから未完了の実行を抽出し、それぞれを中断済みとして保存 | 入力のflush後配送、重複排除、処理順、taskとMoAの寿命の差 |
| イベント→保存→復元→画面 | recallの2クエリで結果と要求の結合を共通化。再利用IDによる行の増殖・誤ったツール名を修正 | JSONL正本、DuckDB派生、検索条件・並び・総件数・出力上限 |
| CLI・キー・画面→workspace操作 | 現行のcommand経由の構造を維持 | CLIの明示対象と現在ペイン、close/detach/terminate、確認・focus |
| 設定→モデル選択・再読込・テーマ | モデル検証を共通化。送信先が終了済みなら切替成功を通知しない | 呼び出し側ごとの設定世代、role制約、設定項目ごとの適用時点 |
| terminal・preview・runtime | 現行の所有関係と接続処理を維持 | daemon別の再読込、接続前後の再試行差、購読とpreview処理の終了責任 |

主な不具合は「同じcall IDでも別の実行」という条件を、表示・復元・検索が別々に扱っていたことによる。
本文、JSONL復元、recall、終了済み送信先の回帰テストは修正前の失敗も確認した。
設定再読込後のモデル切替については、受付が現在設定・実行が開始時設定を使う既存仕様を維持した。
表示と実行がずれる場合があることも[既存の合意](../agent-output-ui-amendment.md)に含まれるため、今回の共通化では変更していない。

同条件解析はproduction 323→325ファイル、2,351→2,357関数、clone pair 220→219、group 129→128。
[4組の対応表](../../scripts/refactor-audit/correspondence-change-paths.json)に移動先・補助関数を含めた。
認知的複雑度の最大値は対応付け・復元21→12、モデル選択9→5。表示と検索の最大値は不変。
未対応の追加/削除関数は0。既存の曖昧な同名10組とboard除外33ノードはhash不変。
維持判断14件のうち根拠が変わった2件を更新し、残る12件は根拠hash不変を確認した。

検証と反映:

- 5段階でmainに統合し、その都度workspaceを再ビルド。最終コードは `a967cc6a`。
  fmt、Clippy、wire schema、WASM previewが成功。全体nextestは2,182件成功・14件skip、sandboxedは2,037件成功・83件skip。
- 実JSONLの復元、実daemonの中断・再開、モデル選択と承認の既存テストを含めて検証した。
  隔離Xvfbではmarker・256色・truecolor・OSC 8と、UI再起動後の2タブ・分割・同じ3端末を確認。
  描画入力とframeの検証であり、pixel目視や実IME操作の検証ではない。
- 途中の全体gateで既存DuckDBの `PhysicalWindow` 内部エラーを1回観測した。
  総件数を同じfiltered CTEから計算する形に変更。変更前後それぞれ120回の実行では再現せず、再発防止を実証したとは扱わない。
- 抽出ツールの20 fixtureが成功。今回ツール本体の変更は不要だった。最終mainのproduction/testsを別々に解析し、
  ソース一致と設定・実装・実行バイナリの比較互換性を確認した。

生成物: [production解析](../../target/refactor-audit/eighth-final/summary.md)、
[tests解析](../../target/refactor-audit/eighth-final-tests/summary.md)、
[前後比較](../../target/refactor-audit/eighth-final-comparison/summary.md)。
