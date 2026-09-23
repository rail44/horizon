# 実行経路と所有関係のリファクタリング第六巡

基準: `901dae76`。board固有実装を除く6領域を、主要経路・関連実装・同種コードまで確認する。
既存仕様・権限・保存/wire形式・イベント順・中断/復元の契約を維持する。

| 領域 | 変更・維持の判断 | 検証 |
| --- | --- | --- |
| runtime通信 | 端末のframe/event/command経路を1つの所有単位に統合。接続失敗後の診断通知は保持。要求期限・再接続・購読終了の順序は維持 | 関連17件、workspace build通過 |
| agentセッション・MoA | 子イベント監視のpanic変換を共有。通常taskとMoAの中断・通知方針、起動/再開/終了時の永続化順は維持 | workspaceと合わせ関連177件 |
| workspace操作 | session/view分割の対象探索を共有。レイアウトの正規化、close/detach/terminate、非アクティブ操作の規則は維持 | 同上 |
| ツールと結果表示 | Web取得を接続/リダイレクト、本文検証、結果変換に分離。検索adapter・SSRF検証・occurrence単位の結果表示は維持 | 関連52件 |
| 端末処理 | 行整形を描画から分離し、文字装飾変換を独立。PTY入出力・query応答優先・frame周期・cache世代/epochは維持 | 端末とmockの関連137件 |
| テスト基盤 | mockの応答シナリオをセッション管理から分離。daemon-testkitの後始末と実daemonによる境界検証は維持 | 同上、プロンプト優先順を追加検証 |

## 再確認で維持した境界

- runtimeは接続全体の失敗と1要求の期限切れを区別し、agent/terminalの独立した寿命を維持。
- agent起動/再開は環境準備と永続化を終えてから通知する。子のtake-once終了、taskの親ターン
  中断後の継続、MoAの中断時終了を統一しない。終了監視の共有後も双方の通知経路を再確認。
- workspaceは対象探索を共有し、タブ/ペインの変更とshellのreconcileを分けたまま保つ。
  session-less view、非アクティブtab、closeとterminate、正規化後の重みの意味を既存テストで固定。
- Web取得の各redirect先でURL・許可domain・DNS・接続先を検証する順序は不変。検索は専用adapterと
  秘密値除去を保ち、短いbody読み込みをエラー表現の異なる取得処理と無理に共通化しない。
  tool結果の表示はoccurrence id優先・旧ログのcall id fallback・承認/拒否/再試行の区別を維持。
- 端末のprotocol表・geometry・paint・cacheは前巡の判断を引き継ぐ。行整形の移動後もlive/scrollback
  双方が同じ関数を呼び、色/font epochと行generationによる無効化範囲は変えない。
- mockは全provider契約の代替ではない。遅いmock応答が途中の通常コマンドを破棄する制約は維持し、
  rigの入力待ち行列・中断・復元はrig/sessionと実daemonのテストで検証。fixtureの共有は既存testkitが担う。

## 検証中に直した準備条件

UI復元チェックが、モデル上の作成直後・daemonのPTY起動完了前にUIを終了していた。
診断dumpのパスに任意の`{session_id}`を指定できるようにし、3端末それぞれの実フレームを
待ってから再起動する。固定パスの従来動作は維持。隔離Xvfbで、追加の固定待機なしに
2タブ・2ペイン分割・同じ3端末と復元フレームを確認した。marker・256色・truecolor・OSC 8も成功。
これはframe/描画入力の検証であり、pixelの目視確認ではない。

## 同条件比較と完了条件

基準の319ファイル/2,331関数から320ファイル/2,338関数へ。追加した7関数と移動先は
[対応表](../../scripts/refactor-audit/correspondence-execution-contracts.json)にすべて含めた。
認知的複雑度の最大値（closure・補助関数込み）は、Web取得21→10、行整形28→17、
mock応答44→18。経路管理は2→3で、効果は数値低下ではなく状態の所有単位の統合。
子監視・分割先探索・診断dumpの値は不変。clone pairは242→241、groupは139→138。
未対応の追加/削除関数は0。同名で曖昧な既存10組はsource hash不変のまま別表示する。
selectマクロ内の複雑さは数値に十分表れないため、実行順序の確認とテストを併用した。

- 変更ごとにbuild、fmt、Clippy、全体nextest、wire schema、WASM previewを通して段階的にmain統合。
  全体nextestは2,172件成功・14件skip。sandboxedは2,028件成功・82件skip。
- 抽出ツールの20 fixtureが成功。前巡の維持判断11件を引き継ぎ、関連変更を再確認し14件に更新。
- board固有の除外33ノードはhash不変。production/testsとも最終mainのソースと一致を確認。

生成物はgit管理外の[最終解析](../../target/refactor-audit/sixth-final/summary.md)と
[前後比較](../../target/refactor-audit/sixth-final-comparison/summary.md)。
判断の根拠は[レビュー記録](../../scripts/refactor-audit/reviews.json)に引き継ぐ。
