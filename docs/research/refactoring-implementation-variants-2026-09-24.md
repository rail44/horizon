# 第十三巡：同じ責務を持つ実装の整合性

boardを除く9組を確認し、共通契約を一か所で変更できるよう整理した。
基準は `e931b5d2`（ツール更新のみ、Rustは `23455eeb` と同じ）、実装完了は `398b746a`。
[確認範囲と判断](../../scripts/refactor-audit/coverage-implementation-variants.json)に43ファイルの根拠を記録した。全行の再監査ではない。

| 対象 | 結果 |
| --- | --- |
| provider | 要求生成・ストリーム収集・再試行の既存共通処理を維持。両adapterを実際のHTTP/SSEで検証。Anthropicの失敗をOpenAIと表示する診断文を修正 |
| 承認 | 人間・自動承認の実行先の振り分けを共有。候補の一致確認、承認元、権限の再検証、二重実行防止は維持 |
| 子プロセス | Linux本番・Linuxテスト・macOSでcwd、環境変数の指定と削除、TMPDIR、標準入出力の引き渡しを共有。OS固有の起動・権限適用は各実装に残す |
| 出力・終了 | 非同期・同期の読み取り規則を共有し、Interruptedで出力を途中で捨てる問題を修正。終了処理の重複したOS別ラッパーを除去。待機・drain期限・Linuxの子孫探索は維持 |
| native/WASM | 共有型・テーマ解決と、host/guest固有の通信・ライフサイクルの境界を維持。実WASM部品の描画・テーマ反映・再読み込みを確認 |

同期結果と非同期完了、通常実行と拒否後の再試行、Linuxの認証済み監督レポートとmacOSのログ収集、
Linux/FreeBSDのprimary selectionは意図的な違いとして維持した。通信・保存形式と依存関係は不変。

抽出ツールは`cfg`・同一ファイル内の所属を識別子と根拠ハッシュに含めるよう改善した。
テスト抽出時に消える型の所属も保持し、従来曖昧だった本体10組・テスト1組を区別できる。
旧レビュー形式は引き続き使用可能。条件の評価・外部モジュールの条件伝播・マクロ展開は行わない。

検証：ツール27件、fmt、Clippy、sandboxed nextest **2,072件**、default nextest **2,219件**、
wire、WASM library、workspace buildを通過。ローカルproviderと実Linux sandbox helperは統合後mainでも確認した。
WASMは実componentをビルドし、board以外のテーマ・再読み込みテストを実行した。
FreeBSD向けsandbox crateの全targetコンパイルも通過したが、実行確認ではない。
**macOSは未検証**：cross-checkが依存の`aws-lc-sys`で停止し、ホストCコンパイラがDarwin用オプションを受け付けなかった。
物理GUI・IME・clipboard、macOS/FreeBSD上の実動作、non-Unixのプロセス終了は確認していない。

[本体4組](../../scripts/refactor-audit/correspondence-implementation-variants.json)・
[テスト4組](../../scripts/refactor-audit/correspondence-implementation-variants-tests.json)の対応に移動先・補助関数・条件別実装を含めた。
本体は **331ファイル / 2,376関数 → 333 / 2,376**、テストは **255 / 2,708 → 259 / 2,717**。
対応不明の追加・削除・曖昧な識別子は0、board除外33構文片は不変。変更したRust13ファイルを確認記録に含めた。
維持判断は14件のうち3件の根拠を更新し、OS条件別の3件を追加した。

最終mainの生データは `target/refactor-audit/thirteenth-final{,-tests}/`、比較は各`-comparison/`、
整合性確認は `thirteenth-final/verification.json`。利用者のアプリ・daemonの再起動は行っていない。
