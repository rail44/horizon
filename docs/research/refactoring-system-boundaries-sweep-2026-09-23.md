# 設定・外部境界・永続化のリファクタリング第五巡

基準: `fd75af25`。6領域の確認・変更を`d5d28ca6`までにmainへ統合し、再ビルド済み。
board固有実装を除き、主要経路から関連実装・テストまで確認した。
既存の仕様・権限・保存形式・イベント順序を維持し、数値だけを理由に分割しない。

| 領域 | 確認・変更 | 状態 |
| --- | --- | --- |
| 設定・テーマ | 読み込みとreload、警告、反映、保存。MoAの解決と診断を一元化 | 関連203テスト通過 |
| コマンド・操作 | CLIのオプション読み取り・コマンド構築・未使用検証を分離 | 関連161テスト通過 |
| provider・履歴管理 | 通信待機と受信内容の集約を分離。履歴圧縮・再試行条件を確認 | 関連183テスト通過 |
| 永続化 | live追記と再構築のトランザクションを統一。検索・復元を確認 | 関連77テスト通過 |
| sandbox・ネットワーク | 基本権限・承認grant・通信設定の構築を分離 | 関連90テスト通過 |
| preview・WASM | 古い応答待ちの所有とreload時の終了を明示 | 関連15テスト、実WASM 2テスト通過 |

設定・テーマ: MoAの採用条件と警告生成を`moa.rs`の同じ解決処理に集約した。
名前・aggregator・proposerの検証順、重複したmemberとentryの順序、元から空のproposer一覧と
検証で空になった一覧の警告の差を、変更前後で同じ回帰テストにより確認した。
loaderは起動時fallbackとreload失敗時の現状維持が異なるため維持。daemonの設定反映は
既存sessionの起動時設定を変えず、次のsessionから適用する。themeはseed解決・live適用・
編集・TOMLの書き戻しが分離済みであり、他のsection・commentを保持する保存処理も維持。
旧keybindingの解除と新設定の登録順、themeのpreviewへの通知と端末palette再送も確認した。

コマンド・操作: CLIの読み取りを`cli/options.rs`へ分離し、値付きオプションの処理を共通化した。
`=`を含む値、flagに見える値、同じ指定の上書き、引数個数エラーと未使用オプションの
優先順位を変更前後のテストで固定した。CLIの外部名とGUIのCommandIdは異なる操作面を
表すため対応表を維持。control planeの型付き検証、通常要求とmodel変更の完了待ち、
キー・paletteから共通executeへの経路も確認し、異なる完了条件の無理な共通化は行わない。

provider・履歴管理: `completion/response.rs`がdeltaの集約、tool要求、本文確定と
履歴復元を所有する。raw payload保存と実行引数修復の順序、途中失敗時の本文未確定と
tool要求済みの再試行禁止、中断時の部分履歴、usageと未完了callによる打切り判定を検証。
要求構築・中断・timeoutは通信側に残す。typed errorの再試行判定、圧縮の出現回数と
現在のcall単位の保護、canonical履歴を変更しないprovider向け投影は維持した。

永続化: live追記と再構築chunkの書き込みを`append_records_atomic`へ統一した。
派生turn行の失敗でeventと高水位も戻り、次の追記が成功することを変更前後で検証。
JSONLの単一writer・破損行と未解釈eventのsequence保存、復元時のturn ID、共有DB接続、
chunkを二分して不良recordだけを除く再構築、SQL側の検索件数・本文上限は維持した。

sandbox・ネットワーク: capability構築を基本のfilesystem、承認grantの再検証、networkへ
分離した。grantのcanonical path/type再照合と、filesystem→loopback→proxyの検証順を維持。
Linuxの除外subpath優先、十分な権限の最長path選択、通知ID再確認と複製socketによる接続、
認証済み拒否reportからbash結果への返却を追跡した。proxyの完全一致allowlistと拒否記録も維持。
macOSの拒否収集は発生源・失敗時の扱いがLinuxと異なるため共通化せず、ソース確認のみ。

preview・WASM: detachedだったpreview名の応答待ちをpane所有にし、reload時にloadと共に
終了させる。旧応答がEmptyをLoadedへ戻す問題を変更前の失敗テストで再現し、修正後に確認。
host/guestのtheme通知・subscription所有、artifactのdirectory監視・debounce・read除外を確認。
実WASMで描画、theme通知、artifact交換、旧store/thread解放、破損artifactの失敗を確認した。

比較は分割先・補助関数・closureを含む。認知的複雑度の関数最大値は、MoA 21→10、
CLI 29→10、provider 17→8、sandbox 32→15。永続化3→3、preview 5→5は、
重複した責務と非同期処理の所有を整理した。数値の低下自体を完了条件にはしていない。
対象は316→319ファイル、2317→2331関数。今回の全変更を6組の対応表で説明でき、
未対応の増減・変更は0。曖昧な関数識別10組は内容の一致を別途確認した。
boardの構文除外33箇所は変更なし。維持判断は既存5件に今回の6件を追加した。

検証: workspace build、fmt、Clippy、全体2163テスト通過（14 skip）、wire schema、
WASM buildを通過。別途、実WASMの2テストと解析ツールの20検証も通過した。
macOS固有の実行は未検証。解析の設定・実装・ツールbinaryは基準時から同一。
[対応表](../../scripts/refactor-audit/correspondence-system-boundaries.json)と
[維持判断](../../scripts/refactor-audit/reviews.json)を保存した。
生成結果: [最終main解析](../../target/refactor-audit/fifth-final/summary.md)、
[同条件の比較](../../target/refactor-audit/fifth-final-comparison/summary.md)。
