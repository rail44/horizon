# コードデータベース:数え方の 3 つの欠陥の修正(2026-10-30)

現在の木の軸の読み判定(`code-db-axes-reading-a-2026-10-30.md`、`code-db-axes-reading-b-2026-10-30.md`)が誤報の機構として挙げたもののうち、どのリポジトリでもコードと食い違う出力になる 3 つの数え方を直した。オーナーは 2026-10-30 に「1〜3は直しましょう」と承認した(依頼文による)。直したのは `duckdb-ext/sql/` の SQL、`check.py`、README で、Rust は `code-db` の end-to-end テストの期待値 1 か所だけを変えた(4 節)。設計文書(`docs/code-database-design.md`)の更新は依頼側に残している。

依頼が直さないと決めたもの(fan-in にテストからの参照を数えること、`loc` にコメントの行を数えること、定数の unit を辺の端にすること、`pub use` の辺)は変えていない。判断の残る数え方で、欠陥ではないとされている。

## 1. 3 つの欠陥と修正

### 1.1 自分自身と重なる断片の組(`duplication`)

**欠陥。** jscpd は、同じファイルの中で行の範囲が重なる 2 つの断片を組として報告する。似た行の並び(データの表、enum の variant、`concat!` の文字列)が、1〜2 行ずらした自分自身と一致したものである。範囲が重なる 2 つの断片は 2 つの写しではない。読み判定 B では `duplication` の上位 10 件のうち 8 件が `same_unit` で、そのうち 7 件がこれだった(機構 D1)。

**修正。** 2 つの断片が同じファイルにあり、行の範囲が 1 行でも重なる組は、クエリが読むビュー `clone_pairs` から除く。jscpd の報告はそのまま `clone_report_pairs`(前の `clone_pairs` を改名したもの、報告どおりの 1 行 1 組)に残る。同じファイルの、重ならない 2 つの断片は残す(1 つの unit の中の写し)。

**数。** 報告は exact 277 組、normalized 1,252 組、計 1,529 組だった。除いたのは 220 組(exact 1、normalized 219)で、テストでない unit 同士の組では 107 組だった。重ならない同じ unit の組は残り、修正後の 1,074 組のうち 527 組(テストでないもの 92 組)が `same_unit` である。

### 1.2 同じ組が 2 行になる(`duplication`)

**欠陥。** jscpd は exact(識別子とリテラルを保つ)と normalized(区別しない)の 2 回走り、同じ 2 か所を両方が報告すると別々の行になる(読み判定 B の 15・16 位の `aggregate_receipt`、機構 D2)。

**修正。** exact の組の断片が、normalized の組の断片と両側で重なる(同じ 2 つのファイルで、それぞれの側の範囲が重なる。側の順は問わない)とき、2 つを 1 組にする。1 つの exact の組が複数の normalized の組と重なるときは、共有する行の最も多いもの(同数なら report_pair_id の小さいもの)に付ける。1 つの normalized の組は複数の exact の組を受け取ることがある。1 つの報告の中の組どうしは合わせない(2.2 節)。合わせた組の範囲は各側の和集合で、`tokens` と `lines` は合わせた組のうち最大の値である(jscpd の数で、和集合の範囲で数え直してはいない)。`sources` は見つけた報告の一覧(`[exact, normalized]` など)、`report_pairs` は合わせた元の組の `report_pair_id` である。比べる前に各組の側を並べ直す(パスの小さい側を a に、同じファイルなら開始行の早い側を a に)。

**数。** 1.1 で残った 1,309 組(exact 276、normalized 1,033)のうち、exact の 235 組が normalized の 228 組に合わさり、1,074 組になった。両方の報告が見つけた組が 228、exact だけが 41、normalized だけが 805 である。normalized の組が exact の組を 2 つ受け取ったのは 7 組だった。合わせた 235 件の範囲の関係は、同じ範囲 89、normalized が exact を含む 136、exact が normalized を含む 2、どちらも他方を含まない 8 だった(どちらも含まない 8 件では、exact の断片の行の 70% 以上を共有していた)。

### 1.3 ファイルのモジュールが中身を合算し、中身と並ぶ(`coupling`、`intent`)

**欠陥。** `unit_scope` はモジュールについて、そのモジュールと中に含む unit のすべてを表すものとしていたので、ファイルのモジュールの fan-in は中の全 unit の合算になっていた。1 つの型だけのファイルは、ファイルと型がほぼ同じ数で 2 行並び(`horizon-wire` の `session` と `SessionId`)、ファイルの行が item の行より上に来た(機構 C1)。`intent` もモジュールの行の `fan_in_crates` で同じ影響を受けていた。

**修正(依頼側が決めた形)。** unit の段では、モジュールの行は自分の symbol への参照だけを数え、中身の合算をしない。ファイルごとの合算は `coupling` の `level := 'file'` で明示して出し、`crate` の段も足した(`size` の段と同じ形。列の先頭に `level` を足した)。

- `unit_fan_in` は、モジュールについて、その symbol への参照の occurrence のうち、モジュールを名指す path の要素(`crate::a::f` の `a`、`use crate::a;`、他の crate からの crate 名、その別名)を、モジュールの範囲の外にある unit から数える。モジュール以外の unit の数え方は変えていない。
- `coupling` の `level := 'file'` は、2 つのファイルの間の unit の辺(`unit_edges`)を、参照される側のファイルで集めたもの(fan-in)と参照する側のファイルで集めたもの(fan-out)である。`loc` はファイルのモジュールのもの。`level := 'crate'` は crate をまたぐ辺を同じように集め、`loc` は crate のファイルのモジュールの合計である。
- `level := 'file'` の行は、前のファイルのモジュールの行と一致することを確かめた(fan-in・fan-out の 3 列ずつ、`include_tests` で 520 ファイルすべて、差 0)。

**SCIP にあるモジュールの symbol への参照。** この木で、モジュールは 758 個(ファイルのモジュール 532、crate の根 49)、その symbol は 734 個である。rust-analyzer はモジュールの symbol に次の occurrence を付けていた(モジュールの範囲の外のもの)。

| 種類 | occurrence | 届くモジュール |
| --- | ---: | ---: |
| path の要素としての名前(コードの中) | 4,297 | 226 |
| `use` の行の名前 | 2,099 | 376 |
| `crate::`・`super::`・`self::` のキーワード | 2,757 | 96 |
| 可視性 `pub(crate)`・`pub(super)` | 2,553 | 58 |
| `mod a;` の宣言 | 483 | 483 |
| `pub(in …)` の path の要素 | 85 | 7 |

数えるのは上の 2 行(名前)だけで、キーワード、可視性、`pub(in …)`、`mod a;` の宣言は数えない(2.3 節)。他のファイルからの参照のあるモジュールは、すべての occurrence を数えると 512、可視性と `pub(in …)` を除くと 510、さらにキーワードを除くと 503、さらに `mod a;` を除くと 439(実装したもの。テストでないもの 428)である。修正前に `fan_in_files` が 0 でないモジュールは 441 だった。

## 2. 依頼で決まっていなかった判断

### 2.1 重なりの定義

範囲は jscpd の 1 始まり・両端を含む開始行と終了行で、2 つの範囲が 1 行でも共有すれば重なるとした。自分自身と重なる組の判定(1.1)も、2 つの報告の組を合わせる判定(1.2)も同じ定義である。

### 2.2 1 つの報告の中では合わせない、推移的にも合わせない

最初の実装は、両側で重なる組を報告を問わず推移的に 1 組にまとめた。この木では、1 つの normalized の報告の中で両側の重なる組が 109 対、exact の中で 8 対あり、推移的なまとめは似たブロックの族を鎖でつないだ。たとえば `crates/horizon-workspace/src/commands.rs` の normalized の 6 組(620-653 と 701-737、620-659 と 787-825、…、687-714 と 755-782)は、境の 1 行を共有するだけの組を介して 1 組になり、和集合の範囲 620-714 と 693-825 は自分自身と重なった。1 つの報告の中の重なる組は、ブロック 1 とブロック 3、ブロック 2 とブロック 4 のような別々の写しの組なので、合わせるのは exact と normalized の間だけにし、各 exact の組は 1 つの normalized の組にだけ付ける形にした(1.2)。この形では、合わせた後の範囲が自分自身と重なる組は 0 だった。

### 2.3 モジュールを名指さない occurrence を除く

依頼の規則は「モジュールの行はモジュール自身(その symbol)への参照だけを数える」である。rust-analyzer が同じ symbol に付ける occurrence には、モジュールを名指さないものが含まれていた(1.3 の表)。

- **可視性**(`pub(crate)`、`pub(super)`、`pub(in crate::a)`):項目の可視性の範囲で、モジュールへの依存ではない。
- **キーワード**(`crate::`、`super::`、`self::`):参照する側から見た相対の名前である。数えると、crate の根は自分の crate の `crate::` を使う全ファイルから、親のモジュールは `super::` を使う子のファイルから参照されることになる。ファイルのモジュールの範囲はそのファイルだけなので、子のファイルは親の範囲の外にある。
- **`mod a;` の宣言**:親が子を宣言する構造上の結び付きで、数えると親のファイルのあるモジュールはすべて fan-in が 1 以上になる(483 個)。

判別は occurrence の位置のソースの文字列で行う(occurrence の文字列が `crate`・`super`・`self` か、その前が `pub(in ` で始まる path か、行が `mod 名前;` の形か)。SCIP の列の位置と行の文字列の位置が合わない行では判別が外れうる。モジュールの名前と違う文字列を持つ occurrence を数えると、ほとんどは別名だった(`use horizon_acp as acp` の `acp` 440、`contract` を `agent` と呼ぶもの 320、`use crate::board::previews as board;` の `board` 32 など)。残りの 5 件は `:warn`・`eme::` のように位置のずれた文字列で、キーワードでないので名前として数えている。

### 2.4 1 つのパッケージの crate の根が symbol を共有する

rust-analyzer は symbol をパッケージで名付けるので、1 つのパッケージの target の根(ライブラリ、bin、example、結合テスト)は crate の根の symbol を共有する(この木で 15 個の symbol)。他の crate からの crate 名の参照が届くのはライブラリなので、symbol を共有する根のうちパスが `src/lib.rs` で終わるものがあればそれだけに数え、無ければ(bin と結合テストだけのパッケージ)共有する根のすべてに数える。最初の実装はすべてに数え、`horizon-agent` の bin と example の根が、ライブラリと同じ 58〜59 ファイルの fan-in を持った。

### 2.5 モジュールの fan-out は変えていない

依頼の規則は fan-in について書かれている。unit の段のモジュールの行の fan-out(`unit_fan_out`)は、前と同じくモジュールとその中身が参照するもの(ファイルの段の fan-out と同じ数)である。

### 2.6 出力の列と値

- `clone_pairs` の `source`(1 つの報告)は `sources`(一覧)になり、`report_pairs` が増えた。`pair_id` は合わせた組の最初の `report_pair_id` の順に振り直した。`duplication` の `source` 変数は「その報告が見つけた組」を選ぶ。
- `unit_clones` の `exact_pairs` と `normalized_pairs` は、両方の報告が見つけた組をそれぞれに数える(2 列の和が `clone_pairs` を超えることがある)。
- 和集合の範囲は、側の最も内側の unit を外側の unit に移すことがある(`clone_sides` は断片の全行を含む最も内側の unit を取る)。exact の組のうち 70 組で、側の unit が修正前の exact の行のものと違った。
- `coupling` の `contract_candidate` は file と crate の段では NULL、`kind` は unit の段だけに効く。`include_tests` は unit と file の段に効き(ファイルのモジュールがテストのファイルを除く)、crate の段には効かない。file の段は fan-in か fan-out のあるファイル、crate の段はすべての crate を返す。

## 3. 修正の前と後

測定は worker の作業ツリーで、`origin/code-quality-survey` の先端 adc765ca の使い捨ての複製(作業ツリーの `target/code-db/clone`)に対して行った。すべて `nice -n 19` で動かし、索引の後に rust-analyzer の `target/debug` を消した。修正前の数は複製の SQL のまま、修正後の数は修正した 4 つの SQL ファイルを複製に写して出した(索引は同じもの)。

```sh
nice -n 19 duckdb-ext/build.sh
nice -n 19 uv run --locked --script scripts/refactor-audit/run.py setup
nice -n 19 duckdb-ext/code-db/target/release/code-db index .
code-db query . duplication LIMIT=10   # coupling、intent も同じ
code-db sql . measure.sql              # unit_quality と unit_fan_in の書き出し、補助の集計(コミットしていない)
```

`index` は jscpd 2.5 s、rust-analyzer scip がルート 156.3 s、`duckdb-ext` 11.8 s、`duckdb-ext/code-db` 7.2 s、`duckdb-ext/host-check` 6.7 s、`preview-plugin` 55.8 s だった。unit は 9,335(テストでないもの 5,464)である。

### 3.1 `duplication`(既定、テストでない組)

行の数は 344 から 201 になった。

修正前:

| # | 側 a | 側 b | 行 / トークン | 報告 | same_unit |
| --- | --- | --- | --- | --- | --- |
| 1 | `src/board/previews.rs:636-819` `long_report` | 同 637-820 | 184 / 367 | normalized | true |
| 2 | `src/runtime/agent.rs:116-261` `agent` | `src/runtime/terminal.rs:83-227` `terminal` | 146 / 695 | normalized | false |
| 3 | `src/terminal/glyphs.rs:282-374` `box_draw_segments` | 同 283-375 | 93 / 1,289 | normalized | true |
| 4 | `duckdb-ext/scip-index-core/src/lib.rs:163-247` `KINDS` | 同 164-248 | 85 / 510 | normalized | true |
| 5 | `crates/horizon-workspace/src/commands.rs:3-85` `CommandId` | 同 4-86 | 83 / 70 | normalized | true |
| 6 | `src/workspace/render.rs:1115-1193` `render` | 同 1208-1286 | 79 / 382 | normalized | true |
| 7 | `src/terminal/glyphs.rs:554-618` `box_draw_geometry` | 同 606-670 | 65 / 170 | normalized | true |
| 8 | `crates/horizon-workspace/src/commands.rs:313-372` `core_commands` | 同 318-377 | 60 / 144 | normalized | true |
| 9 | `src/theme/accessors.rs:37-93` `accessors` | 同 42-106 | 57 / 146 | normalized | true |
| 10 | `crates/horizon-agent/src/frame/types.rs:20-73` `AgentFrameItem` | `src/agent/model/types.rs:146-165` `AgentFrameItem` | 54 / 72 | normalized | false |

修正後:

| # | 側 a | 側 b | 行 / トークン | 報告 | same_unit |
| --- | --- | --- | --- | --- | --- |
| 1 | `src/runtime/agent.rs:116-261` `agent` | `src/runtime/terminal.rs:83-227` `terminal` | 146 / 695 | exact, normalized | false |
| 2 | `src/workspace/render.rs:1115-1193` `render` | 同 1208-1286 | 79 / 382 | exact, normalized | true |
| 3 | `crates/horizon-agent/src/frame/types.rs:20-73` `AgentFrameItem` | `src/agent/model/types.rs:146-165` `AgentFrameItem` | 54 / 72 | normalized | false |
| 4 | `src/board/list.rs:516-568` `list` | `src/board/thread.rs:537-590` `thread` | 53 / 255 | exact, normalized | false |
| 5 | `src/theme/accessors.rs:69-121` `accessors` | 同 233-252 | 53 / 76 | normalized | true |
| 6 | `crates/horizon-agent/src/transcript/receipt.rs:54-98` `aggregate_receipt` | `src/agent/model/transcript.rs:364-396` `aggregate_receipt` | 45 / 224 | exact, normalized | false |
| 7 | `crates/horizon-workspace/src/commands.rs:200-241` `core_commands` | 同 251-293 | 42 / 179 | normalized | true |
| 8 | `crates/horizon-workspace/src/commands.rs:151-192` `core_commands` | 同 200-241 | 42 / 176 | normalized | true |
| 9 | `crates/horizon-agent/src/transcript/tool_call/files.rs:14-53` `affected_files` | `src/agent/model/transcript.rs:291-330` `affected_files` | 40 / 242 | exact, normalized | false |
| 10 | `crates/horizon-acp/src/ext.rs:196-234` `ext` | `crates/horizon-agent/src/contract.rs:435-484` `contract` | 39 / 150 | normalized | false |

修正前の 1・3・4・5・7・8・9 位(自分自身と重なる組)は消えた。修正後の `same_unit` の 4 件(2、5、7、8)はどれも重ならない 2 つの断片である。読み判定 B の 2 位(問題あり、小)と 6 位(問題あり、既知)は 1・2 位に、10 位は 3 位に残った。15・16 位だった `aggregate_receipt` は 1 行の 6 位になった。

### 3.2 `coupling`(unit の段、既定、テストでない unit)

修正前:

| # | unit | 種類 | fan-in(ファイル / crate / unit) |
| --- | --- | --- | --- |
| 1 | `crates/horizon-agent/src/contract.rs` `contract` | module | 144 / 2 / 1,225 |
| 2 | `crates/horizon-wire/src/session.rs` `session` | module | 107 / 4 / 876 |
| 3 | `crates/horizon-wire/src/session.rs` `SessionId` | type | 107 / 4 / 877 |
| 4 | `crates/horizon-agent/src/contract.rs` `Event` | type | 89 / 1 / 573 |
| 5 | `crates/horizon-agent/src/contract.rs` `ToolCallId` | type | 68 / 1 / 336 |
| 6 | `crates/horizon-wire/src/session.rs` `SessionId.new` | method | 65 / 4 / 405 |
| 7 | `crates/horizon-agent/src/contract.rs` `ToolCallRequest` | type | 65 / 1 / 298 |
| 8 | `crates/horizon-agent/src/contract.rs` `OccurrenceId` | type | 62 / 1 / 258 |
| 9 | `crates/horizon-agent/src/config.rs` `config` | module | 59 / 1 / 252 |
| 10 | `crates/horizon-agent/src/contract/tool_result.rs` `tool_result` | module | 59 / 1 / 261 |

修正後:

| # | unit | 種類 | fan-in(ファイル / crate / unit) |
| --- | --- | --- | --- |
| 1 | `crates/horizon-agent/src/contract.rs` `contract` | module | 156 / 2 / 476 |
| 2 | `crates/horizon-wire/src/session.rs` `SessionId` | type | 107 / 4 / 877 |
| 3 | `crates/horizon-agent/src/contract.rs` `Event` | type | 89 / 1 / 573 |
| 4 | `crates/horizon-agent/src/tools/mod.rs` `tools` | module | 80 / 1 / 233 |
| 5 | `crates/horizon-agent/src/contract.rs` `ToolCallId` | type | 68 / 1 / 336 |
| 6 | `crates/horizon-wire/src/session.rs` `SessionId.new` | method | 65 / 4 / 405 |
| 7 | `crates/horizon-agent/src/contract.rs` `ToolCallRequest` | type | 65 / 1 / 298 |
| 8 | `crates/horizon-agent/src/contract.rs` `OccurrenceId` | type | 62 / 1 / 258 |
| 9 | `crates/horizon-agent/src/lib.rs` `horizon-agent`(crate の根) | module | 59 / 2 / 186 |
| 10 | `crates/horizon-agent/src/contract/tool_result.rs` `ToolCallResult` | type | 59 / 1 / 256 |

`session` と `tool_result` の行(中身が 1 つの型のファイル)は上位から消え、`config` の行は 47 ファイルになって 10 位の外に出た。上位 10 件のモジュールは 4 件から 3 件、上位 50 件では 20 件から 20 件、上位 100 件では 38 件から 29 件になった。

上位に残ったモジュールの行は、名前を path に持つ参照の数である。`contract` は 144 ファイルから 156 ファイルに増えた。中の型を参照するファイルだけでなく、子のモジュールの項目を `crate::contract::tool_result::ToolCallResult` のように親の名前を通して参照するファイルも数えるからである。`tools`(6 → 80)と `persistence`(3 → 48)は、`mod.rs` の中身は小さく、子のモジュールへの path の要素として名指されている。crate の根の行(`horizon-agent` 59、`horizon-wire` 30 ファイル・9 crate)は、他の crate からの `horizon_agent::…` の path の数である。

`coupling level=file` の上位は修正前のファイルのモジュールの行と同じ数で、`contract.rs` 144 / 2 / 1,225、`session.rs` 107 / 4 / 876、`config.rs` 59、`contract/tool_result.rs` 59、`tools/state.rs` 41 と続く。`level=crate` の上位は `horizon-wire`(128 ファイル / 9 crate)、`horizon-agent`(56 / 2)、`horizon-sandbox`(35 / 4)である。

### 3.3 `intent`(既定、テストでない unit)

上位 10 件の並びは変わらず、9 位の `contract` の `fan_in_files` が 144 から 156 になっただけだった。行の数は 242 から 209 になり、モジュールの行は 43 から 10 になった。`fan_in_crates` が 0 になって外れたモジュールは、他の crate から中の型だけが(再公開を通して)参照されるファイル(`horizon-terminal-core` の `types/*.rs`、`horizon-workspace` の `types/*.rs` など)で、新しく入ったのは `persistence/projection/mod.rs` と `horizon-workspace` の `types.rs` の 2 個だった。

### 3.4 `unit_quality` の列

- 重複の列(`clone_pairs`、`exact_pairs`、`normalized_pairs`)は 473 行で変わった。`clone_pairs` が変わったのは 459 行で、141 行が 0 になり、0 から増えた行は無い。`clone_pairs` の合計は 2,295 から 1,621 になった。`exact_pairs` は 138 行、`normalized_pairs` は 173 行で変わった(`exact_pairs` は自分自身と重なる exact の 1 組と、和集合の範囲で側の unit が移った組による。2.6 節)。
- fan-in の列(`fan_in_files`、`fan_in_crates`)はモジュールの 293 行だけで変わり(増えたもの 45、減ったもの 248)、モジュールでない unit の行と fan-out の列は 1 行も変わらなかった。

## 4. 確かめたこと

- `check.py fixture` の段を足した。exact の報告に同じファイルの重ならない 2 つの断片(`lib.rs` 10-14 と 40-44、ファイルのモジュールの中の写しとして残る)を、normalized の報告に `Square` の `area` の本体が 1 行下の自分自身と一致する組(除かれる)と、2 つの `area` の本体を 1 行長く、側を逆にした組(exact の組と 1 組になり、`sources` が `[exact, normalized]`、範囲が和集合になる)を足した。`clone_report_pairs`、`clone_pairs`、`duplication` の `source=normalized`、`coupling` の file と crate の段の行を期待値に足し、`unit_quality`、`duplication`、`coupling`、`intent` の期待値を直した。`assert_counting_rules` が規則を 1 つずつ名指して確かめる:除かれた組、残った同じ unit の組、合わさった組、crate の根 `shapes` の unit の段の fan-in(`main.rs`・`fixtures.rs`・`it.rs` の 3 ファイル。`crate::` と `pub mod rect;` は数えない)と、そのファイルの段の合算(5 ファイル)。期待値は fixture の文字列と照らして手で確かめた。
- `code-db` の end-to-end テストは、偽の jscpd が同じ組を両方の報告に書くので、`duplication` が 2 行を返すことを期待していた。1 行(`sources` が `[exact, normalized]`、側の並べ直しで側 a が `src/util.rs` の `helper`)を期待するように直した。

### 4.1 結果

- `uv run --locked --script duckdb-ext/sql/check.py fixture`:`fixture: ok`。
- `duckdb-ext/check.sh`:終了コード 0(書式、lint、全メンバーのテスト、拡張のビルド、host-check、`code-db` の end-to-end テストを含む)。最初の実行では end-to-end テストが側の順(上のとおり)で落ち、期待値を直して通った。
- `check.py refresh`(複製に対して):キャッシュのすべての層とすべてのクエリが live と一致した。
- `coupling level=file` と前のファイルのモジュールの行の照合:520 ファイルで差 0(1.3)。
