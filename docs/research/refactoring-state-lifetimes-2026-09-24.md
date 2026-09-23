# 状態と非同期処理のリファクタリング第九巡

基準 `93a003ad` から、boardを除く4領域で正常終了・途中失敗・中断競合・再開を確認した。
[対象と判断](../../scripts/refactor-audit/coverage-state-lifetimes.json)には呼び出し側・同種実装・テストの根拠を残した。
ファイルhashは調査根拠の識別用であり、全ファイルを逐行監査したという意味ではない。

| 領域 | 変更・確認できた効果 |
| --- | --- |
| 状態の正本と更新責任 | 永続イベントは保存確認後にLiveStateへ反映。providerの中断済みID集合を削除し、未完了マップに一本化。同じIDを再利用した新しい結果が捨てられなくなった |
| 非同期処理の寿命 | bashの登録をキュー投入から終了まで保持。待機中の取消し、起動との競合、古い登録の破棄を修正。完了通知の型を独立させ、bash・web・承認判定の試行IDを受信入口で照合 |
| 保存と復元 | JSONLの書込み・flush失敗を保持し、成功応答やDuckDBだけの更新を防止。未完了末尾を除去してから追記し、UTF-8途中の切断にも対応。復旧イベントの保存失敗時はそのsessionの再開を止める |
| 境界の検証・エラー規約 | 設定ファイル置換失敗時の一時ファイルを片付ける。command経由の操作、runtimeのエラー分類、tool結果の規約を確認し、意味の異なる処理は維持 |

維持した境界: JSONL正本とDuckDB派生、通常streamingの非同期保存、fs.editの順序付き部分成功、
close/detachとterminate、taskとMoAの寿命、daemon別の再読込、timeoutは待機終了という契約。
設定再読込後のモデル選択で受付と実行の設定世代が異なる既存合意も変更していない。
通信・保存形式と権限は不変。flushはOS page cacheまでで、fsyncや複数レコードのtransactionを追加したものではない。

検証・反映:

- 主要な不具合は修正前の失敗を再現した。実プロセスの取消し、実JSONLの末尾復旧、書込み/flush失敗注入、古い完了通知、ID再利用、設定置換失敗を検証。
- 実daemonでも、壊れた行とUTF-8途中の末尾から起動し、次のユーザー入力・応答が保存されることを確認。既存のkill/drain後の復元テストも成功。
- fmt、Clippy、wire schema、WASM preview、workspace buildが成功。nextestは通常2,198件成功・14件skip、sandboxed 2,053件成功・83件skip。
  Linuxでの自動検証であり、macOS実機・GUIのpixel/IME操作を確認したという意味ではない。
- コード6段階と比較ツール1段階をmainへ統合し、その都度workspaceを再ビルド。製品コードの最終変更は `0d6186ec`、ツール改善は `729ce261`。

比較ツールには、OS別の同名関数を明示的にまとめる `variants: all` を追加した（21 fixture成功）。
通常の曖昧な識別子は引き続き自動対応しない。改善後の同じツールで基準/最終コードを再解析し、
基準のRust source hash・測定値が元の解析と一致することも確認した。
[5組の対応表](../../scripts/refactor-audit/correspondence-state-lifetimes.json)には移動先・補助関数・OS別実装を含む。

productionは325→327ファイル、2,357→2,367関数、clone pair 219・group 128は不変。
testsは248→249ファイル、2,663→2,683関数。provider状態処理の認知的複雑度最大は10→9、
保存・非同期通知・bashは不変、設定の後始末は1→2。数値低下を成果の条件にはしていない。
未対応の追加/削除関数は0、残る同名7組とboard除外33ノードはhash不変。
維持判断14件のうち根拠が変わった3件を更新し、11件は変更していない。

生成物: [production](../../target/refactor-audit/ninth-final/summary.md)、
[tests](../../target/refactor-audit/ninth-final-tests/summary.md)、
[前後比較](../../target/refactor-audit/ninth-final-comparison/summary.md)、
[ソース・比較条件・判断根拠の一致検証](../../target/refactor-audit/ninth-final/verification.json)。
