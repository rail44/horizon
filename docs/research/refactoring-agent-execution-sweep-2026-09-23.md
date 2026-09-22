# agent実行経路の横断リファクタリング

基準: `0d693658`。board固有実装は除外。下記5領域の確認、必要な変更の実装・
回帰検証・main反映・再ビルド・再解析を完了条件とする。権限・承認・停止・復元、
イベント順序と保存形式は維持する。

| 領域 | 確認対象と保守上の課題 | 方針・状態 |
| --- | --- | --- |
| 実行開始 | `tools/bash`の起動引数・キュー登録・結果注記が混在 | `BashJob`と`SandboxedRun`を導入し、キュー登録を集約。main反映済み (`beaa9d6b`) |
| 承認・再試行 | `tools/approval`で開始・旧試行の終了・結果返却が重複 | 開始6箇所と拒否・fallbackの結果返却を集約。同上 |
| 非同期結果 | agentdの`completion`と`approval`で実行識別・通知・再承認を受け渡す | 再承認への変換を分離し、未完了要求の選定と旧試行への帰属を集約。main反映済み (`345662b3`) |
| ターン進行・中断 | providerの`session/state`に結果の受理・batch待ち・guard・次turn起動が集中 | `tool_results`へ分離し、batch完了とhalt再開の次turn起動を集約。実装済み |
| 保存・復元 | `mapping`の履歴補正とevent log、daemon復元の分担 | 次に確認 |

入口の解析は90ファイル・683関数で完了。複雑度・重複は読む場所の選定に用い、
呼び出し元・状態の所有者・テストで変更の要否を判断する。

実行開始・承認: ジョブへ渡す情報はセッションthread側で取得し、cwdは共有handleのまま
保持する。Git grantは実際の起動直前にも検証。キュー登録前に作業を数え、結果通知後まで
保持する境界を共通化した。権限別の検証・拒否基準、sandbox/hostの区別、retryの旧試行を
閉じる順序、結果注記の適用対象は維持。panic後の後続ジョブ・cwd更新・作業完了を
実プロセスで確認するテストを追加し、tools/policyの259テストが通過。

結果反映: `completion/retry.rs`がdomain・filesystem・mach serviceの拒否結果と
webの未実行domain要求を再承認へ変換する。通常完了時の既存occurrence保持と、再承認時の
旧試行への帰属は別の契約として維持。新occurrenceの発行は既存の`begin_reissued_approval`に
残す。4種について識別子・通知順・未要求/終了済みの除外・providerを進めないことを
追加検証し、completion/approvalの22テストが通過。同期結果の通知と非同期結果の状態選択は
既存の所有者が適切なため維持する。

ターン進行: 結果ごとのmemory反映・doom-loop判定と、batchごとのiteration判定を分離して
順序を維持。haltからのContinueも同じ次turn起動を通る。入力の優先処理、provider実行中の
中断、新規入力による旧batchの退役は条件が異なるため、それぞれの既存の所有者に残す。
provider/rigの177テストが通過し、batch待ち・遅延結果・新規入力・中断・Continueを検証。
