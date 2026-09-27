# ACP 化の実装計画

Status: 段 A〜D implemented 2026-09-27（統合ブランチ上。main へのマージは
未了）。段 E は未着手（config の相談が先）。設計は
`docs/acp-agentd-design.md`、決定は board #63（本文と 2026-09-27 のコメント）。
実装が計画から外れた点は `docs/acp-agentd-design.md` の「実装での確定事項」
に記録し、下の表はそれに合わせてある。以下の「今の」は計画時点（main
`c96f685a`）の状態を指す。

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

## 今の継ぎ目（main `c96f685a`）

置き換える対象は次の三箇所に閉じている。session loop、`frame`、
`transcript`、persistence は wire に依存していない。

- **agentd**: `hub.rs`（384、`SessionHub` の remoc 実装）、`hub/attachment.rs`
  （68、attachment の pump）、`session/connection.rs`（774、attach と
  hub メソッドの本体）、`session/events.rs`（123、publish 境界）、
  `session/attachment.rs`（216、`Streams` / `SessionStream` / `Bootstrap` /
  `AttachmentLease` / `capture`）。
- **`horizon-agent`**: `wire.rs`（333、`AgentWireEvent` 10 変種）、
  `wire/hub.rs`（418、`SessionHub` 11 メソッド、`AGENT_PROTOCOL_VERSION` 28）。
- **シェル**: `src/runtime/agent.rs`（672）、`runtime/attachment.rs`（161、
  `AttachmentState` と `AgentUpdate`）、`runtime/routing.rs`（382）、
  `runtime/mod.rs`（704、`AgentdHandle` と `AgentSessionHandle`）、
  `src/agent/session.rs`（552）。

attach の仕組みは atomic になっている。`connection.attach(id)` が
`Bootstrap` を返し、`capture()` が session thread 上で history と
メタデータ（model、selection、workspace_root、進行中の tool call preview、
実行中 task）を一度に取り、pump が `ReplayStarted`、history、メタデータ、
`ReplayComplete` の順で流してから live に切り替える。購読は一セッションに
一つで、新しい attach は前の lease を `Replaced` で終え、client の command は
現行 lease からしか通らない。live が詰まれば `Lagged` で購読が落ちる。

承認は `ToolCallIdentity { call_id, occurrence_id }` で鍵付けされる。再試行は
同じ call_id の下に新しい occurrence を作り、古い試行は
`ToolOutcome::Superseded { retry_occurrence_id }` で閉じる。

## 構成

### 新 crate `crates/horizon-acp`

シェルと agentd の両方が依存する拡張語彙。`horizon-agent` には依存せず、
`contract` の型を写した plain な serde 構造体を持つ（変換は agentd 側）。

- `_horizon/*` の要求と通知の型（下表）。
- `_meta.horizon` に載せる構造体（`InitializeMeta`、`SessionNewMeta`、
  `SessionInfoMeta`、`ToolCallMeta`、`ApprovalMeta`、`PermissionResponseMeta`、
  `MessageMeta`）と、それを `Meta` に出し入れするヘルパ。
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
| agent→client 通知 | `_horizon/tool_call_progress` | `ToolCallProgress` と `ToolCallProgressClosed` |
| agent→client 通知 | `_horizon/memory` | `MemoryDigest` / `MemoryCheckpointMissed` |
| agent→client 通知 | `_horizon/session_event` | `SessionResumed`、`ProviderRateLimited`、`HistoryCleared`、`Error`、`Exited`、`SkippedLines`（旧 `HubHello::skipped_lines`）、`AttachmentClosed{Replaced/Lagged/Detached/SessionEnded}` |
| agent→client 通知 | `_horizon/provider_request` | `ProviderRequestSent` / `FirstToken` / `Finished`（turn receipt 用） |

`ReplayStarted` / `ReplayComplete` は `session/resume` の要求と応答に対応する
ので拡張は要らない。

`_meta.horizon` に載せるもの。

| 場所 | 内容 |
|---|---|
| `initialize` 両方向 | `ext_version`、`binary_id` |
| `session/new` 要求 | `session_id`（シェル発行）、`provider_id`、`role_id`、`isolate`、`spawn_source_session_id` |
| `session/new` 応答、`SessionInfo` | `workspace_root`、`parent_session_id`、`role_id`、`provider_id` |
| `tool_call_update` | `call_id`（toolCallId は occurrence_id）、`tool_id`、`outcome`（Succeeded / Failed / Denied / Cancelled / Superseded{retry_occurrence_id}）、`auto_approved`、`policy_tier`、`human_decision`（`ApprovalResolved` を写す。再生でも人の承認の印が残る） |
| `request_permission` 要求 | `identity`（call_id、occurrence_id）、`ApprovalKind` の構造化 payload |
| `request_permission` 応答 | deny の `reason` |
| `user_message` / `agent_message` | 役割 `TaskNotification` / `AutoContinue` |
| `state_update` idle | `stop_reason` の独自値 `_horizon/failed`、`_horizon/doom_loop` |

線を越えないもの（agentd 内に留まる）: Input routing 系 6 種、Environment 系
3 種、`MoaPassStarted`、`MemorySeeded`、
`ContinueTurnRequested`、`ConversationRecorded`、`ProviderRequestUsage`
（ペインは token 数を描いていない。`usage_update` は当面送らない）。
`WorkspaceRootResolved` は `session_info_update` の `_meta.horizon`
（`SessionInfoMeta`）で運ぶ。`ToolCallResult`（host tool の結果）は
`_horizon/host_tool` の応答そのもの。

### agentd 側

- `hub.rs` と `SessionHub` の remoc 実装を、**接続ごとに一つの
  `Agent.v2()...connect_to(ByteStreams)`** に置き換える。`AgentdState` は
  `Arc` で共有。`horizon_wire::daemon::run` の accept loop は残し、
  `serve_connection` の remoc 版の隣に「生の `UnixStream` を閉包に渡す」
  版を足す。
- ハンドラ:
  - `initialize`: v2 が取れれば常に成功させ、応答の `_meta.horizon` に
    daemon 側の `ext_version` と `binary_id` を載せる。client の
    `ext_version` が違えば接続を不一致と記録し、以後 `_horizon/drain` 以外の
    要求を `horizon ext version mismatch` で始まる JSON-RPC error で拒否する。
    不一致の検知と drain・respawn は client 側の仕事（SDK の v2 guard が
    `initialize` 成功前の要求を通さないため、`initialize` 自体を拒否すると
    古い daemon を drain できない）。応答に `capabilities.session = {}`。
  - `session/new`: `_meta.horizon.session_id` を採用し `handle_session_new`。
    応答の `config_options` に category `model` の select（現在の
    provider・model）。
  - `session/resume`: `connection.attach(id)` で `Bootstrap` を得て、
    `capture()` の history とメタデータを下の写像で流してから
    `ResumeSessionResponse` を返す。メタデータのうち model/selection は
    `config_option_update`、workspace_root は `_meta`、preview と task は
    `_horizon/tool_call_progress` と `_horizon/task_progress`。
  - `session/list`、`session/close`（`Command::Shutdown`）、`session/prompt`
    （`Command::UserMessage`、応答は messageId。キューは今のまま agentd
    側）、`session/cancel`、`session/set_config_option`
    （`set_session_model` の本体）。
  - `_horizon/*` の要求は今の hub メソッドの本体をそのまま呼ぶ。
- **pump の差し替え。** `hub/attachment.rs::start(Bootstrap)` が remoc の
  `AgentAttachment` チャネルに流している部分を、接続へ `session/update` と
  `_horizon/*` 通知を送る形に変える。`Streams` / `SessionStream` /
  `AttachmentLease` の意味（一購読、`Replaced`、`Lagged`、command は現行
  lease のみ）はそのまま使い、`AttachmentEnd` は `_horizon/session_event` で
  通知する。`Lagged` を受けたシェルは `session/resume` を出し直す。
- **`contract::Event` から v2 更新への写像**は接続側に置き、セッション単位の
  状態を持つ:
  - messageId は再生でも同じ値になるよう、セッションごとの連番から
    `msg-{n}` / `thought-{n}` を作る（連番は `MessageCommitted` と
    `ToolCallRequested` で進む）。
  - `StateChanged` と `TurnEnded` から `state_update`（running / idle+
    stop_reason / requires_action）を作る。
  - `ToolCallRequested` / `Started` / `Finished` は toolCallId =
    occurrence_id の `tool_call_update` 系列（初出で作成）。`ToolOutcome` は
    status（completed / failed / cancelled）と `_meta.horizon.outcome` に
    分ける。kind はカタログから（`fs.read`→read、`fs.edit`→edit、
    `bash`→execute、`web_fetch`→fetch、`fs.grep`/`fs.glob`→search）。
    `fs.edit` の diff content は `transcript/diff.rs` の再構成を agentd 側で
    行う。
- **承認の往復。** `ApprovalRequested` はイベントではなく agentd 発の
  `session/request_permission` 要求になる。承認待ちごとに spawn したタスクが
  要求を送って応答を待ち、`ApproveToolCall{identity}` /
  `DenyToolCall{identity, reason}` を `dispatch_inbound_command` に渡す。
  選択肢は `ApprovalKind` によらず `approve`（allow_once）と
  `deny`（reject_once）の二つ。種類ごとの内容は `_meta.horizon` の
  `ApprovalMeta` で運ぶ。
  **再接続時は、まだ保留中の承認を `session/resume` の応答後に再度要求する**
  （前の接続とともに要求が死ぬため）。今の無人時拒否はそのまま。

### シェル側

- `src/runtime/agent.rs` を ACP v2 client に書き換える。std スレッド上の
  current-thread runtime は今のまま。`Op` は v2 要求と `_horizon/*` 要求に
  なる。ハンドラは `UpdateSessionNotification` と `_horizon/*` 通知を
  session_id でセッションへ配り、`RequestPermissionRequest` は `Responder`
  をセッションに預ける（ユーザーの操作か CLI の approve/deny で応答、
  cancel 時は `Cancelled`）。`_horizon/host_tool` は今の host tool 経路へ。
- `runtime/attachment.rs` の `AttachmentState`（Connecting / Restoring /
  Ready / Failed / Disconnected）と `AgentUpdate` は残す。Restoring は
  `session/resume` 送信から応答まで、Ready は応答受領、Failed は再生途中の
  切断か `Lagged`、Disconnected は live 中の切断に対応させる。command を
  Ready まで待たせる今の挙動も保つ。
- `common.rs` の remoc 形の部分（`connect_hub`、`classify_connect_error`、
  `StreamEnd`）は terminald 用に残し、agent 側は ACP 用の接続と失敗分類を
  `agent.rs` に持つ。version 不一致の判定は `initialize` の応答の
  `_meta.horizon.ext_version` で行い、`_horizon/drain` で古い agentd を
  落として respawn する回復を保つ。
- **モデルの書き直し。** `LiveState` と `frame::fold` は `contract::Event` の
  上に書かれている。シェルは新たに `src/agent/model/` に v2 `SessionUpdate`
  と `_horizon/*` 通知を畳む fold を持ち、出力型は今の `AgentFrame` /
  `AgentFrameItem` / `SessionStatus` の形を保って `src/agent/view` と
  `src/agent/turns` の変更を最小にする。`crates/horizon-agent/src/frame` と
  `live.rs` は agentd 側（再生・永続化）で使われ続ける。シェルの
  `horizon-agent` 依存は `transcript` の描画ヘルパのために当面残す。
- `AgentSession` が送るものは `Prompt`、`Cancel`、`Approve{identity}`、
  `Deny{identity, reason}`、`ContinueTurn`、`Close` の六つ。モデル切替は
  応答が要り、attach していないセッションにも向くので、session の command
  ではなく `AgentdHandle::set_session_model` が接続上で
  `session/set_config_option` を送る。`control_plane.rs` の語彙はこれに写す。
- `session_lifecycle.rs` / `commands.rs` / `restore.rs` / `modals.rs` の
  `AgentdHandle` 呼び出しは名前を保ち、中身だけ ACP 要求に変える。
- `src/agent/auxiliary.rs`（タイトル要約の補助 AI）は provider へ直接 HTTP
  を打つだけで wire を使わない。対象外。

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
| `crates/horizon-agentd/tests/e2e.rs`（28 件） | crate の `Client.v2()` で socket に繋ぐ形に書き換え。項目は保つ |
| `crates/horizon-agentd/src/session/attachment/tests.rs`（9 件） | agentd 内部。そのまま |
| `crates/horizon-daemon-testkit/src/hub.rs` | ACP の接続・drain ヘルパに置き換え |
| `crates/horizon-terminald/tests/e2e.rs` の agentd drain | `_horizon/drain` |
| `src/runtime/tests.rs` の `FakeSessionHub`（31 件） | crate の `Channel::duplex()` 上で偽の v2 agent を動かす |
| `src/runtime/routing/tests.rs`（7 件）、`runtime/attachment.rs` の 2 件 | 型を差し替えて保つ |
| `crates/horizon-agent/tests/wire_schema.rs` | `crates/horizon-acp/tests/wire_schema.rs`（旧ファイルと `agent-wire.json` は削除） |
| `crates/horizon-agent/tests/skew.rs`（Postbag） | 削除 |
| `scripts/check-wire-schema.sh` | `agent-wire.json` の削除を RESHAPE にしない移行の腕を足す（`session-wire.json` の腕と同じ形） |

`.config/nextest.toml` の sandboxed profile は、socket を張る試験の名前が
変わる分だけ追随する。

## 段取り

一つのブランチで A から D を積み、切り替えは一括。E は別ブランチ。

| 段 | 内容 | 主な対象 |
|---|---|---|
| A | `horizon-acp` crate、拡張型、`_meta` 構造体、版定数、schema artifact | 新 crate |
| B | agentd を v2 agent に。pump の差し替え、写像、承認の往復、resume の再生、`_horizon/*` | `crates/horizon-agentd/src/{hub,main}.rs`、`hub/attachment.rs`、`session/{connection,events,approval}.rs`、`horizon-wire/src/daemon.rs` |
| C | シェルを v2 client に。`src/agent/model/` の fold、runtime、control plane の写し | `src/runtime/{agent,attachment,routing,mod}.rs`、`src/agent/{session,model}`、`src/workspace/*` |
| D | 試験と script の書き換え、`horizon-agent`/agentd/root からの remoc 除去、`wire.rs`/`wire/hub.rs`/`agent-wire.json` の削除、checker の移行の腕、AGENTS.md の protocol bump の記述更新、古いコメント（`hub.rs` の「TaskProgress は再生しない」等）の整理 | 上の試験面の表 |
| E | 外部エージェント（v1 client、spawn、正規化、ペイン配線）。config の相談を先に | `src/runtime/external_acp.rs` |

規模の目安（行数は main `c96f685a` の `wc -l`）: シェル runtime 側で
`agent.rs` 672、`attachment.rs` 161、`routing.rs` 382、`mod.rs` の agent 部分、
試験 `tests.rs` 31 件。agentd 側で `hub.rs` 384、`hub/attachment.rs` 68、
`connection.rs` 774、`events.rs` 123、e2e 28 件。

## 一度きりの移行手順

remoc を話す旧 `horizon-agentd` は新しいシェルから drain できない
（シェルに remoc client が残らないため）。切り替え時は旧 daemon を手で
止めてから新しいビルドを起動する。AGENTS.md の protocol bump の項に
書く（段 D）。

## 実装中に決めてよいこと

- messageId の決め方（セッションごとの連番に決着）。
- `_horizon/session_event` を一本にまとめるか、種類ごとに分けるか（一本に
  決着）。
- v1 の外部エージェントからの `SessionNotification` を `src/agent/model/`
  へ正規化する層の置き場（`horizon-acp` かシェル内か）。

## 関連

- `docs/acp-agentd-design.md`、`docs/agent-runtime-split-design.md`、
  `docs/remoc-adoption-design.md`（skew checker の由来）。
- crate の参照箇所: `examples/simple_agent_v2.rs`、
  `examples/v2_one_shot_client.rs`、`tests/application_dispatch_v2.rs`
  （`Send` なハンドラから `!Send` な前景へ mpsc で渡す形。GPUI 側の型）。
