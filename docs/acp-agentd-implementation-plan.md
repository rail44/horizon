# ACP 化の実装計画

Status: plan 2026-09-27。設計は `docs/acp-agentd-design.md`、決定は board #63
（本文と 2026-09-27 のコメント）。実装は未着手。

## 前提となる調査結果

crate は `agent-client-protocol` 2.2.0 と `agent-client-protocol-schema`
1.9.1（SDK が `=1.9.1` で固定）。v2 は両方の `unstable_protocol_v2` feature の
下にあり、schema 側は "may change at any time" と書いている。**両 crate を
exact pin し、同時に上げる。** 手元の rustc 1.96 で v2 feature 付きの型検査は
通っている（2026-09-27）。

crate の実行モデルで守ること。

- tokio 依存なし。`ByteStreams::new(write, read)` は `futures::io` の trait を
  取るので、`tokio::net::UnixStream` は `tokio_util::compat` で包む。LocalSet
  不要。agentd の multi-thread runtime とシェルの current-thread runtime の
  どちらでも動く。
- 一接続につき dispatch loop は一本で、ハンドラは一つずつ完了を待つ。
  **ハンドラの中で `send_request(..).block_task()` を待つと詰まる。**
  client への要求（`session/request_permission`、`_horizon/host_tool`）は
  `connection.spawn` か `tokio::spawn` したタスクから送る。
- `connection.spawn` したタスクが `Err` を返すと**接続全体が落ちる**。
  セッション単位の作業は自分で `Result` を畳み、`Err` を返さない。
- `connection.spawn` の仕事は全部一つの poll に乗る。重い処理は tokio 側へ。
- 一接続につき `initialize` は一度。再接続は新しい `initialize` と
  `session/resume`{replay_from: Start} で組み直す。v1 と v2 の翻訳は無い。
- `AcpAgent`（stdio 子プロセス起動）に cwd と env_clear が無い。外部
  エージェントは tokio で自分で spawn し、パイプを `ByteStreams` で包む。
- 拡張メソッドは derive macro（`JsonRpcRequest` / `JsonRpcNotification`、
  `#[request(method = "_horizon/...")]`）で型付きに定義でき、標準の型と同じ
  経路で送受信できる。`_meta` は `serde_json::Map`。v2 では capability の
  `_meta` を直接書く（`MetaCapability` は v1 専用）。

## 構成

### 新 crate `crates/horizon-acp`

シェルと agentd の両方が依存する拡張語彙。

- `_horizon/*` の要求と通知の型（下表）。
- `_meta.horizon` に載せる構造体（`SessionNewMeta`、`SessionInfoMeta`、
  `ToolCallMeta`、`ApprovalMeta`、`PermissionResponseMeta`、`MessageMeta`）と、
  それを `Meta` に出し入れするヘルパ。
- `HORIZON_ACP_EXT_VERSION`。`initialize` の `_meta.horizon.ext_version` で
  両端が突き合わせる lockstep。今の `AGENT_PROTOCOL_VERSION` の後継。
- スキーマ artifact `crates/horizon-acp/schema/acp-ext-wire.json`。schemars で
  拡張型を出力し、`HORIZON_BLESS_WIRE_SCHEMA=1` で bless する今の作法を
  引き継ぐ。

拡張の一覧。

| 方向 | メソッド | 今の対応物 |
|---|---|---|
| client→agent 要求 | `_horizon/continue_turn` | `Command::ContinueTurn` |
| client→agent 要求 | `_horizon/list_providers` | `list_providers` |
| client→agent 要求 | `_horizon/list_provider_models` | `list_provider_models`（provider の `GET /models` を生で引く挙動を保つ） |
| client→agent 要求 | `_horizon/watch_board` | `watch_board` |
| client→agent 要求 | `_horizon/ensure_board_organizer` | `ensure_board_organizer` |
| client→agent 要求 | `_horizon/reload_provider_config` | `reload_provider_config` |
| client→agent 要求 | `_horizon/drain` | `drain` |
| agent→client 要求 | `_horizon/host_tool` | `HostToolRequest` / `HostToolResponse` |
| agent→client 通知 | `_horizon/task_progress` | `AgentWireEvent::TaskProgress` |
| agent→client 通知 | `_horizon/tool_call_progress` | `AgentWireEvent::ToolCallProgress` |
| agent→client 通知 | `_horizon/memory` | `MemoryDigest` / `MemoryCheckpointMissed` |
| agent→client 通知 | `_horizon/session_event` | `SessionResumed`、`ProviderRateLimited`、`HistoryCleared`、`skipped_lines` |
| agent→client 通知 | `_horizon/provider_request` | `ProviderRequestSent` / `FirstToken` / `Finished`（turn receipt 用） |

`_meta.horizon` に載せるもの。

| 場所 | 内容 |
|---|---|
| `initialize` 両方向 | `ext_version`、`binary_id` |
| `session/new` 要求 | `session_id`（シェル発行）、`provider_id`、`role_id`、`isolate`、`spawn_source_session_id` |
| `session/new` 応答、`SessionInfo` | `workspace_root`、`parent_session_id`、`role_id`、`provider_id` |
| `tool_call_update` | `occurrence_id`、`denied`、`auto_approved`、`policy_tier`、`tool_id` |
| `request_permission` 要求 | `call_id`、`occurrence_id`、`ApprovalKind` の構造化 payload |
| `request_permission` 応答 | deny の `reason` |
| `user_message` / `agent_message` | 役割 `TaskNotification` / `AutoContinue` |
| `state_update` idle | `stop_reason` の独自値 `_horizon/failed`、`_horizon/doom_loop` |

線を越えないもの（agentd 内に留まる）: Input routing 系 6 種、Environment 系
3 種、`MoaPassStarted`、`MemorySeeded`、`ApprovalResolved`、
`ContinueTurnRequested`、`ProviderRequestUsage` は `usage_update` で代替。

### agentd 側

- `hub.rs` と `SessionHub` の remoc 実装を、**接続ごとに一つの
  `Agent.v2()...connect_to(ByteStreams)`** に置き換える。`AgentdState` は
  `Arc` で共有。`horizon_wire::daemon::run` の accept loop は残し、
  `serve_connection` の remoc 版の隣に「生の `UnixStream` を閉包に渡す」
  版を足す。
- ハンドラ:
  - `initialize`: `_meta.horizon.ext_version` を照合し、不一致は JSON-RPC
    error で拒否（今の `HandshakeRejected` 相当）。応答に
    `capabilities.session = {}` と `_meta.horizon`。
  - `session/new`: `_meta.horizon.session_id` を採用し `spawn_session_thread`。
    応答の `config_options` に category `model` の select（現在の
    provider・model）。
  - `session/list`、`session/resume`（`replay_from: Start` なら
    `live_state.events()` を下の写像で流してから応答）、`session/close`
    （`Command::Shutdown`）、`session/prompt`（`Command::UserMessage`、
    応答は messageId。キューは今のまま agentd 側）、`session/cancel`、
    `session/set_config_option`（`Command::SetSessionModel`）。
  - `_horizon/*` の要求は今の hub メソッドの本体をそのまま呼ぶ。
- **イベントの sink の差し替え。** `session/events.rs::send_session_event`
  と `agent_subscribers` の型を `AgentWireEvent` から「接続への送信ハンドル」
  に変え、`contract::Event` から v2 `SessionUpdate` と `_horizon/*` 通知への
  写像を接続側に置く。写像はセッション単位の状態を持つ:
  - messageId は再生でも同じ値になるよう `turn_id` と turn 内の序数から
    決める。
  - `StateChanged` と `TurnEnded` から `state_update`（running / idle+
    stop_reason / requires_action）を作る。
  - `ToolCallRequested` / `Started` / `Finished` は一つの `tool_call_update`
    系列（初出で作成）。kind はカタログから（`fs.read`→read、`fs.edit`→edit、
    `bash`→execute、`web_fetch`→fetch、`fs.grep`/`fs.glob`→search）。
    `fs.edit` の diff content は `transcript/diff.rs` の再構成を agentd 側で
    行う。
- **承認の往復。** `ApprovalRequested` はイベントではなく agentd 発の
  `session/request_permission` 要求になる。承認待ちごとに spawn したタスクが
  要求を送って応答を待ち、`Approve` / `Deny{reason}` を session loop に渡す。
  選択肢は `ApprovalKind` から組む（Standard は allow_once / reject_once、
  DomainGrant は「ドメインを許可して再実行」の allow_once など）。
  **再接続時は、まだ保留中の承認を `session/resume` 後に再度要求する**
  （前の接続とともに要求が死ぬため）。今の無人時拒否はそのまま。
- `agent_subscribers` の「一セッションに一購読、新しい attach が置き換える」
  意味は保つ。

### シェル側

- `src/runtime/agent.rs` を ACP v2 client に書き換える。std スレッド上の
  current-thread runtime は今のまま。`Op` は v2 要求と `_horizon/*` 要求に
  なる。ハンドラは `UpdateSessionNotification` と `_horizon/*` 通知を
  session_id でセッションへ配り、`RequestPermissionRequest` は `Responder`
  をセッションに預ける（ユーザーの操作か CLI の approve/deny で応答、
  cancel 時は `Cancelled`）。`_horizon/host_tool` は今の host tool 経路へ。
- `common.rs` の remoc 形の部分（`connect_hub`、`classify_connect_error`、
  `StreamEnd`）は terminald 用に残し、agent 側は ACP 用の接続と失敗分類を
  `agent.rs` に持つ。version 不一致の判定は `initialize` の error で行い、
  `_horizon/drain` で古い agentd を落として respawn する今の回復を保つ。
- **モデルの書き直し。** `LiveState` と `frame::fold` は `contract::Event` の
  上に書かれている。シェルは新たに `src/agent/model/` に v2 `SessionUpdate`
  と `_horizon/*` 通知を畳む fold を持ち、出力型は今の `AgentFrame` /
  `AgentFrameItem` / `SessionStatus` の形を保って `src/agent/view` と
  `src/agent/turns` の変更を最小にする。`crates/horizon-agent/src/frame` と
  `live.rs` は agentd 側（再生・永続化）で使われ続ける。シェルの
  `horizon-agent` 依存は `transcript` の描画ヘルパのために当面残す。
- `AgentSession` が送るものは `Prompt`、`Cancel`、`Approve{call_id}`、
  `Deny{call_id, reason}`、`ContinueTurn`、`Close`、`SetModel` の七つ。
  `control_plane.rs` の語彙はこの七つに写す。
- `session_lifecycle.rs` / `commands.rs` / `restore.rs` / `modals.rs` の
  `AgentdHandle` 呼び出しは名前を保ち、中身だけ ACP 要求に変える。

### 外部エージェント（後段）

- `src/runtime/external_acp.rs`。tokio で子プロセスを spawn（cwd を渡す）、
  `Client.builder()` の v1、`schema::v1` のハンドラを同じ `src/agent/model/`
  の入力に正規化する層を置く。第一弾は claude-agent-acp のみ。
- セッション管理はしない。ペインを閉じれば接続が切れて子プロセスが終わる。
  workspace 復元の対象外。
- 起動コマンドをどこから取るか（config に新セクションを足すか）は
  **未決**。config の面は 2026-07-18 の narrowing wave の対象なので、
  この段に入る前に相談する。

## 試験面

| 今 | 後 |
|---|---|
| `crates/horizon-agentd/tests/e2e.rs`（23 件） | crate の `Client.v2()` で socket に繋ぐ形に書き換え。項目は保つ |
| `crates/horizon-daemon-testkit/src/hub.rs` | ACP の接続・drain ヘルパに置き換え |
| `crates/horizon-terminald/tests/e2e.rs` の agentd drain | `_horizon/drain` |
| `src/runtime/tests.rs` の `FakeSessionHub`（27 件） | crate の `Channel::duplex()` 上で偽の v2 agent を動かす |
| `crates/horizon-agent/tests/wire_schema.rs` | `crates/horizon-acp/tests/wire_schema.rs` |
| `crates/horizon-agent/tests/skew.rs`（Postbag） | 削除 |
| `scripts/check-wire-schema.sh` | `agent-wire.json` の削除を RESHAPE にしない移行の腕を足す（`session-wire.json` の腕と同じ形） |

`.config/nextest.toml` の sandboxed profile は、socket を張る試験の名前が
変わる分だけ追随する。

## 段取り

一つのブランチで A から D を積み、切り替えは一括。E は別ブランチ。

| 段 | 内容 | 主な対象 |
|---|---|---|
| A | `horizon-acp` crate、拡張型、`_meta` 構造体、版定数、schema artifact、checker の移行の腕 | 新 crate、`scripts/check-wire-schema.sh` |
| B | agentd を v2 agent に。sink の差し替え、写像、承認の往復、resume の再生、`_horizon/*` | `crates/horizon-agentd/src/{hub,main}.rs`、`session/{connection,events,approval}.rs`、`horizon-wire/src/daemon.rs` |
| C | シェルを v2 client に。`src/agent/model/` の fold、runtime、control plane の写し | `src/runtime/{agent,routing,mod}.rs`、`src/agent/{session,model}`、`src/workspace/*` |
| D | 試験と script の書き換え、`horizon-agent`/agentd/root からの remoc 除去、`wire.rs`/`wire/hub.rs`/`agent-wire.json` の削除、AGENTS.md の protocol bump の記述更新 | 上の試験面の表 |
| E | 外部エージェント（v1 client、spawn、正規化、ペイン配線）。config の相談を先に | `src/runtime/external_acp.rs` |

規模の目安（行数は 2026-09-27 の `wc -l`）: シェル runtime 側で書き換え
約 1,700 行と試験 1,700 行、agentd 側で hub 450 と connection 889 と
events 78、e2e 2,191 行。

## 実装中に決めてよいこと

- messageId の決め方（`turn_id` + 序数を既定）。
- `_horizon/session_event` を一本にまとめるか、種類ごとに分けるか。
- v1 の外部エージェントからの `SessionNotification` を `src/agent/model/`
  へ正規化する層の置き場（`horizon-acp` かシェル内か）。

## 関連

- `docs/acp-agentd-design.md`、`docs/agent-runtime-split-design.md`、
  `docs/remoc-adoption-design.md`（skew checker の由来）。
- crate の参照箇所: `examples/simple_agent_v2.rs`、
  `examples/v2_one_shot_client.rs`、`tests/application_dispatch_v2.rs`
  （`Send` なハンドラから `!Send` な前景へ mpsc で渡す形。GPUI 側の型）。
