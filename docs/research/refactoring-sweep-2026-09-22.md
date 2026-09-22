# リファクタリング第一巡

基準: `86601ea1`。通常実装339ファイル・2,548関数を解析し、下表の入口・
呼び出し先・関連テストから責務、同じ役割の実装、読みやすさを確認した。
全関数の精査ではなく、全領域の境界と代表例を確認する第一巡である。
解析値は選定の入口とし、変更の波及・重複修正・状態遷移の追いにくさを優先した。

## 今回の対象と完了条件

| 対象 | 保守上の負担 → 分離する責務 | 維持する契約 | 状態 |
| --- | --- | --- | --- |
| `src/control_plane.rs` | 引数検証と画面操作が同じ分岐内に混在 → 型付き要求の解析と実行 | 検証順・エラー文・既定値・遅延応答 | main反映済み |
| `horizon-cli/src/board.rs` | オプション追加が多数の引数と同型のエラー処理に波及 → オプション解析の所有者 | フラグの解釈・出力・終了コード | main反映済み |
| `horizon-workspace/src/persistence.rs` | セッション、レイアウト、参照整合性が多重ループに混在 → 検証単位 | 保存形式・最初のエラー・空workspace | main反映済み |
| `src/workspace/session_lifecycle.rs` | 復元の非同期処理に在庫照合が埋没 → 復元候補の選定 | 両runtimeの世代確認・ID競合除外・メタデータ | main反映済み |
| `horizon-config/src/grants.rs` | 権限種別の検証が一関数、同じroot選択が4箇所 → 種別検証と共通の集約 | 拒否基準・警告順・重複排除と入力順 | main反映済み |
| `horizon-agent/.../session/state.rs` | 制御要求の優先処理と新規入力開始が実行ループ内に埋没 → 入力の準備と開始 | cancel/shutdown優先・入力受理・MoAとイベント順 | 実装済み |
| `horizon-agentd/src/session/resume.rs` | 復帰可否・環境復元・中断補正・thread起動が混在 → 復帰準備 | 既存の拒否条件・権限・turn履歴・追記順 | 未着手 |

上記を実装・検証してmainへ反映し、再解析の結果と残件を追記して完了とする。
仕様や責務の所有者を変える判断が必要になった場合は、その対象について相談する。

## 領域ごとの確認

`crates/` 以下はcrate名、`src/` 以下は画面・接続領域。各行は確認した入口と判断を示す。

| 領域 | 確認した責務の境界・代表例 | 判断 |
| --- | --- | --- |
| horizon-agent | providerの`state::run`、`frame/status`、tool schema | 入力準備を対象化。schema列挙と状態の投影は維持 |
| horizon-agentd | `session/{resume,spawn,setup,run}` | 永続履歴からの復帰準備を対象化。実行threadの所有者は維持 |
| horizon-board | `store/query`、`model`、logd client | 読み取り投影と書き込み先の分離を維持。階層走査は前段で対応済み |
| horizon-cli | `board::{run_board,dispatch}`、通常CLIのparse | boardのオプション解析を対象化。コマンド別処理の列挙は維持 |
| horizon-config | `grants::{resolve,*_for_project}`、設定loader | 権限種別の検証と同一rootの集約を対象化 |
| horizon-control | `host/listener`、`wire::parse_line` | 接続処理・版検証・payload decodeの順序が明確。維持 |
| horizon-daemon-testkit | `process::{DaemonProcess,wait_for_exit}`、hub接続 | 子プロセス所有権と接続準備を分離済み。維持 |
| horizon-logd | `hub::ingest`、writer、subscription | ロック内の判断と追記、成功後の通知が分離済み。維持 |
| horizon-sandbox | `helper::resolve`、能力構築 | 探索順とOS境界を維持。古いbuild構成向けfallbackは別途必要性を検証 |
| horizon-sandbox-runtime | `linux/network`、open時の能力照合 | syscall別の検証と許可判定を維持。単純な分岐数では共通化しない |
| horizon-sandbox-proxy | `handler::{target_host,forbidden}`、allowlist | 宛先取得・許可判定・TLS非介入が分離。維持 |
| horizon-terminal-core | `session_loop`、kitty `legacy_bytes` | プロトコルの対応表を維持。frame送信制御の引数集中は次段階 |
| horizon-terminald | `terminal::{spawn_terminal,run_writer}` | PTYの所有権とcoreへの入力転送を維持 |
| horizon-wire | `daemon::{bind_listener,run}`、spawn | 共通transportとruntime別終了処理の分離を維持 |
| horizon-workspace | `persistence::validate`、command catalog | 保存データの検証を対象化。コマンドの宣言的な列挙は維持 |
| src直下 | `control_plane`、palette delegate | 外部要求の解析を対象化。paletteは既存のcommand modelに委譲 |
| src/agent | sessionのLiveState/RuntimeLink、transcript描画 | 接続・投影・描画の所有者を維持。text/markdown行の小さな重複は低優先 |
| src/board_pane | detail、execute、model、command button | store実行とnative/wasm分岐は集約済み。detail内の表示用選択処理は次段階 |
| src/preview | guest、watch、registry | guest/host、購読寿命、artifact監視が分離。維持 |
| src/runtime | request、link、agent/terminal復旧 | 前段の共通化を維持。異なるdrain・復旧予算を持つループは個別に保つ |
| src/terminal | sessionのRowGenerations、glyph geometry | 行世代と描画を分離済み。文字別の幾何・プロトコル仕様を維持 |
| src/theme | `scheme_from`、色変換・contrast | seedから各roleの色を導出するまとまりを維持 |
| src/theme_settings | seed、save、picker | 編集値と保存、他のTOML項目を保つ処理が分離。維持 |
| src/workspace | command実行、`spawn_workspace_restore` | 復元候補の照合を対象化。command routingとpane/session寿命は維持 |
| preview-plugin | `register_plugin!`のみ | 実装はsrc/previewへ委譲。関数数0でも確認範囲に含めた |
| scripts/refactor-audit | sources、tooling、audit、results、verify | 解析・外部ツール実行・報告を分離済み。失敗時の不完全結果を成功扱いしない |

## 結果

対象確定時点。各対象の完了時に上表と検証結果を更新する。
