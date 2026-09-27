# ACP をシェルと agentd の間の線にする

Status: design 2026-09-27。決定の記録は board #63（本文と 2026-09-27 のコメント）。
実装計画は `docs/acp-agentd-implementation-plan.md`。実装は未着手。

## 形

- シェル（`src/`）は ACP の client になる。`agent-client-protocol` crate
  の client 側を使い、v1 と v2 の両方を受ける。
- `horizon-agentd` は ACP の agent 側として話す。線は ACP v2（crate の
  `unstable_protocol_v2`、仕様上は draft）。agent hub の remoc は ACP に
  置き換える。terminald と logd の hub は remoc のまま。
- 外部の ACP エージェント（claude-agent-acp など）は同じ client がペインに
  直結して収容する。現時点の外部エージェントは v1 しか話さない
  （claude-agent-acp 0.81.2 は `protocolVersion: 1`）。外部エージェントの
  セッション管理（記録・detach・系譜）はそのエージェント実装に任せる。
  agentd を経由させる形（旧 roadmap の ACP-proxy provider 案）は採らない。
- 標準語彙で載る面は、相手が agentd でも外部エージェントでも同じ表示と
  操作になる。Horizon 固有の面は `_horizon/*` と `_meta` の拡張で、agentd
  が `initialize` の capability の `_meta` で広告したときだけ有効になる。

承認の三層、sandbox、judge、roles、skills、recall、knowledge、standing
memory、MoA、compaction、worktree 隔離、trusted project gate は agentd の
内側で完結し、この線の設計には現れない。外部エージェントが相手のとき、
承認を求めるかどうかの起点は相手側にあり、Horizon の承認方針は相手が
求めてきた分にしか効かない。

## 版

| | agentd | 外部エージェント |
|---|---|---|
| 版 | v2 | v1 |
| crate 側 | `Agent.v2()` / `Client.v2()`。v1/v2 併用の
`ClientProtocolConnector` も `unstable_protocol_v2` の下 | 既定の v1 |

v2 を agentd に選ぶ理由は、現在の contract との構造のズレを二つ解消する
ことにある。実行中ターンへの投入は v1 では禁止だが、v2 では
`session/prompt` が「挿入して messageId を返す」形なので、今 agentd が持つ
キュー（`providers/rig/session/input.rs`）をそのまま置ける。状態と turn
終端は v2 の `state_update`（running / idle+stopReason / requires_action）
で明示される。v2 は schema crate 1.x の feature の中にあり minor 更新で
変わりうるが、両端とも Horizon が持つので、版不一致は既存の lockstep 運用
（AGENTS.md「A protocol bump needs a full Horizon restart」）で吸収する。

## 対応表

今 `AgentWireEvent` / `Command` / hub RPC として線を越えているものの行き先。
「標準」は v1 で安定な語彙、「v2」は v2 のみ。

### 標準語彙に載るもの

| 今の線上のもの | ACP |
|---|---|
| `UserMessage` | `session/prompt` |
| `Cancel` | `session/cancel` |
| `ApproveToolCall` / `DenyToolCall` | `session/request_permission` への応答 |
| `SetSessionModel`、`SessionModel`、`SessionSelection`、`list_provider_models` | config option（category `model`、select 型）と `config_option_update` |
| `Shutdown` | `session/close` |
| `new_agent` | `session/new`（cwd に workspace_root） |
| `list_agents` | `session/list` |
| `attach_agent` の再生 | `session/resume`{replayFrom}（v1 は `session/load`） |
| `hello` | `initialize` |
| `AssistantTextDelta` / `ReasoningDelta` / `MessageCommitted` | `agent_message_chunk` / `agent_thought_chunk`、v2 は messageId 付き `*_message` upsert |
| `ToolCallRequested` / `Started` / `Finished` | `tool_call_update`（rawInput/rawOutput、diff と terminal の content、kind、locations） |
| `ApprovalRequested` | `session/request_permission`。選択肢は agent が定義するので、DomainGrant 等も「ドメインを許可して再実行」という optionId として出せる |
| `TurnEnded` | stopReason。Completed は `end_turn`、Cancelled は `cancelled`、HaltedByIterationCap は `max_turn_requests` |
| `StateChanged` | v2 `state_update` |
| タブタイトル | `session_info_update` |
| `Error` | JSON-RPC error |
| `ProviderRequestUsage` | `usage_update`（文脈使用量として） |

### unstable 層に載るもの

これらは仕様が "may be removed or changed at any point" とする層にある。
相手が agentd なら `_horizon/*` で代替できるので、使うかどうかは実装時に
決める（board #63 の未決）。

- `HistoryCleared` は `compaction_update`。意味は要約寄りなので
  cleared_call_ids は `_meta` に添える。
- `skipped_lines`、`Exited`、`ProviderRateLimited` の表示は `notice`。
- token 内訳は idle 時の `Usage`（`unstable_end_turn_token_usage`）。
- `list_providers` / `reload_provider_config` は `providers/list` と
  `providers/set`（`unstable_llm_providers`）。
- host tool の往復は MCP over ACP（`unstable_mcp_over_acp`）。

### `_meta` で添えるもの

標準メッセージに Horizon 固有の欄を足す。仕様が認める形で、両端が Horizon
でないときは無視される。

- `session/new` の provider_id、role_id、isolate、spawn_source_session_id、
  シェルが先に発行する session_id
- SessionInfo の parent_session_id、role_id、provider_id
- tool call の occurrence_id、denied、auto_approved、policy_tier
- ApprovalKind の構造化 payload（domains、denials、grants、writable_roots、
  prior_result）
- Deny の reason
- Message の役割 TaskNotification / AutoContinue
- stopReason の Failed と HaltedByDoomLoop（`_` 始まりの独自値）
- `WorkspaceRootResolved`

### `_horizon/*` の拡張メソッド・通知が要るもの

ACP に居場所が無い。線を越える必要があるかどうかの選別は実装時に行う
（2026-09-27 時点の grep では、Input routing 系 6 種、Environment 系 3 種、
`MoaPassStarted`、`MemorySeeded` はシェル側で frame に畳まれるだけで表示に
使われていない）。

- `ContinueTurn`
- durable routed input 一式。`SessionInput`、`SendSessionInput`、
  `AcknowledgeDelivery` と、`InputAccepted`、`InputStarted`、
  `InputQueuePaused`、`InputOutcome`、`DeliveryAcknowledged`、
  `SessionInputSent`
- `TaskProgress`
- `MoaPassStarted`
- `MemoryDigest`、`MemoryCheckpointMissed`、`MemorySeeded`
- `ActivateWorktree` と `EnvironmentReady`、`EnvironmentActivated`、
  `EnvironmentActivationFailed`
- `SessionResumed`
- `ToolCallProgress`（引数のストリーム）
- `ProviderRequestSent`、`FirstToken`、`Finished`、`RateLimited`
- hub の `drain`、`watch_board`、`ensure_board_organizer`
- host tool の往復（MCP over ACP を使わない場合）

`ApprovalResolved` は人間の決定の監査記録で、決めたのはシェル自身なので
線を越えなくても transcript に出せる。

## 構造のズレ

- **session id の発行者。** ACP では agent が発行する。シェルが先に発行して
  workspace に保存する今の流れを保つなら、`_meta` で渡して agentd が
  そのまま採用する。
- **attach の解除。** 終了せずに購読だけ止める方法が無く、`session/close`
  は終了。接続を切るか、届く更新を捨てるかになる。
- **daemon と一接続多セッション。** 仕様の記述は stdio の子プロセス前提
  だが、crate は任意のバイトストリームを受け、sessionId で多重化できる。
  agentd の Unix socket 上で動かすのは実装の自由。
- **版管理。** 今の lockstep（`AGENT_PROTOCOL_VERSION`）は `initialize` の
  `_meta` に載せる。`docs/agent-runtime-split-design.md` が hello の版
  チェックを wire から分けた理由がここ。
- **wire skew checker。** `crates/horizon-agent/schema/agent-wire.json` と
  `scripts/check-wire-schema.sh` の agent 側は、ACP 拡張のスキーマ管理に
  置き換わる。

## 関連

- `docs/agent-runtime-split-design.md` "ACP compatibility guardrails"。
  この文書はその guardrail 6 が求めた mapping table に当たる。
- `docs/research/hosting-external-agents.md`（2026-08-06）。外部エージェント
  側のネイティブ機能がアダプタ越しに失われる調査。この線の設計とは別の軸
  で、外部エージェントを収容するときの前提として残る。
- 仕様: agentclientprotocol.com の `/protocol/v1/*` と `/protocol/v2/*`、
  `schema/v1|v2/schema(.unstable).json`。crate: `agent-client-protocol`
  2.2.0 / `agent-client-protocol-schema` 1.9.1（2026-09-27 時点）。
