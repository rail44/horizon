# 仕様統一の実装記録

第十八巡。[前回の調査](refactoring-spec-simplification-2026-09-25.md)の C01・C02 と、C04・C05・C07 を実装する。関連する旧イベント形式も切替時に整理する。board の機能変更、MoA・AIタイトル生成・自動承認の廃止、Cargo共有キャッシュガードの廃止は含めない。

| 対象 | 状態・変更 |
|---|---|
| C01: DuckDBの残骸 | 常にfalseの移行関数・保存フラグ・getterと、それに依存する分岐を削除。正常利用・差分追従・全再構築は維持。データ形式変更なし。 |
| C02: 旧コマンド名 | CLI・制御API・キーバインドを正規名だけに統一し、実際に使われるヘルプ・設定例・組込みスキルを更新。`new-config-agent` は `new-agent --role config`、`reload-session-runtime` は `reload-agent-runtime` に書き換える。 |
| C04: provider設定 | 未完了。補助AIの接続先の指定方法を確認中。 |
| C07: モデル切替 | daemonが受理時の最新設定で一度解決した内容を渡し、providerが適用後に通知する。セッション内の起動時provider一覧と二重解決を削除し、表示と再接続時の状態は適用済みの値だけ更新する。 |
| C05: 実行ID | 未完了。発行側・保存・再生の統一と一度限りの旧履歴切替を行う。 |

C01・C02の既存sandboxed workspaceテストは2,082件成功、86件skip。正規名によるconfig role起動とruntime reload、DuckDBの復旧経路を既存テストで確認した。各実装単位を必須品質ゲートに通してからmainへ反映する。

C07はsandboxed workspaceテスト2,084件成功。実daemonを使い、起動済みセッション→設定reloadでprovider追加→RPCとコマンド経由の切替→存在しないproviderへの切替拒否→再接続で適用済みの選択を復元、まで通過した。解決後に設定一覧が変わっても受理済みの内容を適用すること、ロール制限・MoA・履歴圧縮状態の維持も検証した。内部の解決済み設定は通信で受け付けず、wire schemaの変更は説明文のみ。
