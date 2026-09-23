# 実行・操作経路のリファクタリング第四巡

開始: `ceede795`。下記5領域を呼び出し先・関連実装・テストまで確認し、変更を段階的にmainへ
反映した。board UI・CLI・horizon-board・logd・board連携専用directory・共通ファイル内の
board固有処理は対象外。既存の操作・表示・権限・停止・復元・保存形式を維持した。

| 領域 | 変更 | 維持した境界・理由 |
| --- | --- | --- |
| セッション寿命 | 在庫取得・接続・モデル反映を分離。端末entity生成と通知配線を集約 (`5827fdd9`) | 選定前と反映前に両runtimeの世代を確認。terminalのAttach成功とagentの非同期接続は完了契約が異なる。daemonの購読登録・終了通知・lifecycle lockの順序も維持 |
| 端末操作・描画 | scrollbackの状態遷移をsessionの通信・表示更新から分離し、両端の履歴要求を共通化 (`6473dcb5`) | 取得中の要求は重複送信せず最新の小数行位置を保持。履歴とliveはcacheのindex・generationが異なり、cursor・selection・IMEはlive専用なのでpaint経路を維持 |
| agent状態・表示 | 純粋な表示行projectionをGPUI描画から分離。burstの終了処理を統一 (`3b338101`) | receiptの範囲とkey、thinkingの表示条件を維持。foldとstatusは履歴と最新稼働状態を区別する既存の配置を維持 |
| 個別ツール | readの取得と整形、grepの検証・走査・出力を分離。Git/Cargo共通のshell解析を独立。出力再利用の逆順走査を単純化 (`f120f173`) | ignore・mtime・read/writeの権限差・出力上限・承認経路を維持。字句解析の状態機械と、budgetの異なるglob/grepの収集処理は維持 |
| 抽出ツール | 構文単位の除外、移動・分割の明示的な対応表、根拠hash付きの判断記録を追加 (`16dcdd84`) | 同条件の完全なreportだけを比較。判断の根拠が変わった場合は再確認対象とし、結果を隠さない。未記録の依存先まで妥当とは扱わない |

抽出ツールの実用上の改善は、既存の解析器を使ったまま、比較と再レビューを再現できるように
した点。責務の適切さや仕様維持の判断は引き続きソース・呼び出し先・テストの確認が必要。
boardのmacro宣言や混在する配線は一律には除けないため、変更境界を手作業でも確認する。

## 同条件の前後比較

最終の除外設定で開始時のRustソースを再解析した。基準reportのcommitは`16dcdd84`
（`ceede795`からRust変更なし）。最初の318ファイル・2342関数という値からの除外による減少は
改善に含めない。同条件では313→316ファイル、2308→2317関数、clone pair 244→243、
clone group 140→140。件数の増減自体を品質評価には使わない。

下表は認知的複雑度。分割後は入口だけでなく対応する補助関数を含む最大値で比較した。

| 対応する責務 | 前 | 後 |
| --- | ---: | ---: |
| workspace復元と端末entity配線 | 54 | 24 |
| scrollbackのwheelと履歴要求 | 65 | 49 |
| fs.readの取得と行窓の整形 | 26 | 19 |
| fs.grepの検証・走査・出力 | 36 | 20 |
| 出力再利用の探索 | 41 | 14 |
| burstの分割・終了 | 26 | 13 |

scrollbackのwindow管理、表示行projection、shell字句解析は所有箇所の整理で、複雑度は不変。
対応表で説明できない追加・削除関数は0。既存の条件付き実装など、同名で一意に対応しない
10組は曖昧なまま別表示し、改善とは数えない。高い値が残る状態機械・描画・protocol表も、
数値だけを理由に分割しない。

## 検証と再利用

- 変更段階ごとにworkspace build、fmt、Clippy、全体nextest、wire schema、WASM previewを確認。
  最終の全体nextestは2151件成功・14件skip。抽出ツールの回帰fixtureは20件成功。
- 関連テストで復元・世代棄却、scrollbackのlive復帰・resize・stale response、deltaの昇格位置、
  burst/表示行、read/grepの上限・権限・ignore、Git/Cargo分類、出力再利用の時系列を確認。
- 隔離XvfbでUI再起動後の2タブ・2ペイン分割・同じ3端末を確認。marker・256色・truecolor・
  OSC 8のframe dumpも確認した。これは描画入力の検証であり、pixelの目視確認ではない。
- production/testsの解析が完了し、最終mainのソースhashと比較対象の一致を確認した。

入口は[利用手順](../refactoring-review.md)。継続利用する設定・判断は
[除外profile](../../scripts/refactor-audit/profiles/non-board.json)、
[判断記録](../../scripts/refactor-audit/reviews.json)、
[今回の対応表](../../scripts/refactor-audit/correspondence-cross-domain.json)に残した。
生成物はgit管理外の[最終解析](../../target/refactor-audit/fourth-final/summary.md)と
[前後比較](../../target/refactor-audit/fourth-final-comparison/summary.md)。
