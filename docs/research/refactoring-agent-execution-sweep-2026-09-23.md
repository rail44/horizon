# agent実行経路の横断リファクタリング

2026-09-23完了。基準は`0d693658`。board固有実装を除く5領域を確認し、
以下をmainへ反映した。権限・承認・停止・復元、イベント順序と保存形式は維持。

| 領域 | 変更と結果 | コミット |
| --- | --- | --- |
| 実行開始 | `BashJob`がthreadへ渡す情報とキュー登録、`SandboxedRun`が実行時の権限を所有。共有cwd、起動直前のGit grant検証、結果通知までの作業計数を維持 | `beaa9d6b` |
| 承認・再試行 | 開始6箇所と拒否・fallbackの結果返却を集約。権限別の検証・変更は各resolverに残す | `beaa9d6b` |
| 非同期結果 | `completion/retry`へ再承認への変換を分離。未完了要求の選定と旧試行への結果帰属を集約し、新承認の識別子発行は既存の所有者に残す | `345662b3` |
| ターン進行・中断 | `session/tool_results`へ結果受理・batch待ち・guard判定を分離。batch完了とhalt再開で次turn起動を共通化 | `11819a25` |
| 保存・復元 | `mapping/replay`へ履歴補正を分離。全履歴の応答・tool名の索引と、順序に沿う補正状態を明示 | `554b54ef` |

維持した境界:

- 通常完了は既存の実行識別子を保持。再承認は旧結果と新承認を別の試行へ帰属させる。同期結果の通知と非同期結果の状態選択も各所有者に残した。
- memory反映・doom-loop判定は結果ごと、iteration判定はbatchごと。入力の優先処理、新規入力による旧batchの退役、provider実行中の中断はそれぞれ条件が異なる。
- `Appender/TurnTracker`はturn識別、単一writerは連番・JSONL保存・DuckDB投影、daemonは再起動時の中断確定を担当。投影の遅延中にも履歴を読むため、provider側の補正も必要。隣接assistantの結合、未要求resultの除外、未応答callのcancel補完を維持した。

検証: 全workspaceビルド、fmt、Clippy、**2,149テスト成功・14 skip**、wire schema、
preview WASM checkが通過。実デーモンの中断・再起動・履歴復元を含む通常profileを
sandbox外で実行し、各反映後にmainを再ビルドした。追加した回帰検証は以下。

- ジョブのpanic後も次のジョブが動き、共有cwdと作業完了を正しく引き継ぐ。
- 4種の再承認で実行識別子・通知順・未要求/終了済みの除外を維持し、providerを進めない。
- 履歴のmessage ID・内容順・補完するtool名、要求より前に届いたresult、再補正の不変性。

同じ範囲をRCA・jscpd・ast-grepで再解析し、94ファイル・700関数をエラーなく処理
（開始時90ファイル・683関数）。代表的な認知的複雑度は次のとおり。

| 対象 | 前 | 後 |
| --- | ---: | --- |
| bash sandbox起動 | 33 | 起動0、切り出したjob処理の最大6 |
| provider command loop | 36 | loop 9、切り出した結果処理の最大10 |
| 履歴pairing補正 | 47 | 入口1、切り出した補正処理の最大7 |

重複は52組→42組、重なりをまとめた群は26→24。残る権限別の再試行はgrantと
結果注記が異なるため維持。event列挙やtool定義の類似も、そのまま共通責務とは扱わない。
数値には入れ子のclosureを含み、macro展開は含まれない。改善の判断は呼び出し元・
責務・契約と上記テストに基づく。

daemon起動・手動resumeのライフサイクル制御や、Git字句解析・出力再利用・個別fs toolの
内部判定は独立した責務として維持し、次回の詳細検討候補に残す。

解析範囲は`providers/rig`・`tools`・`persistence`（`horizon-agent/src`配下）と
`horizon-agentd/src/session`。既定設定に`**/board.rs`・`**/board/**`の除外を追加。
元データは作業worktreeの`target/refactor-audit/`に保存。最終commitのcleanな状態でも
再実行し、解析対象のSHA-256とmainの一致を確認する。
