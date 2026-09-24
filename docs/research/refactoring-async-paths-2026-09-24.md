# 第十五巡：非同期処理の順序と接続の所有

boardを除く8つの経路を確認し、2コミットをmainへ反映した。
基準は `438aa4f3`、実装完了は `d934f7ee`。
[確認範囲と判断](../../scripts/refactor-audit/coverage-async-paths.json)は40ファイル。全行・全並行実行順の網羅検証ではない。

| 経路 | 結果 |
| --- | --- |
| agentの入力・承認・tool完了 | 受付と永続化、重複承認防止、occurrenceによる古い完了の除外、結果前の入力転送を維持 |
| agentの取消・次の処理 | 取消の優先、未処理入力の保留、部分完了したtool群の後始末を維持 |
| runtimeから画面への通知 | **接続ごとの識別を追加**。古いハンドルの破棄・遅れたエラーやイベントが新しい接続に作用しない。workspace-root通知も対象 |
| terminalの入力・出力 | キー／文字／貼り付けのFIFO、PTY照会への優先応答、snapshotの集約とイベント配送の区別を維持 |
| terminalの登録・終了 | **存在確認と登録を同じロック内に集約**。失敗したattachが起動中の通知先を消さず、古いcreateの失敗も新しい登録を消さない |
| 接続待ち・timeout・再接続・reload | timeoutは待機終了であり取消ではない。接続前の再試行、接続後のsticky failure、daemonごとのreloadと復元の世代確認を維持 |
| previewの変更検出・load | 内容変更のdebounce、pane所有のwatch/load task、置換時の破棄を維持 |
| previewの通知・表示 | 古い名前取得の応答を捨てるtask所有と、theme・host rootの寿命を維持 |

接続の識別は内部だけで使い、通信・保存形式やprotocol versionは変更していない。
受付済みRPCの副作用を取り消したり、独立したRPCの完了順を統一したりはしない。
terminalのwatchより手前には既存の非有界キューが残る。経路全体のメモリ上限を保証する変更ではない。

新規回帰テストは**7件**。古い登録の破棄・通知、失敗したattach/createの5件は修正前の失敗を確認した。
残る2件は実際のremoc経路で、旧ハンドル破棄後も新しいagent/terminalの送受信が継続することを確認する。
fmt、Clippy、sandboxed nextest **2,081件**、default nextest **2,230件**、wire、WASM library、workspace buildを通過。

統合後の隔離環境で、端末の色・OSC8・文字のframeデータ、2タブ・分割・同じ3セッションの復元、
X11のキー／文字入力・Enter・Tabを確認した。実WASM componentの描画データ・theme反映・再読込も通過。
**未検証**：画素単位の見た目、実provider接続、物理IME・Wayland・他OSでの実動作。
利用者のアプリ・daemonは再起動していない。board除外33構文片と、変更ファイル内のboard専用分岐は不変。

抽出ツール自体の変更は不要だった。[本体1組](../../scripts/refactor-audit/correspondence-async-paths.json)と
[テスト3組](../../scripts/refactor-audit/correspondence-async-paths-tests.json)で新しい補助処理・回帰テストを対応付け、他は条件別実装を含め自動比較した。
本体は **346ファイル / 2,380関数 → 346 / 2,382**、テストは **263 / 2,721 → 263 / 2,728**。
clone pairsは本体219→221、テスト993→998。識別の照合と再現手順による増加であり、数値低下を成果にはしない。
対応不明の追加・削除・曖昧な識別子は0。維持判断17件は全件有効で、terminal関連の根拠1件を更新した。

最終clean mainの解析は `target/refactor-audit/fifteenth-final{,-tests}/`、比較は各`-comparison/`、
整合性確認は `fifteenth-final/verification.json`。実動作ログは `/tmp/horizon-fifteenth-{gui,preview-e2e}.log`。
