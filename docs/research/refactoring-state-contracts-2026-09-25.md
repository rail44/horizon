# 状態と実行契約の整理（2026-09-25）

対象は main `55febd67` からの non-board コード。当初の4件に追加調査の4件を加えた。
固定の種類はenum、資源の有無はOptionと所有型で表す。新しいtraitは追加せず、
既存の `HostTools` と provider の `CompletionClient` を引き続き境界として使う。

| 対象 | 変更と得られる効果 |
| --- | --- |
| [同期ツール](../../crates/horizon-agent/src/tools/synchronous.rs) | 13種類のenumがID・承認区分・実行先を持ち、カタログもそこからIDと区分を取得する。選択後の実行は必ず結果を返す。承認済みの外部読取と通常読取を区別し、書込制約は維持する。 |
| [AI応答の終了理由](../../crates/horizon-agent/src/providers/rig/completion/outcome.rs) | 成功・取消し・失敗・切詰めをenum化。未完成ツールと出力上限の同時発生、取消し時の発行済みツール・usageを保持する。 |
| [workspace復元](../../src/workspace/recovery.rs) | 復元中・復元失敗・利用可能・保存ファイル保護の4状態から、操作と保存の可否を導く。読み取れない／未知形式のファイルを上書きしない。 |
| [preview読込](../../src/preview/pane.rs) | 読込タスク、成功したguestとroot、名前取得タスクを各状態が所有する。状態の置換でまとめて解放し、古い応答が新しい状態を書き換えられなくする。watchは対象パスの寿命に残す。 |
| [記憶更新の確認](../../crates/horizon-agent/src/providers/rig/session/memory.rs) | 文書と確認状態をまとめ、未確認・通知済み・更新済みをenum化。1回の対話につき催促は最大1回とし、次の対話でリセットする。 |
| [workspace操作モード](../../crates/horizon-workspace/src/mode.rs) | pane入力／workspace操作をenum化し、操作モードだけがカーソルを持つ。空workspaceは常にコマンドを受け付ける仕様を保持する。 |
| [端末の描画予約](../../crates/horizon-terminal-core/src/session_loop/frames.rs) | dirtyとtimer有効フラグをなくし、予約タイマーのOptionだけで未送信フレームの有無を表す。即時送信・連続更新の集約・再予約を維持する。 |
| [ツール結果の参照](../../crates/horizon-agent/src/frame/tool_calls.rs) | 結果と表示元の位置を1つのOptionにまとめ、片方だけ存在する状態をなくす。実行ごとの識別と承認表示は維持する。 |

同期ツールでは実際の不具合も修正した。workspace rootのない `knowledge.read/write` は、
従来Errorだけを返してproviderへの応答が欠落した。回帰テストが修正前に失敗することを確認し、
修正後は同じ実行IDの失敗結果をちょうど1回返す。ホスト側の実行先がない場合も同様に完了する。
非同期のbash/web/taskとboardの実行方式は統合していない。

## 追加調査と維持したもの

non-boardの構造検索は、複数のbool、複数のOption、連動する代入を入口とした。
「boolが2個以上、またはbool/Optionが合計3個以上」の構造体は50件あり、呼出側と遷移を追って選定した。
この検索だけではworkspace操作モードの1 bool＋1 Optionは拾えないため、既存候補の責務追跡も併用した。
全型の設計を精査し尽くしたという意味ではない。全リポジトリの一般的な検出器・自動修正規則にはしない。

- 端末の装飾・modifier、sandboxの各許可、role設定は独立した値。単一enumにしない。
- `ToolCallView`、commandの有効条件などの表示・問合せ用の値は元状態から組み立てる。今回整理した可変の状態源と同列に扱わない。
- 子taskの失敗と上限到達、検索の走査打切りと出力打切りは同時に起こりうる。排他的なenumにしない。
- provider処理とdaemon接続には既存共通部がある。さらにtraitで包む利益は確認できず、異なる起動・切断契約を維持する。
- 永続イベント／一時通知の `ProviderEvent`、公開される結果の `is_error/denied`、modalとsubscriptionの所有権は別の整理候補として残る。これらは値の排他性・外部入力やcallbackの取扱いまで未評価であり、今回の8件と同じ確度での境界変更を採用していない。互換維持を必須とする判断ではない。

board固有の仕様は対象外。共有workspace状態を参照するboard用callbackのガード3か所だけを
同等の状態判定へ置換した。通信schema、保存形式、利用者設定は変更していない。

## 検証

- non-board再走査: 355ファイル・2,411関数、boardの33構文範囲を除外。
  生成物は `target/refactor-audit/55febd6704b3-20260924T181952Z-3636f3b8/`。
  構造体の入口一覧は同 `55febd6704b3-20260924T173938Z-2044975b/state-signals.json`。
  行数・複雑度の低下を採用理由にはしていない。
- 通常環境のworkspaceテスト: 2,252成功・15skip。実ソケット・daemonを使う境界テストも通過。
- sandboxed workspaceテスト: 2,101成功・88skip。新規の意味のある確認は結果欠落、取消し時の情報保持、復元の保存禁止、記憶更新の催促上限、preview資源解放。
- 実WASMのpreview描画・theme反映・reloadと、paneの非同期読込・reload／retarget資源解放: 2テスト成功。
- 隔離した実UIとdaemon: UI再起動後も2タブ・2分割・同じ3端末sessionを復元。
- workspaceビルド、fmt、Clippy、wire schema、WASM targetのゲートを通過。
