# remoc 採用可否スパイク (2026-07-20)

調査日: 2026-07-20(worker セッションによるスパイク)。目的: horizon の UI ↔ horizon-sessiond IPC
(unix socket 上の JSONL + serde)の置き換え候補として [remoc](https://github.com/ENQT-GmbH/remoc)
(検証時最新 0.18.3)を実測評価する。スパイクコード一式は `spike/remoc/`(独立クレート、
自身の空 `[workspace]` テーブルによりリポジトリの workspace / Cargo.lock / clippy ゲートから隔離)。
ペイロードは合成ではなく `crates/horizon-terminal-core` の実物 `TerminalFrame`/`TerminalLine`/
`TerminalSpan` を path 依存で使用。

**結論サマリ(4項目判定)**

1. **性能**: remoc 既定の Postbag(Full) は現行 JSONL 比で **CPU が遅い**(encode ~3.4x /
   decode ~3.3x)、ワイヤは ~26% 小さい。e2e スループット上限は JSONL の約 1/4。ただし絶対値は
   200x50 フル装飾フレームでも 361 fps あり、16ms コアレス(60fps)要件に対しブロッカーではない。
   同条件の PostbagSlim はワイヤ **1/8**・CPU JSON 同等という別の顔を持つ(進化制約あり、§1)。
2. **skew 耐性**: (a) フィールド追加=無視される○ (b) 欠如=`#[serde(default)]` が効く○
   (c) 未知 enum variant=素のままではエラー、**`#[serde(other)]` は Postbag で機能**し Unknown に
   落ちる○。さらに live チャネル上ではデコード失敗はその 1 item の `recv error` で済み
   **チャネル自体は死なない**(§2)。
3. **remoc 自身の版互換**: chmux ワイヤプロトコルは 2021 年以降 v2→v3 の一度だけ変更で、v3 は
   「fully backward compatible」(公式 CHANGELOG)。実験でも **0.16.1 ↔ 0.18.3 が同一コーデック
   なら完全相互運用**(live チャネル転送込み)を確認。危険は 0.18.0 の**既定コーデック変更
   (JSON→Postbag)** のような "既定値の断層" で、コーデックを明示 pin すれば管理可能(§3)。
4. **ergonomics**: `#[rtc::remote]` + 「チャネル入り struct を返す」は素直に書けて動く。切断時は
   watch/mpsc/rtc call の全てが即座に「multiplexer terminated」系エラーを返し、判別可能(§4)。

---

## 0. 環境と再現手順

- Linux (x86_64)、rustc 1.96.0、release ビルド(mold リンカ、`debuginfo=line-tables-only`)。
- ベンチはワンプロセス内で `tokio::net::UnixStream::pair()` の両端を worker 4 スレッドの
  runtime に載せる形(実運用のプロセス境界は無い。数値は相対比較と上限の見積り用)。
- フレーム合成(`spike/remoc/src/frames.rs`): 80x24 と 200x50、行ごとに複数スパン、
  fg/bg は Named/Indexed/Rgb 混在、italic/underline を確率的に付与、CJK 文字含む。
  毎フレーム 1 行スクロールし全行が変化する「重い TUI」ケース。実運用にはこの他に
  `TerminalFrameDiff` 配信があるため、本測定はフル・フレーム配信の上限ケースにあたる。
- 再現:

```sh
cd spike/remoc
cargo run --release --bin bench                       # §1 の全数値
cargo test --release --test skew -- --nocapture       # §2
cargo test --release --test cross_version -- --nocapture  # §3 の実験部
cargo test --release --test ergonomics -- --nocapture # §4
```

数値は 3 回実行の中央値。走行間ばらつきは ±10〜20%。

## 1. hot path 性能: フレームストリーム実測

### 1a. フレームあたり encode+decode CPU 時間・エンコードサイズ(コーデック単体)

| コーデック | 80x24 enc | 80x24 dec | 80x24 size | 200x50 enc | 200x50 dec | 200x50 size |
|---|---|---|---|---|---|---|
| serde_json(現行) | 42 µs | 138 µs | 46,454 B | 217 µs | 784 µs | 241,037 B |
| **Postbag Full(remoc 既定)** | **144 µs** | **459 µs** | **34,487 B** | **735 µs** | **2.40 ms** | **178,743 B** |
| Postbag Slim | 52 µs | 158 µs | 5,922 B | 235 µs | 862 µs | 30,797 B |
| CBOR(ciborium) | 37 µs | 168 µs | 33,045 B | 183 µs | 912 µs | 171,526 B |

- **Postbag(Full) は JSONL より CPU が重い**: encode 3.4x、decode 3.3x(80x24)。Full 構成は
  フィールド識別子(名前文字列)を書き・読み時に照合するため、スパン(8 フィールド × 数百個/
  フレーム)主体のこのペイロードでは識別子コストが支配的になる。サイズ削減は -26% にとどまる。
- **Postbag Slim の異常値は本スパイクの発見**: フィールド名を書かないため 80x24 で **5.9KB/frame
  (JSON の 1/8)**、CPU も JSON 同等。ただし互換性は「struct 末尾への追加/削除・enum 末尾への
  variant 追加のみ」(postbag README)で、Full の「並べ替え・任意位置追加削除」より弱い。
  なお Full には `#[serde(rename = "_0")]`〜`_59` の数値 ID エンコード(1 バイト識別子)という
  中間案があり、識別子コストを圧縮できるが今回は未測定。
- 60fps 換算の codec CPU 占有(200x50、enc+dec 合算): JSON 6%、Postbag Full **19%**、
  Slim 6.6%、CBOR 6.6%(1 コア比)。

### 1b. e2e スループット上限とワイヤバイト(unix socket、rch::mpsc / 現行相当 JSONL)

buffer: `rch::mpsc::channel(64)`、`Cfg::default()`(chunk 16KiB)、`Connect::io_buffered` 256KiB。
逆方向(受信側→送信側)のフロー制御トラフィックは全ケースで **≤11 B/frame** と無視できる。

| 方式 | 80x24 fps | 80x24 wire B/frame | 200x50 fps | 200x50 wire B/frame |
|---|---|---|---|---|
| JSONL + UnixStream(現行相当) | **8,918** | 46,445 | **1,175** | 241,038 |
| remoc mpsc + Postbag Full | 2,023 | 34,526 | 361 | 178,908 |
| remoc mpsc + Postbag Slim | 5,493 | 5,935 | 1,047 | 30,827 |
| remoc mpsc + CBOR | 3,722 | 33,081 | 740 | 171,687 |
| remoc mpsc + JSON | 2,122 | 46,496 | 388 | 241,260 |

- ワイヤ実測(カウンタ挟み込み)はコーデック単体サイズ +0.1% 程度 = **chmux の多重化
  オーバーヘッドはフレームあたり数十バイトで無視できる**。
- 同一コーデック比較(JSONL 8,918 vs remoc mpsc JSON 2,122)から、**remoc フレームワーク自体の
  タスクホップ/チャネル機構で e2e 上限が約 1/4 になる**ことが分かる(ワイヤではなく CPU・
  スケジューリング由来)。Postbag Full の遅さと合わせても、200x50 で 361 fps = 60fps 要件の
  6 倍の余裕があり、実用上の性能ブロッカーではない。ただし「remoc 化で速くなる」は成立しない。

### 1c. rch::watch の挙動(最新値スキップ)

- 過負荷実験(送信側が 2 秒間ノーウェイトで値を差し替え続ける): producer は 2 秒に 16〜18 万回
  値を書き換え、ワイヤには帯域が許す分(~7 フレームに 1 回)だけ直列化され、受信側の観測は
  **253 obs/s(80x24)/ 231 obs/s(200x50)**。中間値は握り潰され、キューは伸びない。
- ペーシング実験(送信 1ms 間隔 × 500 フレーム、受信側 5ms/回): 受信側の観測は 166 回、
  観測シーケンスは `[20, 23, 26, 29, ...]` と飛び飛びで、**最終値(センチネル直前の seq 498)は
  必ず届く**。「遅い受信側では中間フレームがスキップされ最新値が届く」を確認。
- 注意 2 点: (1) watch は受信側が読む読まないに関わらず帯域上限まで直列化・送信し続けるため、
  ワイヤ帯域の節約にはならない(節約されるのは受信側の処理だけ)。(2) 送信側 `Sender` を
  センチネル送信直後に drop すると受信側が最終値を観測する前にチャネルが閉じ得るため、
  送信側は少し生かしておく必要があった(ベンチ実装の注記参照)。

## 2. skew 耐性の実証(Postbag Full = remoc 既定)

`spike/remoc/tests/skew.rs`。V1/V2 型ペアは `src/skew.rs`(TerminalCommand の運用を模した
`CommandV1/V2` と `FrameMetaV1/V2`)。結果は全て実測ログの転記:

| ケース | 結果 |
|---|---|
| (a) フィールド追加(V2 送信 → V1 受信) | **○** 追加フィールドは黙って無視される |
| (b) フィールド欠如(V1 送信 → V2 受信、`#[serde(default)]` 付き) | **○** default が適用される(`zoom: None, alt_screen: false`) |
| (b') 同上で `#[serde(default)]` **無し** | **✕** `serde error: missing field 'alt_screen'` — default 属性は必須 |
| (c) enum に新 variant(V2 `Scroll` → V1) | **✕(期待通り)** `serde error: unknown variant 'Scroll', expected one of 'Key', 'Resize', 'Paste'` |
| (c) 同上、V1 側に `#[serde(other)] Unknown` | **○ Postbag で機能する** — `Unknown` にデコードされる |
| (c2) live `rch::mpsc` 越しに Key → Scroll → Paste を送付(受信側 V1) | Key は届き、Scroll は `recv error: receive error: deserialization error: serde error: unknown variant …` になり、**続く Paste は正常に届く = チャネルは 1 item の失敗で死なない** |
| (d) rch チャネルを含む struct のフィールド追加(V2→V1) | **○** 追加フィールド無視、同梱チャネルはその後も生きて全 item 到達 |
| (d') 同(V1→V2、default 付き) | **○** default 適用、チャネル生存 |

`TerminalCommand` に variant を足す運用への含意: **新→旧方向は受け側が `#[serde(other)]` を
持っていれば安全に落とせる**(unit variant に落ちるのでペイロードは失われる。「未知コマンドは
無視」という horizon の想定挙動には十分)。持っていない場合も被害はその 1 コマンドの
recv error で済み、接続・チャネルは無傷。旧→新方向は常に安全。

## 3. remoc 自身のワイヤ互換保証(調査 + 裏取り実験)

**プロトコル階層は 2 層**あり、保証が異なる。

1. **chmux(多重化層)**: `remoc/src/chmux/mod.rs:41` `pub const PROTOCOL_VERSION: u8 = 3`。
   接続確立時に `MultiplexMsg::Hello { version, cfg }` を交換し(`chmux/mux.rs` の
   `exchange_hello`)、**バージョン不一致で接続を拒否するコードは存在しない**。相手の version は
   保持され、`PROTOCOL_VERSION_PORT_ID`(=3)以上かどうかで port-id 機能を出し分ける後方互換
   ゲートに使われる(`mux.rs:739,778`)。変更履歴は crate 公式 CHANGELOG で:
   - v1→v2: "Credits system and new message format"(2021-07-08、remoc 0.8.0 で出荷)。
   - v2→v3: **remoc 0.12.0(2024-04-03)** — "chmux: protocol version is now 3; **fully backward
     compatible**, but custom id and forwarding requires endpoint of same or higher version"。
   - モジュール docs(`chmux/mod.rs`)は保守的に「同一 protocol version の端点のみ通信可能。
     protocol version の変更は remoc crate の major version 増を伴う」と宣言している
     (0.x 系なので "major" = 0.x の x)。つまり**公式な約束は「バージョン交渉はしない。壊すときは
     semver でシグナルする」**であり、実績としては 2021 年以降ワイヤ破壊は一度も無い。
2. **データ層(コーデック)**: chmux は合意機構を持たず、両端が同じコーデックを使う前提。
   **remoc 0.18.0(2025-09-07)で既定コーデックが JSON → Postbag に変更**(CHANGELOG に
   BREAKING と明記。旧側と話す場合は `default-codec-json` feature を指定せよとの移行注記付き)。
   0.17.3 で Postbag が experimental 追加、0.18.1/0.18.2 の postbag 0.3/0.4 更新は
   "fully compatible" と明記。

**裏取り実験**(`spike/remoc/tests/cross_version.rs`: crates.io の remoc **0.16.1** と
**0.18.3** をリンクした 2 バイナリを unix socket で接続):

- 同一コーデック(JSON 明示): chmux 接続・base channel・**live `rch::mpsc` チャネルの転送 +
  100 item ストリーム**・応答の往復まで全て成功。channel 転送の内部表現
  (`TransportedReceiver`)も 0.16.1/0.18.3 で同一であることをソース比較で確認済み。
- 既定コーデック同士(0.16=JSON vs 0.18=Postbag): **chmux 接続は成立**し、最初の base channel
  受信が `deserialization error: IO error: failed to fill whole buffer` で失敗(接続断に伝播)。

**結論**: remoc のバージョン差そのもの(chmux)は 2021 年以降実質安定で、0.12 の唯一の変更も
後方互換だった。実運用で危険なのは**既定値の断層**(0.18.0 のコーデック変更)と将来の
semver シグナル付き破壊のみ。horizon のように UI と daemon が別バイナリで rolling に混在し得る
構成では、(1) コーデックを feature/型引数で**明示 pin** し、(2) remoc の 0.x バージョンを
workspace で一元管理し、(3) 0.x bump 時は UI/daemon を同時更新(既存の wire-version bump 運用と
同じ扱い)にすれば管理可能。remoc 自身にはアプリケーションレベルのバージョンネゴ機構は無いので、
horizon の wire version handshake は remoc 化しても残す価値がある。

## 4. ergonomics 所見(副次)

`spike/remoc/src/svc.rs` + `tests/ergonomics.rs`。`#[rtc::remote] trait TerminalService` の
`attach_terminal() -> Result<TerminalAttachment, AttachError>`(`TerminalAttachment { frames:
rch::watch::Receiver<TerminalFrame>, input: rch::mpsc::Sender<TerminalInput> }`)は
**そのまま書けて動く** — チャネル入り struct は derive(Serialize, Deserialize) するだけで
値ごと remote 化される。エラー型は `From<rtc::CallError>` 実装が必須で、ドメインエラーと
輸送エラーを 1 つの enum に同居させる公式パターン(examples/rtc)は素直。嵌りどころは
3 点: `Connect::io` は両端のハンドシェイクを**同時に** poll しないと 60 秒の
`ChMux(Timeout)` になる(片側ずつ `.await` すると即デッドロック)、server 構築時にコーデックの
型注釈が必要(`ServerSharedMut::<_, codec::Default>::new`)、base sender は `&mut`。
**切断時の挙動**(サーバ側 mux を abort して観測): watch は `changed()` が一度 Ok を返した後
`borrow_and_update()` が `Err(RemoteReceive(Receive(ChMux)))`(表示: "receive error:
multiplexer terminated")を返す — エラー自体が「値の変化」として通知されるので、UI 側は
borrow の `Result` を見て「daemon 消失」状態に遷移すればよい。mpsc sender は
`send error: multiplexer terminated`(`is_disconnected()=true` / `is_closed()=false` で
「相手が閉じた」と「接続が死んだ」を判別可)、rtc 呼び出しは `Err(Call(RemoteSend(Send(ChMux))))`。
全チャネルが即座に・判別可能な形で切断を報告するため、再接続フローは書きやすい。

## 5. 採用判定への示唆

- 性能面: remoc 化は**速くならない**(Postbag Full で e2e 上限 1/4)が、60fps 要件には
  200x50 フル・フレームでも 6 倍の余裕があり、diff 配信併用ならさらに軽い。ブロッカーではない。
- ワイヤ帯域が問題になった場合の弾は remoc 内にある(PostbagSlim = 1/8、ただし進化制約との
  トレードオフ。numeric field id は中間案)。
- skew は現行 JSONL(serde_json も未知フィールド無視 + default)と同等以上の性質を Postbag が
  持ち、`#[serde(other)]` + 「1 item 失敗でチャネルは死なない」により `TerminalCommand` 運用に
  耐える。
- 最大の獲得物は §4 の「チャネル入り struct を返す RPC」= attach/input/frames の配線コードと
  切断ハンドリングの大幅な定型化。最大のコストは remoc 0.x への追随(既定値断層に注意)と、
  hot path CPU の 3 倍化(コーデック選択で緩和可)。
