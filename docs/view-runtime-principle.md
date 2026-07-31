# ビュー種別 = runtime — プロセス構成の原則

Status: 決定 2026-07-30（オーナー）。terminald 分離
（`docs/terminald-split-design.md`）の事後整理として、以後の構成判断の
基準を固定する。

## 何が起きたか

Horizon は当初「セッションの daemon」（`horizon-sessiond`）を 1 つ持つ
構成だった。2026-07-30 の terminald 分離で、実体は
**「ビュー種別ごとに runtime プロセスがあり、それぞれ独立に更新・
再起動される」**構造に変わった。`Reload Session Runtime` は今や
セッション一般ではなく *エージェントというビュー種別* の runtime だけ
を再起動し、agent runtime のホスト（当時 `horizon-sessiond`、現在の
`horizon-agentd`）はその 1 種別を持つに過ぎない。

つまり実体が先に動き、名前と概念が遅れている。本 doc はその概念を
正典化する（名前の追随は別途）。

## 原則

1. **ビュー種別が runtime の単位である。** 端末・エージェント・（将来
   の）WASM プラグインは、それぞれ自分の runtime プロセスを持ちうる。
   セッションは runtime の *中身* であって、分割の単位ではない。
2. **reload はビュー種別ごとに閉じる。** あるビュー種別の runtime を
   更新しても、他の種別のセッションは生き続ける。これが分割の目的で
   あり、成立しない分割は分割の意味がない。
3. **共有の土台は 1 箇所に持つ。** wire 型・socket 規約・hello と
   バージョン交渉・runtime 制御（drain/respawn/adoption）は共通の
   基盤であり、ビュー種別ごとに複製しない（backlog 70 の
   `horizon-wire` 切り出しはこの前提工事）。
4. **クレートは runtime の境界に沿う。** 「このクレートは他のビュー
   種別の何も使わない」が manifest で検証できることを、分割の正しさの
   指標とする（terminald を bin target でなく crate にした理由）。

## 分割の基準（プロセスを増やす条件）

プロセスを増やすことには実費がある — 発見・spawn・バージョン検査・
drain・診断の組み合わせが増え、stale daemon の面（backlog 51）や
workspace restore の inventory 検査も種別の数だけ膨らむ。したがって
**「ビュー種別だから分ける」ではなく、次の 2 つが揃った時に分ける**:

- **更新頻度が実測で有意に違う**（terminald の場合: 直近 60 日で
  agent 側 135 コミットに対し terminal 側は 1/3 程度）
- **生存価値がある**（中で走っているものを殺したくない。端末の場合は
  対話 CLI のプロセスそのもの）

片方しか満たさないなら、同じ runtime に相乗りさせる方が総コストは
低い。

## 適用予定

- **命名の追随**（`sessiond` → agent runtime を指す名前、
  `Reload Session Runtime` → 対応する名前）: 実体に言葉を合わせる
  機械的な整理。**完了 2026-07-31**: crate は `horizon-agentd`、
  コマンドは `Reload Agent Runtime`、shell 側 client は
  `src/runtime/`（`AgentdHandle` / `TerminaldHandle`）。
  `docs/runtime-crate-alignment-design.md` phase 3。
- **WASM プラグインビュー**（roadmap）: 上記基準を満たすなら第 3 の
  runtime として立てる。満たさないうちは既存 runtime に相乗りさせる。
  いずれにせよ「相乗りさせたまま結合を作り込まない」ことを、この
  原則が防ぐ役割を持つ。
