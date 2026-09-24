# 第十四巡：workspace・terminal・agent画面の責務

boardを除く3領域を8つの操作経路で確認し、4コミットをmainへ反映した。
基準は `9af4e934`、実装完了は `9f820cce`。
[確認範囲と判断](../../scripts/refactor-audit/coverage-shell-responsibilities.json)に52ファイルの根拠を記録した。全行の再監査ではない。

| 領域 | 変更 | 維持した境界 |
| --- | --- | --- |
| workspace | セッション生成、通知、runtime再読込、preview操作、ナビゲーションを分離 | モデルとsession/entityの所有、復元中の操作制限、古いruntimeの結果を捨てる確認、close/detach/terminateの区別 |
| terminal | キー・IME状態、ポインター状態、描画・キャッシュ、フォント、診断出力を分離 | IMEの重複送信防止、ライブ画面と履歴画面の描画・キャッシュの違い、OS別primary selection |
| agent画面 | transcriptが表示状態・展開状態・仮想リストを所有。投影と更新判定、個別行、receipt、変更一覧を分離 | composer/status/tasksの独立した通知、呼出し出現位置による行識別、承認・停止・続行の既存操作 |

隔離X11検証で、末尾の文字よりEnterが先に処理される既存不具合を再現した。
daemonが文字・キー・貼り付けを別々のキューへ分け、coreが順不同に選んでいた。
内部の `CoreInput` FIFOにまとめ、これらの到着順を保つよう修正した。PTYからの照会への優先応答と、
入力の種類ごとの画面更新規則は維持した。mouse/scrollの別キュー、通信・保存形式、protocol versionは変更していない。

検証はfmt、Clippy、sandboxed nextest **2,076件**、default nextest **2,223件**、wire、WASM library、workspace buildを通過。
新規テストは表示更新3件と入力順序1件。後者は96件を先に積む手順で修正前の失敗、修正後の成功を確認した。
workspaceの全関数本体は移動前後で構文トークンが一致する。board固有コードは不変。

統合後の隔離GUIで、色・OSC8リンクのframeデータ、2タブ・分割・同じ3セッションの復元、
X11のキー／テキスト入力・Enter・Tabを確認した。入力失敗と修正後は同じ操作手順を使った。
**未検証**：画素単位の見た目、agent画面の実provider接続、物理IME・Wayland・OS clipboard、macOS/FreeBSDでの実動作。
利用者のアプリ・daemonは再起動していない。

抽出ツール自体の変更は不要だった。[本体17組](../../scripts/refactor-audit/correspondence-shell-responsibilities.json)・
[テスト9組](../../scripts/refactor-audit/correspondence-shell-responsibilities-tests.json)の対応表に、移動・補助関数・条件別実装を含めた。
本体は **333ファイル / 2,376関数 → 346 / 2,380**、テストは **259 / 2,717 → 263 / 2,721**。
clone pairsは本体218→219、テスト991→993。数値の低下を成果とはせず、責務の所有と変更箇所の局所化を評価した。
対応不明の追加・削除・曖昧な識別子は0、board除外33構文片は不変。既存の維持判断17件のうち5件の根拠を更新した。

最終clean mainの生データは `target/refactor-audit/fourteenth-final{,-tests}/`、比較は各`-comparison/`、
整合性確認は `fourteenth-final/verification.json`。GUI検証ログは `/tmp/horizon-fourteenth-ordered-gui.log`。
