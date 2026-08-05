# 変更通知の手段と標準 — 調査記録(2026-08-06)

調査日: 2026-08-06。web 検証つき(出典は各所にリンク)。発端は「イベントログ
(JSONL)/ DuckDB 射影への変更を、実装に結合せずエージェント等へ伝えたい」
というオーナーの問い。**この調査の中心軸(書き手非依存性)は、後続の設計
議論で要件ごと解消された**(logd 設計 — `docs/logd-design.md` を参照。
ログの書き込みを収集プロセスに一元化すれば、通知はその書き手の役割で
あり、書き手非依存の機構は不要になる)。事実の記録として残す。

## 確定した事実

- **inotify は追記で `IN_MODIFY` を発火し、唯一「書き手の協力なしに」
  通知が生じる機構**。未読の同種イベントは合体(coalesce)され、キュー
  あふれは `IN_Q_OVERFLOW`(wd=-1)として通知される — poke+カーソル
  モデルなら合体もあふれも無害(「見ろ」という信号に変わりない)。
  非再帰(動的に増えるディレクトリは親を `IN_CREATE` で見て watch を
  追加し、起動時/あふれ時に再走査)。[inotify(7)]
- **`tail -F` は inotify 裏付きの正しいゼロ結合消費者**。ただし既定は
  末尾開始なのでカーソル無し — `tail -n +$((cursor+1)) -F` で
  カーソル意味論になる。[coreutils]
- **systemd `.path` の罠2つ**: `PathChanged=` は「書き込み用に開かれた
  fd が閉じられた時」のみ発火(fd を持ちっぱなしのデーモンの追記では
  沈黙 — 半分だけ動く最悪の形)。`PathModified=` が正。また
  `TriggerLimitBurst=`(既定 200回/2秒)を超えると **unit が fail して
  以後の起床がすべて止まる**。[systemd.path(5)]
- **D-Bus** はローカル pub/sub として最も綺麗だが書き手の協力が必要で、
  本リポジトリは control plane 検討時に依存性(macOS の dbus-daemon 等)
  を理由に却下済み(`docs/research/cli-control-plane.md`)。
- **人間向けは `org.freedesktop.Notifications`**(freedesktop 標準 v1.3、
  Rust は `notify-rust`)。`replaces_id` で通知の積み上がりを防ぎ、
  `ActionInvoked` でクリック動作を拾える。**機械の poke 経路とは厳密に
  分離する**(人間チャネルはカーソルと合成できない失われ方をする)。
  なおファイル監視の `notify` crate と名前が紛らわしい。
- **設計語彙**: 本設計は「単一マシンの Kafka 形ログ(consumer offset =
  カーソル)+ ファイルシステム/ソケット poke」。Postgres の
  LISTEN/NOTIFY 公式 doc 自身が「小さな参照だけ流して実体は問い
  合わせろ」= poke+カーソルを推奨している。通知を正本にする主流
  システムは存在しない。

## 採用への写像(logd 設計後の整理)

logd(ログ基盤デーモン)が唯一の書き手になったため、通知の一次経路は
logd の購読ストリーム(socket、sequence 番号のみ)になり、inotify は
「logd 不在時の低水準の代替」以上の地位を持たない。`tail -n +N -F` は
外部消費者(Claude Code 等)の暫定手段として引き続き正しい。

出典: man7.org の inotify(7)/fanotify(7)/systemd.path(5)、GNU coreutils
manual、docs.rs/notify、freedesktop Desktop Notifications Specification
v1.3、postgresql.org SQL-NOTIFY、kafka.apache.org design。
