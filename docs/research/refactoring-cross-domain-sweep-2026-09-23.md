# 実行・操作経路のリファクタリング第四巡

基準: `ceede795`。board固有実装を除く下記5領域を、呼び出し先・関連実装・
テストまで追って確認する。必要な変更、回帰検証、main反映、再ビルド、同条件の
再解析までを完了条件とする。既存の操作・表示・権限・停止・復元の仕様は維持。

| 領域 | 確認する責務 | 状態 |
| --- | --- | --- |
| セッションの寿命管理 | shellとdaemonを通した生成・接続・復元・終了 | main反映済み (`5827fdd9`)。復元を3段階へ分離し、世代確認と端末entityの配線を集約 |
| 端末の操作と描画 | スクロール状態、履歴取得、表示更新 | main反映済み (`6473dcb5`)。scrollbackの状態遷移を分離し、履歴要求を共通化。114テスト・描画入力検証通過 |
| agentの状態と表示 | イベント反映、状態判定、transcript変換 | 表示行のprojectionをGPUI描画から分離し、burstの終了処理を統一。関連167テスト通過 |
| 個別ツール内部 | ファイル検索・読み取り、Git解析、出力再利用 | 未着手 |
| 抽出ツール | 前後比較、判断の引き継ぎ、除外範囲の精密化 | main反映済み (`16dcdd84`)。20件の回帰検証、全体production/tests解析、全体ゲート通過 |

抽出ツール: 構文単位の除外を全解析器へ適用し、理由と座標を記録する。
比較は同条件の完全なreport間に限定し、移動・分割は明示した対応表で補助関数まで含める。
過去の判断は関数と関連ファイルのhashで根拠の変化を示す。結果を隠したり、
未確認の依存先まで妥当と扱ったりはしない。通常ファイル内に残るboardの配線や
macro宣言は手作業でも境界を確認し、変更対象から外す。

同じRustソースに細かい除外を適用した開始値は318ファイル・2342関数。
除外範囲の変更による件数の減少は改善値に含めず、この条件を以降の比較基準とする。

セッション寿命: 在庫取得と接続は背景thread、候補選定とモデル反映はUI threadに維持。
両runtimeの世代は選定前・反映前の2箇所で同じ条件を使う。terminalはAttach成功分のみ、
agentは従来どおり非同期接続のhandleを採用し、完了契約を揃えない。作成・再接続・workspace
復元の端末entity生成を一箇所に集約し、終了・title・通知channelの配線を統一する。

agentdのspawn/run/resumeは、thread登録・panic時の記録・終了時の登録解除とworktree回収、
manual resumeのlifecycle lockを確認。terminaldは購読を先に登録するcreateと、存在確認付きの
attach、終了通知と登録削除の順序を確認した。復帰条件と資源の寿命が異なるため、共通の
起動/終了抽象へはまとめない。workspace/runtimeの62テストと、専用Xvfb上のUI再起動で
2タブ・2ペイン分割・同じ3端末session・復元frameを確認した。

端末: `scrollback.rs`が表示位置・先読み・到着windowの採否を所有し、sessionはその判断に
従ってIPCと再描画を行う。取得中のwheel操作は要求を重複送信せず、最新の小数行位置を
保持する。live復帰・alternate screen・resize・stale responseの既存テストを移動して確認。
描画は履歴window内とlive viewport内でcacheのindex・generationが異なり、cursor・selection・
IMEもlive専用なので各paint経路を維持する。行の文字整形と描画は既に共通化されている。
専用Xvfbでmarker・256色・truecolor・OSC 8のframe dumpも確認した（pixelの目視ではない）。

agent: event fold → LiveState → session → turn/burst → 表示行 → GPUI描画を確認。
表示行のdescriptorとそのテストを`view/projection.rs`へ移し、純粋なprojectionとlist更新・描画の
境界を明確化。burstは同じ範囲を持つ値を開閉し、assistantの終了文・TurnEnded・compactionの
3箇所で繰り返していた終了処理を統一した。receiptの範囲とkey、thinkingの表示条件は維持する。
foldとstatusはイベントの履歴と最新の稼働状態を区別する既存の責務配置を維持。deltaをmessageへ
昇格する位置、tool progressの非永続化、失敗後のidle表示、再接続世代の棄却は既存テストで確認。
