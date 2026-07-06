# Plan 01 — セッション管理モーダル

Status: ready

## 背景

cursor の導入(docs/workspace-mode-design.md)でパレットの役割が変わり、
control_surface の Commands/Workspace の Tab 切替は過剰になった。セッシ
ョン管理(detached の発見・attach・terminate)は独立した面として立てる。

## スコープ

- セッション管理モーダルを開く**専用コマンド**を新設(パレットに列挙、
  `[keybindings]` で任意バインド可能 — 既存の予約名機構をそのまま使う)。
- 既存 workspace overview の機能(一覧・attach・terminate、動的件数)を
  モーダルへ移し、パレットの Tab 切替を廃止(パレットは Commands 専用化)。
- attach 等の「開く系」はモーダル経由=人間の面なので activate=true(潜る)
  — docs/cli-control-plane-design.md の Second revision の原則どおり。

## 完了条件

- detached セッションの発見/attach/terminate がモーダルから可能。
- Tab 切替が消え、関連する表記(ステータスバー・README・スモーク)が同期。
- 既存テストとスモークシナリオが緑。ゲート緑。

## ドメインセッションに委ねる設計判断

- モーダルの見た目・検索・キー操作(workspace mode のキー判定器との整合)。
- セッション行の情報量(種別・attach 状態・タイトル以外に何を出すか)。

## 参照

workspace-mode-design.md / cli-control-plane-design.md / ux-principles.md
