# 第十巡：型・公開API・依存境界

boardを除く主要APIを10の責務に分け、利用側・同種実装・テストまで確認した。
目的は、生成や更新の条件を所有者の入口で守り、呼び出し側の知識を減らすこと。
基準は `5c6ea7c2`。確認範囲とソースのハッシュは
[coverage-api-boundaries.json](../../scripts/refactor-audit/coverage-api-boundaries.json) に記録した。
ファイル全行の再監査を意味しない。

| 対象 | 変更と効果 |
| --- | --- |
| ツールの再試行 | 完了結果と別に持っていたcall IDを削除。受付と再試行が同じ結果のIDを使う |
| セッション初期設定 | `ToolSessionBuilder`で設定後に共有状態を生成。clone後の設定が黙って無視されるAPIをなくした |
| 一時通知 | 進捗・モデル・選択・子タスク通知の保存除外をAppenderに集約。直接呼出しでも仮イベントが履歴へ入らず、処理・送信でも通知の種類を維持する |
| workspace | 内部コレクションを直接変更できない形にし、既存操作へ統一。セッション生成に`SessionKind`を要求し、viewの混入を型で防ぐ |
| terminal | 生成・リサイズでPTYとコアに同じ最小セル寸法を適用。0寸法によるpanicを解消し、通常寸法・pixel情報は維持 |
| 拒否結果 | 本文のerrorフラグに依存せず、既存契約どおり拒否を必ずエラーにする |

設定ローダー→daemon→agentの変換、runtimeごとの通信・寿命、previewの外部公開部分は維持した。
特に、モデル設定の世代差、実行中の権限再検証、closeとterminate、公開された読取用snapshotと
内部可変状態の違いは残している。board固有処理と通信・保存形式は変更していない。

検証はfmt、Clippy、sandboxed nextest **2,058件**、統合フックのdefault nextest **2,204件**、
wire schema、preview WASM、workspace buildを通過。進捗通知・拒否結果・端末寸法の
5つの回帰テストは修正前の失敗も確認し、実terminaldのPTY寸法検証を1件追加した。
外部クレートのコンパイル検証で不正なAPI利用が拒否され、正しい利用が通ることも確認した。
隔離GUIでは端末の文字・色・リンクと、2タブ・分割・同じ3セッションの復元を確認した。
pixel表示や物理キー・IME入力の検証ではない。利用者のアプリ・デーモンは再起動していない。

解析は同じ非board設定と固定ツールでproduction/testsを別々に実行した。
移動・補助関数を含む[6組の対応](../../scripts/refactor-audit/correspondence-api-boundaries.json)で比較する。

| 集計 | 基準 → 完了時 |
| --- | --- |
| production：ファイル / 関数 | 327 / 2,367 → 328 / 2,373 |
| production：重複ペア / グループ | 219 / 128 → 220 / 129 |
| tests：ファイル / 関数 | 249 / 2,683 → 251 / 2,692 |

数値の減少を成果条件にはしていない。productionの未対応追加・削除は0。
同名のOS別実装10組は勝手に対応づけず、各実装のハッシュ不変を確認した。
board除外33構文片も不変。既存の維持判断14件のうち影響した3件を再確認し、根拠を更新した。
テスト側の9関数増は回帰テスト6件とfixture／構築補助3件で、削除はない。

ツール本体の変更は不要だった。解析ツールの21検証も通過。
依存方向はlocked Cargo metadataとcargo-modules 0.26.0でも照合したが、後者はhost/default cfgのみ。
最終mainの生データは `target/refactor-audit/tenth-final{,-tests}/`、比較は
`tenth-final-comparison/` と `tenth-final-tests-comparison/`、追加整合検証は
`tenth-final/verification.json` に置く。
