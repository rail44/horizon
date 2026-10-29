# 現在の木の軸の読み判定・B:重複・結合・参照ゼロ・循環(2026-10-30)

SCIP で組み直したコードデータベース([設計文書](../code-database-design.md)の「Rebuilt on SCIP」「Unit syntax trees」「Quality axes, settings and the launcher」)の、現在の木の軸のうち 4 つ(`duplication`、`coupling`、`unreferenced`、`module_cycles`)について、HEAD での上位の候補を読んで判定し、3 回目の読み判定([記録](code-db-dogfood-reading-3-2026-10-28.md)、以下「3 回目」)が `問題あり` とした障害に、今のどのクエリが届くかを追った。判定と記述だけを行い、ソースコード、SQL、Rust は変えていない。残りの 4 軸(`size`、`complexity`、`interface`、`intent`)は別の読み手が並行して判定しており、ここでは判定しない(追跡の表では順位だけを引く)。`layer_violations` は層が宣言されていないので対象外である(`board_flow` の問いは未決のまま)。

目的は、オーナーの言葉では「refactor auditの仕組みをよりよくしたい」(2026-10-24)であり、依頼文によれば、構造と依存のグラフをつかんで候補を見つけ、判定すること(2026-10-25)である。品質は「継続的にメンテナンスをする上での障害の少なさや機能の足しやすさ」で、好みではなく、重複と置き場所は構造上の性質である(2026-10-28/29)。この評価の形(軸ごとに上位 10 件を下の規則で判定し、軸ごとの精度と誤報の機構、既知の障害の追跡を出す)は、オーナーが 2026-10-30 に「はい。お願いします。」と承認した。

前提として読んだのは、設計文書の上記の節、[軸の調査](code-quality-axes-2026-10-29.md)(以下「軸の記録」)、`duckdb-ext/sql/queries/` のすべてのクエリの見出し、`duckdb-ext/sql/README.md`、3 回目の記録(9 節の引き継ぎの一覧、`問題あり` 14 件と 9 個の原因)である。3 回目の判定の規則は、下の規則に置き換えた。

## 要約

- **軸ごとの判定**(`問題あり` / `問題なし` / `判断できない`、上位 10 件):`duplication` 2 / 8 / 0、`coupling` 1 / 9 / 0、`unreferenced` 0 / 10 / 0、`module_cycles` 2 / 7 / 1。precision@10 はそれぞれ 0.2、0.1、0.0、0.2 である。
- **新しい原因は 1 つ**:2 つの daemon のクライアントの接続ループ(`src/runtime/agent.rs:122-244` と `src/runtime/terminal.rs:87-210`、約 120 行)が写しで、再接続の方針を変えると 2 箇所を同じに直す必要がある(`duplication` 2 番、`問題あり(小)`)。3 回目の原因 8(`AgentdHandle` と `TerminaldHandle` の同じ操作)と同じ系統だが、別の箇所である。残りの `問題あり` 4 件は既知の原因(リストのモーダルの写し、`RigAgentConfig`、`start_approval_gate`、`list_model_ids`)だった。
- **誤報の主な機構**は軸ごとに 1 つずつはっきりしている。`duplication` は行数の降順なので、表やリストの自分自身とのずれた一致(上位 10 件のうち 8 件が `same_unit`)が上に来る。`coupling` はファイルのモジュールが中身の参照を合算し、テストからの参照も数えるので、契約の型と ID の newtype とそのファイルが上を占め、派生の印 `contract_candidate`(3 crate 以上)は上位 10 件の型 5 件のうち `SessionId` の 1 件にしか付かない。`unreferenced` は SCIP に参照が無いだけの項目(書式文字列、serde の属性の文字列、`macro_rules!` の本体、wasm でしか有効でない cfg の利用側)が上位 10 件のすべてを占めた。`module_cycles` は、1 つの型の状態を兄弟のファイルに分けた形(状態の型が子の型をフィールドに持ち、子のファイルがその型の impl を足す)を構造上の辺として除けず、判定した 3 モジュールの循環 6 個のうち 2 個がこれで、ほかの 3 個も型とその置き場所の近い形だった(すべて `問題なし`)。
- **最大の循環の 25 → 40**(設計文書が「whether that count fits the purpose is not yet judged」と書く点)は、`persistence/event_log/mod.rs` のファイル直下の `pub use` 1 本(`upgrade_conversation_records` の再公開)が、rig の 5 モジュールと persistence・`live.rs` などの 9 モジュールを judge/tools の循環に結んだものだった。この辺を除くと 26 と 5 の 2 つに分かれる。
- **追跡**:3 回目の引き継ぎの 14 件(9 節)のうち、既定(本番のコード)のどれかのクエリの上位 10 件に入るのは 4 件(`start_approval_gate` 循環 1、リストのモーダル `size` 3・`duplication` 6、`list_model_ids` 循環 4、`RigAgentConfig` は `coupling` 9 のファイルのモジュール経由)である。テストの切り替えを入れると、real-git のテストの写しが `duplication`(`include_tests := true`)の 7 位に、写しの隣の断片で届く。残りは 13〜339 位か、テストの unit なので本番の一覧に出ない。
- **ソースは 3 回目から変わっていない**(`git diff --stat b3b94fc1 d6299e9b -- src crates preview-plugin` が空)。3 回目が引く行番号はすべて有効で、原因はどれも直っていない。

## 方法

### 環境と手順

作業は worker の作業ツリー(ブランチ `worktree-agent-ab307230ab8a5bdd0`、`origin/code-quality-survey` の先端 d6299e9b に合わせた)で行った。測定は使い捨ての複製(作業ツリーの `target/code-db/repo`、`git clone --no-checkout` の後 d6299e9b を checkout)で行い、作業ツリーのファイルは測定に使っていない。機械はオーナーの対話用デスクトップで、すべて `nice -n 19` で動かした。

```sh
nice -n 19 duckdb-ext/build.sh                                         # 拡張 2 つと code-db
nice -n 19 uv run --locked --script scripts/refactor-audit/run.py setup  # jscpd 5.3.1
nice -n 19 code-db index target/code-db/repo
#   jscpd(537 ファイル)5.3 s、rust-analyzer scip:ルート 232.3 s、duckdb-ext 13.4 s、
#   duckdb-ext/code-db 18.4 s、duckdb-ext/host-check 11.9 s、preview-plugin 88.1 s
rm -rf target/code-db/repo/{,duckdb-ext/,duckdb-ext/code-db/,duckdb-ext/host-check/,preview-plugin/}target/debug
nice -n 19 code-db query target/code-db/repo QUERY FORMAT=csv LIMIT=10000000
```

`.code-db.toml` は無いので、`scope.exclude` も層も無い既定の状態である(`duckdb-ext/` 自身のコードも数に入る)。クエリはすべて既定の変数で走らせた。テストの切り替えのあるクエリ(`duplication`、`coupling`、`size`、`complexity`、`interface`、`intent` の `include_tests`)は既定の `false`(本番のコード)で判定し、追跡のときだけ `true` も走らせた。`unreferenced` は既定(`include_structural := false`)で判定し、構造上の参照ゼロの行も追跡に使った。`module_cycles` は既定の `cycle_edges := 'collapsed'` で判定し、`raw` も書き出した。各クエリはキャッシュの上で 2〜6 s だった。

行の数は次のとおり。`duplication` 344 組(テストを含めて 1,529)、`coupling` 5,376 行(9,207)、`unreferenced` 27 行(構造上のものを含めて 3,105。`test` 2,827、`trait_impl` 225、`entry_point` 11、`trait_item` 8、`crate_root` 3)、`module_cycles` 24 個(辺 193)、`module_graph` 3,811 辺、`layer_violations` 0 行。

上位 10 件は、クエリの出力の順のままの最初の 10 行である(`duplication` は組、`coupling` と `unreferenced` は unit、循環はモジュールの数の降順で、同じ数なら出力の順。3 モジュールの循環は 7 個あり、出力の順で `providers/mock.rs` から `src/board/events.rs` までの 6 個を取り、11 番目の `src/theme/palette.rs` は外した)。追跡は、`queries/` のすべてのクエリを大きい LIMIT で書き出し、原因の unit(パスと名前、名前の無い原因は行の範囲に重なる関数と method)が現れる最初の行を探した(`target/code-db/q/track.py`、コミットしていない)。循環の中の辺の重みを見るために、`module_cycles` の出力から強連結成分を Python で計算し直した(`target/code-db/q/scc.py`、同じくコミットしていない)。

### 判定の規則

依頼文の規則に従った。**問題あり**:その unit(モジュール、組、循環)が軸の性質をどう損なうかと、それが次の変更や機能の追加のどこで障害になるかを名指しでき、`ファイル:行` で示せるもの。**問題なし**:そうした障害を名指しできないもの(それでも数が高い理由を書く。皆が使う契約の型、テストの補助、コンパイラが強制する網羅的な `match`、親子のモジュールなど)。**判断できない**:書かれていない意図に依存するもの(問いを書く)。好みでは判定しない。コードは AI が書くので、小さな重複や並べ方の好みは、変更の費用にならないかぎり障害としない。

前の判定がある候補は、同じ箇所について同じ理由のものを引き、引いた箇所が 3 回目のコミット(b3b94fc1)から変わっていないことを確かめた(上のとおり差分は無い)。循環は、前の判定が指した辺が今の循環に残っているかを辺の一覧で確かめた。

---

## 1. `duplication`(組の上位 10 件、本番のコード)

`duplication.sql` は jscpd の組を、両側の断片を含む最も内側の unit とともに、行数、トークン数の降順で返す。`same_unit` は 2 つの断片が同じ unit の中にあること、`conversion_pair` は両側が変換の impl で結ばれた 2 つの型にあることを示す。上位 10 件はすべて normalized(識別子とリテラルを区別しない)の組で、`conversion_pair` はどれも false だった。

| # | 側 a | 側 b | 行 / トークン | same_unit | 判定 |
| --- | --- | --- | --- | --- | --- |
| 1 | `src/board/previews.rs:636-819` `long_report` | 同 637-820 | 184 / 367 | true | 問題なし |
| 2 | `src/runtime/agent.rs:116-261`(ファイルのモジュール `agent`) | `src/runtime/terminal.rs:83-227`(`terminal`) | 146 / 695 | false | 問題あり(小) |
| 3 | `src/terminal/glyphs.rs:282-374` `box_draw_segments` | 同 283-375 | 93 / 1,289 | true | 問題なし |
| 4 | `duckdb-ext/scip-index-core/src/lib.rs:163-247` `KINDS` | 同 164-248 | 85 / 510 | true | 問題なし |
| 5 | `crates/horizon-workspace/src/commands.rs:3-85` `CommandId` | 同 4-86 | 83 / 70 | true | 問題なし |
| 6 | `src/workspace/render.rs:1115-1193` `Render for WorkspaceShell.render` | 同 1208-1286 | 79 / 382 | true | 問題あり(既知) |
| 7 | `src/terminal/glyphs.rs:554-618` `box_draw_geometry` | 同 606-670 | 65 / 170 | true | 問題なし |
| 8 | `crates/horizon-workspace/src/commands.rs:313-372` `core_commands` | 同 318-377 | 60 / 144 | true | 問題なし |
| 9 | `src/theme/accessors.rs:37-93`(ファイルのモジュール `accessors`) | 同 42-106 | 57 / 146 | true | 問題なし |
| 10 | `crates/horizon-agent/src/frame/types.rs:20-73` `AgentFrameItem` | `src/agent/model/types.rs:146-165` `AgentFrameItem` | 54 / 72 | false | 問題なし(引用) |

### 判定

1. **`long_report` — 問題なし。** プレビュー用の見本の文書で、`concat!` に日本語の文字列リテラルを 1 行ずつ並べたもの(`src/board/previews.rs:633-820`)。normalized ではリテラルの行がすべて同じトークン列になるので、同じ並びを 1 行ずらした自分自身と一致する。写された処理は無い。
2. **2 つの daemon のクライアントの接続ループ — 問題あり(小)。** `agent.rs` の `spawn`(122-244)と `terminal.rs` の `spawn`(87-210)は、tokio の runtime を立て、接続か起動を再試行し、`StreamEnd` の 7 つの腕で、hello 前の転送の失敗を指数のバックオフで再試行し、`SILENCE_MISMATCH_THRESHOLD` 回の無応答で回復に落とし、世代の不一致と版の拒否を回復に回し、致命的な失敗と取り消しで止まる、という同じ約 120 行の状態機械である。違いは daemon の名前の文字列、経路の表の型(`AgentRoutes` / `TerminalRoutes`)、回復の関数(`recover_generation_mismatch` / `recover_stale_terminald`)だけで、どれも引数にできる。`terminal.rs` のモジュール文書は「Structurally the twin of [`super::agent`], with two deliberate differences」と書き、その 2 つ(drain が破壊的であること、hello の後の skew の確かめ)は回復の関数と `run_stream` の側にあって、このループには無い(`terminal.rs:1-20`)。`docs/runtime-crate-alignment-design.md` の 4 番(2026-08-01)は 2 つのクライアントの写しを `horizon-wire` に出し、意図して分けたもの 4 つ(2 つの drain、SIGTERM、送信の pump、状態の表)を挙げているが、このループはそのどちらにも入っていない。障害は、再接続の方針(バックオフの上限、無応答の閾値の扱い、新しい `StreamEnd` の腕の扱い)を変えるとき、2 箇所を同じに直さなければならず、`StreamEnd` の網羅性はコンパイラが確かめるが、2 つの腕の中身が食い違うことは止めないことである。履歴にも跡がある:`terminal.rs` の側ができた a4fd9605(2026-07-30)の後、agent の側だけが 8d8cdd0e、9763a937、8c52fe5a で変わり、8c52fe5a(2026-09-27)は `VersionRejected` の腕を `GenerationMismatch` と同じ腕にまとめて回復を関数に出し、terminal の側の形に手で寄せ直した(`git log -L` で確かめた。この組の断片は `agent.rs:116-261`)。3 回目の原因 8(`AgentdHandle` と `TerminaldHandle` の `begin_reload`・`stop_and_wait`・`register_*`、`src/runtime/mod.rs`)と同じ「2 つの runtime のクライアントの写し」の系統だが、別の箇所なので新しい原因として数える。断片が `Op` の終わりから回復の関数の始めまでにまたがるので、側はファイルのモジュールに解決している(機構 D3)。
3. **`box_draw_segments` — 問題なし。** Unicode の罫線の文字ごとに 4 方向の線の種類を返す対応表(`src/terminal/glyphs.rs:261-391`)。1 行ずつのほぼ同じ形の腕が、ずらした自分自身と一致した。
4. **`KINDS` — 問題なし。** SCIP の `SymbolInformation.Kind` の値と名前の表(`duckdb-ext/scip-index-core/src/lib.rs:161-248`)。3 番と同じ。
5. **`CommandId` — 問題なし。** ユーザーに見える操作の enum の variant の並び(`crates/horizon-workspace/src/commands.rs:1-86`)。識別子を区別しないと 1 行ずらして一致する。3 回目が `unit_fan_in_churn` で判定した理由(AGENTS.md の「Operations go through the command model」が決めた形)と同じ型である。
6. **`render` の 4 つのモーダルの重ね描き — 問題あり(3 回目 1 節の 8 番を引く)。** 断片 1115-1193 と 1208-1286 は、パレット・ビューの選択とセッションの管理・モデルの選択の重ね描きの組で、3 回目が `render.rs:1112-1287` の 4 つの約 40 行として判定し、1c27f1ab が同じ修正を 4 箇所に当てたことを書いた箇所そのものである。
7. **`box_draw_geometry` — 問題なし。** 4 方向(上・右・下・左)それぞれの線の種類(`Empty`・`Light`・`Heavy`・`Double`)で矩形を積む 4 つの `match`(`src/terminal/glyphs.rs:428-722`)。座標の組が方向ごとに違うのが中身で、識別子を正規化すると一致する。線の種類を足すと 4 つの `match` に腕が要るが、それはコンパイラが網羅性で直させる。
8. **`core_commands` — 問題なし。** 各コマンドの表題・分類・説明を並べた `CommandSpec` のデータの表(`crates/horizon-workspace/src/commands.rs:144-388`)。3 番と同じ。
9. **`theme/accessors.rs` — 問題なし。** 色の役割ごとの 1 行の関数(`pub fn danger() -> Hsla { packed_hsla(scheme().danger) }` など)が並ぶファイル(`src/theme/accessors.rs:37-121`)。役割の名前で呼ぶための薄い関数で、役割を足すと 1 つ増やす形が意図である。
10. **2 つの `AgentFrameItem` — 問題なし(2 回目 3 節の 8 番を引く)。** エージェントの内部の frame とシェルの ACP の frame で、シェルが ACP だけを読む分割(`docs/agent-runtime-split-design.md`)の写しである(`crates/horizon-agent/src/frame/types.rs:15-73`、`src/agent/model/types.rs:145-165`)。表示する項目を足すと 2 つの frame に 1 つずつ足すのは、境界の決まった費用である。

## 2. `coupling`(fan-in の上位 10 件、本番のコード)

`coupling.sql` は unit ごとに、それが表すもの(`unit_scope`:自分、中に含むもの、型ならその impl の member)を参照する他のファイル・crate・unit の数を出し、他のファイルの数の降順に並べる。テストの unit は行から除かれるが、テストからの参照は数える。

| # | unit | 種類 | loc | fan-in(ファイル / crate / unit) | fan-out(ファイル) | contract_candidate | 判定 |
| --- | --- | --- | ---: | --- | ---: | --- | --- |
| 1 | `crates/horizon-agent/src/contract.rs` `contract` | module | 1,195 | 144 / 2 / 1,225 | 7 | false | 問題なし |
| 2 | `crates/horizon-wire/src/session.rs` `session` | module | 38 | 107 / 4 / 876 | 0 | false | 問題なし |
| 3 | `crates/horizon-wire/src/session.rs` `SessionId` | type | 10 | 107 / 4 / 877 | 0 | true | 問題なし |
| 4 | `crates/horizon-agent/src/contract.rs` `Event` | type | 153 | 89 / 1 / 573 | 3 | false | 問題なし(引用) |
| 5 | `crates/horizon-agent/src/contract.rs` `ToolCallId` | type | 2 | 68 / 1 / 336 | 0 | false | 問題なし |
| 6 | `crates/horizon-wire/src/session.rs` `SessionId.new` | method | 3 | 65 / 4 / 405 | 0 | false | 問題なし |
| 7 | `crates/horizon-agent/src/contract.rs` `ToolCallRequest` | type | 8 | 65 / 1 / 298 | 0 | false | 問題なし(引用) |
| 8 | `crates/horizon-agent/src/contract.rs` `OccurrenceId` | type | 30 | 62 / 1 / 258 | 0 | false | 問題なし |
| 9 | `crates/horizon-agent/src/config.rs` `config` | module | 1,082 | 59 / 1 / 252 | 4 | false | 問題あり(既知、中身経由) |
| 10 | `crates/horizon-agent/src/contract/tool_result.rs` `tool_result` | module | 118 | 59 / 1 / 261 | 1 | false | 問題なし |

参照する側を本番とテストに分けると(参照のあるファイルの数。1 つのファイルが両方を持つことがある)、`SessionId.new` は本番 4 / テスト 62、`OccurrenceId` 15 / 52、`ToolCallId` 22 / 56、`contract` 102 / 98、`config` 41 / 34、`tool_result` 30 / 39、`SessionId` 74 / 74 だった。

### 判定

1. **`contract` モジュール — 問題なし。** `horizon-agent` の契約の語彙のファイル(イベントログの `Event`、コマンド、ツール呼び出しの型、ID の newtype)で、その fan-in は中の型の fan-in の合算である(同じファイルの型の fan-in は `Event` 89、`ToolCallId` 68、`ToolCallRequest` 65、`OccurrenceId` 62、`ToolCallIdentity` 54 …)。型を変えるとコンパイラが使う側を直させる。3 回目が fan-in の 1・3・4・5・7 番を `問題なし` とした理由(機構 M3)と同じで、ファイルの行は新しい情報を足さない。
2. **`horizon-wire` の `session` モジュール — 問題なし。** `SessionId` の 1 つだけを持つ 38 行のファイルで、3 番の行と同じ数である(ファイルのモジュールが中身を合算するので、ほぼ同じ行が 2 つ出る。機構 C1)。
3. **`SessionId` — 問題なし。** UUID の newtype(`crates/horizon-wire/src/session.rs:15-38`)。モジュール文書が「the id is the only part of it any two runtimes ever need to agree on」と書く、意図した共有の基盤である。3 回目が impl を `問題なし` とした理由と同じ。
4. **`Event` — 問題なし(1 回目の 1 番を引く)。** イベントログの語彙で、網羅的な `match` への伝播をコンパイラが強制する(`contract.rs:273-423`)。
5. **`ToolCallId` — 問題なし。** `pub struct ToolCallId(pub String)` の 1 行の newtype(`contract.rs:32-33`)。ツール呼び出しの id を皆が使うことを示すだけである。
6. **`SessionId.new` — 問題なし。** `Uuid::new_v4()` を包む 3 行(`session.rs:27-29`)。参照のある 65 ファイルのうち本番は 4 で、残りはテストがセッションの id を作る箇所である(テストからの参照を数える規則による。機構 C2)。
7. **`ToolCallRequest` — 問題なし(1 回目の 6 番を引く)。** 7 行のデータ型(`contract.rs:871`)。
8. **`OccurrenceId` — 問題なし。** ツール呼び出しの出現ごとの id の newtype で、30 行のほとんどは、なぜ provider の `call_id` とは別の鍵が要るかの文書である(`contract.rs:35-66`)。fan-in 62 のうち本番は 15 ファイル。
9. **`config` モジュール — 問題あり(既知の原因 4 に、中身を通して届いた)。** `horizon-agent` の設定の型のファイルで、fan-in 59 は 15 ほどの設定の型の合算である(中の最大は `RigAgentConfig` 25、`AgentToolsConfig` 17、`AgentConfig` 14、`ProviderKind` 10 …)。障害は中の `RigAgentConfig` にある:接続、ループの調整値、セッションごとの値の 3 種類を 1 つの構造体が持ち(`config.rs:519-610`)、そのどれか 1 つだけを読む 25 ファイルが構造体全体に依存する。セッションごとの値を足すと、プロセスの設定の複製を書き換える箇所(`providers/rig/session/mod.rs:84-91`)に 1 つ足すことになる(1 回目の 5 番、3 回目の fan-in 6 番の判定)。ただしこの行の数はファイルの合算で、`RigAgentConfig` を指してはいない。読み手は中を開いて原因を探すことになる。`RigAgentConfig` 自身は `coupling` の 48 位である。
10. **`tool_result` モジュール — 問題なし。** `ToolCallResult` と `ToolOutcome` のファイル(`contract/tool_result.rs`)。`ToolCallResult` 自身が 11 位で同じ数を持つ(機構 C1)。2 回目の判定どおり、wire のデータ型で変更はコンパイラが使う側に強制する。

`contract_candidate`(3 crate 以上から参照される型)が付いたのは上位 10 件のうち `SessionId` だけだった。`Event`、`ToolCallId`、`ToolCallRequest`、`OccurrenceId` は、`horizon-agent` の外では `horizon-agentd` と、シェルのテストの 1〜2 crate からしか参照されないので、印が付かない。3 回目が契約の型として `問題なし` にした 6 件(`Event`、`ToolCallResult`、`Command`、`ToolCallRequest`、`MessageRole`、`CommandId`)はどれも印が無い(機構 C3)。

## 3. `unreferenced`(参照ゼロの上位 10 件)

`unreferenced.sql` は、自分の symbol に参照の occurrence がどの索引にも無い unit を、構造上の理由(テスト、トレイトの impl、`main`、crate の根、トレイトの項目)を除いて、loc の降順で返す。既定で 27 行である。

| # | unit | 種類 | loc | 可視性 | 判定 |
| --- | --- | --- | ---: | --- | --- |
| 1 | `duckdb-ext/code-db/src/main.rs` `USAGE` | constant | 28 | private | 問題なし |
| 2 | `crates/horizon-agent/src/tools/input/mod.rs` `ToolInput.validate` | method | 26 | private | 問題なし |
| 3 | `crates/horizon-cli/src/cli.rs` `USAGE` | constant | 21 | private | 問題なし |
| 4 | `crates/horizon-config/src/lib.rs` `deserialize_lenient_f64` | function | 21 | private | 問題なし |
| 5 | `crates/horizon-config/src/lib.rs` `deserialize_lenient_model_pair` | function | 14 | private | 問題なし |
| 6 | `src/input_trace.rs` `write_line` | function | 14 | pub(crate) | 問題なし |
| 7 | `crates/horizon-cli/src/board.rs` `BOARD_USAGE` | constant | 13 | private | 問題なし |
| 8 | `src/preview/sample.rs` `PreviewIcons` | type | 13 | pub | 問題なし |
| 9 | `src/preview/schema.rs` `PreviewHostCaller.theme` | method | 12 | pub | 問題なし |
| 10 | `src/preview/schema.rs` `PreviewHostCaller.selected_preview` | method | 12 | pub | 問題なし |

### 判定

どれも使われている。参照が SCIP に無い理由ごとにまとめる。

- **書式文字列の中の名前(1・3・7)。** `USAGE` は `eprint!("{USAGE}")`(`duckdb-ext/code-db/src/main.rs:79`)、`horizon-cli` の `USAGE` は `write!(f, "{}\n\n{USAGE}", self.0)`(`crates/horizon-cli/src/cli.rs:152`)、`BOARD_USAGE` は `writeln!(stdout, "{BOARD_USAGE}")` と `format!(…{BOARD_USAGE})`(`crates/horizon-cli/src/board.rs:45`、300)で使われる。クエリの見出しが挙げる「a name used only inside a format string such as "{USAGE}"」そのものである。27 行のうち 11 位以下の `SKEW_REMEDY`(`src/runtime/terminal.rs:227`、425)と `RESULT_CALL_JOIN`(`persistence/projection/duckdb/query.rs:509`、605)も同じ。
- **serde の属性の文字列(4・5)。** `#[serde(deserialize_with = "deserialize_lenient_model_pair")]`(`crates/horizon-config/src/lib.rs:55`、61、69)と `deserialize_with = "deserialize_lenient_f64"`(443)。見出しが挙げる形である。
- **`macro_rules!` の本体(2・6)。** `ToolInput.validate` は同じファイルの宣言的マクロの本体から `input.validate()` で呼ばれる(`crates/horizon-agent/src/tools/input/mod.rs:34`)。`write_line` はトレースのマクロの本体から `$crate::input_trace::write_line(…)` で呼ばれる(`src/input_trace.rs:85`)。
- **wasm でしか有効でない利用側と、マクロが生成した項目(8・9・10)。** `PreviewIcons` は `gpui_kit_assets::icon_assets!` が生成する型(`src/preview/sample.rs:26-37`)、`PreviewHostCaller.*` は `#[interface]` 属性マクロがトレイトから生成する呼び出し側の method(`src/preview/schema.rs:11-22`。同じ行の範囲の 6 つの method が 9・10 位と 11〜14 位に並ぶ)である。使う側は `src/preview/guest.rs`(`PreviewIcons` は 14 行と 100 行、`theme_json` は 53 行、`selected_preview` は 165 行)で、`src/preview/mod.rs:22-23` の `#[cfg(target_family = "wasm")] pub mod guest;` によりホストの target では無効なので、rust-analyzer の索引に入らない。この機構はクエリの見出しの列挙に無い(機構 U2)。

上位 10 件の外で、読んだかぎり本当に呼び出し元が無いものが 2 つあった(判定の対象外):`Appender.set_session_context`(`crates/horizon-agent/src/persistence/event_log/appender.rs:55`、`pub`、16 位)と `known_security_services`(`crates/horizon-sandbox/src/lib.rs:104`、`pub`、18 位)。どちらもリポジトリの中に名前の出現が定義の 1 つしか無い。並びが loc の降順なので、小さい本物の死んだ `pub` 項目は、大きな文字列の定数の下に来る(機構 U3)。

## 4. `module_cycles`(大きい 10 個)

| # | 循環(最小の要素) | crate | モジュール | 辺 | unit の辺 | 3 回目 | 判定 |
| --- | --- | --- | ---: | ---: | ---: | --- | --- |
| 1 | `crates/horizon-agent/src/judge/handle.rs` | horizon-agent | 40 | 101 | 405 | 1 番(25) | 問題あり(小)(引用、拡大は 4.1) |
| 2 | `crates/horizon-agentd/src/board_flow/operations.rs` | horizon-agentd | 9 | 16 | 52 | 2 番(9) | 判断できない(引用) |
| 3 | `crates/horizon-agent/src/transcript/diff.rs` | horizon-agent | 7 | 13 | 51 | 3 番(7) | 問題なし(引用) |
| 4 | `crates/horizon-agent/src/auxiliary.rs` | horizon-agent | 5 | 8 | 20 | 5 番(3、`config.rs`) | 問題あり(小)(引用) |
| 5 | `crates/horizon-agent/src/providers/mock.rs` | horizon-agent | 3 | 4 | 11 | 上位外 | 問題なし(1 回目・2 回目を引く) |
| 6 | `crates/horizon-agent/src/providers/rig/session/continuation.rs` | horizon-agent | 3 | 5 | 28 | 上位外(判定なし) | 問題なし |
| 7 | `crates/horizon-agentd/src/session/attachment.rs` | horizon-agentd | 3 | 4 | 27 | 上位外 | 問題なし |
| 8 | `crates/horizon-board/src/store/logd.rs` | horizon-board | 3 | 4 | 9 | 上位外 | 問題なし |
| 9 | `crates/horizon-workspace/src/mode.rs` | horizon-workspace | 3 | 4 | 17 | 上位外 | 問題なし |
| 10 | `src/board/events.rs` | horizon | 3 | 4 | 10 | 上位外 | 問題なし |

3 回目の 4 番(`rig/clearing.rs` ほか 5 モジュール、`問題なし`)は、1 番の 40 モジュールの中に入った(4.1)。3 回目の 5 番(`config.rs`・`model_limits.rs`・`model_catalog.rs` の 3 モジュール)は、`auxiliary.rs` と `decision/config.rs` を加えた 5 モジュールになった。`violating_edges` は層が無いのですべて 0 である。

### 4.1 1 番の 40 モジュールの中身

40 モジュールは、judge の 3 つ(`handle.rs`、`mod.rs`、`record.rs`)、`policy.rs`、tools の 23、`live.rs`、persistence の 6(`event_log/{appender,mod,writer}.rs`、`projection/duckdb/{import,mod,query}.rs`)、rig の 5(`clearing.rs`、`completion.rs`、`conversation/{projection,upgrade}.rs`、`mapping.rs`)である。辺を外して強連結成分を求め直すと(`scc.py`):

- `persistence/event_log/mod.rs → providers/rig/*` を外すと、26 モジュール(judge の 2 つ、`policy.rs`、tools の 23)と 5 モジュール(rig、3 回目の 4 番と同じ顔ぶれ)の 2 つに分かれる。`judge/record.rs`、`live.rs`、persistence の 6、`tools/transition.rs` の 9 モジュールは循環から外れる。
- judge から tools への辺(4 本)を外すと、36 モジュールが残る。両方を外すと、`policy.rs` と tools の 21 モジュールと rig の 5 モジュールが残る。

`persistence/event_log/mod.rs → providers/rig/conversation/upgrade.rs` は unit の辺 1 本で、symbol は `upgrade_conversation_records` である。これはファイル直下の `pub use crate::providers::rig::conversation::{interrupted_conversation_calls, upgrade::upgrade_conversation_records};`(`crates/horizon-agent/src/persistence/event_log/mod.rs:23-25`)で、SCIP の組み直しで「file-level `pub use` … now belong to the file module」となった数え方による辺である(設計文書「Rebuilt on SCIP」)。実際に関数を呼ぶのは子の `event_log/offline.rs:27`(オフラインの v3 → v4 の変換)だが、子から親への辺は構造上の辺として除かれるので、再公開の 1 行が無ければ persistence は循環に入らない。逆向きは rig の `conversation/upgrade.rs` から `event_log/mod.rs` の `Record` への辺 2 本、rig から tools への辺(`completion.rs`・`mapping.rs` から `catalog.rs` の `Definition`)、tools・judge・`live.rs` から persistence への辺(`tools/recall` の DuckDB の問い合わせ、judge の記録、`live.rs` の `Appender`)である。

### 判定

1. **judge・`policy.rs`・tools を中心とする 40 モジュール — 問題あり(小)(1 回目の 1 番、3 回目の 1 番を引く)。** judge から tools への辺は、`start_approval_gate`(`judge/mod.rs:301-345`)の `ToolCompletion`・`ToolSessionState`・`live_frame_for_session` と、ツールの説明の `definitions`・`description`・`id`、`JudgeHandle` の `ToolCompletion`・`ApprovalJudged` で、前の判定が指した辺そのものが残っている。拡大した 15 モジュールについて:rig の 5 モジュールは 3 回目の 4 番の `問題なし`(残った循環を閉じる辺は `conversation/upgrade.rs` の `restore_tool` から `completion.rs` の `replay_safe_tool_arguments` への 1 本)を引く。persistence と rig を結ぶ 2 方向の辺(event log の v3 → v4 の変換が rig の会話の upgrade を使い、その upgrade が event log の `Record` を使う)は、変換を event log の側に置くのか rig の会話の側に置くのかの意図に依存し、書かれた決定を見つけられなかったので、この部分は判断しない(11 節の問い)。循環全体の判定は、名指しした judge → tools の辺の費用(1 回目の判定)で `問題あり(小)` のままとする。
2. **`board_flow/operations.rs` と `session/*` — 判断できない(1 回目の 4 番、3 回目の 2 番を引く)。** session から board_flow への辺は、前と同じ 2 本(`session/board.rs` → `operate`、`session/connection.rs` → `organizer_session`)だけだった。問いは 3 回目の 11 節のまま。
3. **`transcript/diff.rs` と `transcript/tool_call/*`・`receipt.rs` — 問題なし(3 回目 5 節の 3 番を引く)。** 辺は前と同じ形(`diff.rs` → `receipt.rs` の `CallClass`、`diff.rs` → `tool_call/view.rs`、`tool_call/util.rs` → `diff.rs` の `reconstruct_line_diff`)である。
4. **`auxiliary.rs`・`config.rs`・`decision/config.rs`・`model_limits.rs`・`model_catalog.rs` — 問題あり(小)(3 回目 5 節の 5 番を引く)。** 循環を閉じる逆向きの辺 `config.rs → model_limits.rs` は、前と同じ `list_model_ids` の 1 本(`crates/horizon-agent/src/config.rs:414-431`)である。加わった 2 モジュールは、`config.rs` の設定の型が `AuxiliaryConfig`・`DecisionConfig` をフィールドに持ち、その 2 つのファイルが `config.rs` の定数 `OPENAI_BASE_URL_VAR` を読むことでできた 2 方向の辺である。定数が unit になった(設計文書「Rebuilt on SCIP」)ことで数えられるようになった。親の設定が子の設定を持ち、子が共通の環境変数名を親から読むのは、障害を名指しできない。`list_model_ids` の辺を外しても `config.rs`・`auxiliary.rs`・`decision/config.rs` の 3 モジュールの循環が残る。
5. **`registry.rs`・`providers/mock.rs`・`providers/rig/mod.rs` — 問題なし(1 回目の 3 番、2 回目の 3 番「トレイトと工場の同居、親子の参照」を引く)。** `registry.rs` は `Provider`・`SessionHandle` のトレイトを定め、`Provider::for_entry` と `MockProvider::new` で provider を作る。provider の側はトレイトを実装する。
6. **`rig/session/continuation.rs`・`input.rs`・`state.rs` — 問題なし。** `SessionLoopState`(`state.rs:36-74`)はセッションのループの状態で、`inputs: super::input::Inputs`、`continuation: super::continuation::ContinuationState` を兄弟のファイルの型としてフィールドに持ち、兄弟のファイルは `impl SessionLoopState`(`input.rs:190-`、`continuation.rs` の `handle_continuation_check` など)で method を足す。faeccb05(2026-08-04「bundle run_session_loop state into SessionLoopState, split session.rs」)で 1 つのファイルを分けた形である。1 つの型の impl を複数のファイルに分けた形は `cycle_edges := 'collapsed'` が除く対象だが、辺の中に型のフィールドや兄弟の型への参照(`state.rs → input.rs` の `Inputs`、`input.rs → state.rs` の `activation`・`commands`・`events_tx`)が混ざるので、「every unit edge joins impls of one type」に当たらず残る(機構 Y1)。3 回目の原因 8(継続の 3 つの写し、`continuation.rs:287-303` と `turn.rs:325-343`・387-405)はこのファイルにあるが、循環の辺ではない。
7. **agentd の `session/state.rs`・`attachment.rs`・`subscription.rs` — 問題なし。** 6 番と同じ形である。`AgentdState` は `agent_subscribers: super::attachment::Streams` と `session_subscriptions: super::subscription::SessionSubscriptions` を持ち(`crates/horizon-agentd/src/session/state.rs:58`、63)、`subscription.rs:71-` は `impl AgentdState` で購読の method を足し、`attachment.rs` の `capture`・`subscribe` は `&AgentdState` を取る。3 回目の原因 3(`AgentdState` の 2 つの lock、`state.rs:42`、46)はこの型にあるが、循環の辺とは関係しない。
8. **`horizon-board` の `store/source.rs`・`logd.rs`・`memory.rs` — 問題なし。** `Source` は読み書きの出所の enum で(`crates/horizon-board/src/store/source.rs:17-25`)、モジュール文書が「This is the one place that knows which sources a target has」と書く。`source.rs` は各出所の関数に振り分け、`memory.rs` は `Source::Memory` を作り、`logd.rs` は `Source` で分岐する(`logd.rs:74-75`)。出所を足すと variant と振り分けと出所のモジュールを足すことになり、`logd.rs` の `match` の腕はコンパイラが直させる。
9. **`horizon-workspace` の `mode.rs`・`nav.rs`・`types/entity.rs` — 問題なし。** `Workspace` が `workspace_mode` のフィールドに `mode.rs` の `WorkspaceMode` を持ち、`mode.rs` は `Workspace` のカーソルの遷移を実装し、幾何の計算 `nearest_in_direction`・`pane_rects` を `nav.rs` に委ね、`nav.rs` は `mode.rs` の `Direction` を使う(`crates/horizon-workspace/src/nav.rs:10`)。`Direction` を `nav.rs` に移せば 1 本は切れるが、名指しできる費用は無い。
10. **`src/board/events.rs`・`list.rs`・`thread.rs` — 問題なし。** `events.rs` はビューが上に知らせるイベントの型を定め、その隣で `impl EventEmitter<OpenTaskThread> for BoardListView {}` などの印の impl を宣言する(`src/board/events.rs:32-35`)。この 4 行がビューの型への辺である。

## 5. 軸ごとの集計

| 軸 | 問題あり | 問題なし | 判断できない | precision@10 | 問題ありの内訳 |
| --- | --- | --- | --- | --- | --- |
| `duplication` | 2(2、6) | 8 | 0 | 0.2 | 新しい原因 1(2 番、小)、既知 1(6 番、リストのモーダル) |
| `coupling` | 1(9) | 9 | 0 | 0.1 | 既知 1(`RigAgentConfig`、ファイルのモジュール経由) |
| `unreferenced` | 0 | 10 | 0 | 0.0 | — |
| `module_cycles` | 2(1、4) | 7 | 1(2) | 0.2 | 既知 2(`start_approval_gate`、`list_model_ids`、どちらも小) |

合わせて 40 件のうち `問題あり` 5、`問題なし` 34、`判断できない` 1 である。原因の重複を除くと `問題あり` の原因は 5 個で、新しいものは 1 個(daemon のクライアントの接続ループの写し)だった。

## 6. 誤報の機構

「種類」は、依頼文の分類(軸の定義、数え方の規則、`.code-db.toml` に置くべき判断、SCIP や木の限界)である。「上位で説明する件数」は、この記録の上位 10 件のうち、その機構で `問題なし` の数が高くなったものの数で、機構どうしは重なる。

| 機構 | 軸 | 種類 | 上位で説明する件数 | 中身 |
| --- | --- | --- | --- | --- |
| **D1** 表・リストの自分自身とのずれた一致 | duplication | 数え方の規則 | 7(1、3、4、5、7、8、9) | 同じ形の行の並び(データの表、enum の variant、`concat!` の文字列、1 行の関数、4 方向の `match`)が、数行ずらした自分自身と jscpd の組になる。`same_unit` は 10 件のうち 8 件で true だった。並びが行数の降順なので、長い表ほど上に来る。`same_unit` を分けて並べるか、2 つの断片が重なる組を除くかは数え方の問題である |
| **D2** normalized がリテラルと識別子を区別しない | duplication | 数え方の規則 | D1 の 7 件と重なる。単独では 0 | 上位 10 件はすべて normalized の組だった。exact の組では 1・3・4・5・8・9 のような一致は起きにくい。同じ断片の exact と normalized は別々の行になる(15・16 位の `aggregate_receipt`) |
| **D3** 断片が unit の境をまたぐと側がファイルのモジュールになる | duplication | 木と SCIP の限界(impl の unit が無い) | 0(2 番は判定に影響しないが、側の名前が `agent`・`terminal` になった) | 最も内側の、断片の全行を含む unit を側にするので、2 つの item にまたがる断片はファイルのモジュールに解決する。impl の unit が無くなったので、method 2 つにまたがる断片も同じになる(原因 8 の `src/runtime/mod.rs:464-486` と 651-673 は `runtime`) |
| **D4** 意図した写し | duplication | `.code-db.toml` に置くべき判断、または派生 | 1(10) | 境界の向こう側の写し(シェルの ACP の frame と内部の frame)。`conversion_pair` は `From` などで結ばれた型の組だけを印にし、この組は変換の impl で結ばれていないので印が付かない |
| **C1** ファイルのモジュールが中身を合算する | coupling | 軸の定義(`unit_scope`) | 4(1、2、9、10) | ファイルのモジュールの fan-in は中の全 unit の合算なので、契約の型のファイルが上に来る。中身が 1 つの型のファイル(2・10 番)は、その型とほぼ同じ行がもう 1 つ出る。9 番は原因の型を含んでいたが、行の数はその型を指さない |
| **C2** テストからの参照を数える | coupling | 数え方の規則(見出しに書かれている) | 3(6、8、5 も一部) | `SessionId.new` は本番 4 ファイル・テスト 62 ファイル。`OccurrenceId` 15 / 52、`ToolCallId` 22 / 56 |
| **C3** 契約の型を crate の数で見分ける | coupling | 派生の印の閾値 | 4(4・5・7・8 の型。ファイルのモジュールの行には印がそもそも付かない) | `contract_candidate` は 3 crate 以上から参照される型だけに付き、`horizon-agent` の契約の型(他の crate からは 1〜2)には付かない。上位で付いたのは `SessionId` だけ。M3(3 回目)の「伝播をコンパイラが強制する契約」は crate の数では見分けられなかった |
| **C4** ID の newtype | coupling | 軸の定義 | 3(3、5、8) | 1 行の newtype は、id を皆が使うことを示すだけで、変更の経路にならない |
| **U1** SCIP が記録しない参照(書式文字列、属性の文字列、`macro_rules!` の本体) | unreferenced | SCIP の限界(見出しに書かれている) | 7(1〜7) | 見出しが挙げるとおりの形だった。27 行のうち少なくとも 10 行がこれ |
| **U2** cfg で無効な利用側と、マクロが生成した項目 | unreferenced | SCIP の限界(見出しに無い) | 3(8〜10) | 利用側の `src/preview/guest.rs` は `#[cfg(target_family = "wasm")]` で、ホストの索引に入らない。`#[interface]` と `icon_assets!` が生成した項目は、生成した側の行の範囲を持つ |
| **U3** loc の降順 | unreferenced | 数え方の規則 | —(順位の問題) | 本物の死んだ `pub` 項目(`set_session_context` 16 位、`known_security_services` 18 位)は小さいので、大きな文字列の定数の下に来る |
| **U4** 分析の対象の範囲 | unreferenced、duplication | `.code-db.toml` に置くべき判断(`scope.exclude`) | 2(unreferenced 1、duplication 4) | 設定ファイルが無いので `duckdb-ext/` 自身のコード(`USAGE`、`KINDS`、拡張の `entrypoint`)も数に入る。対象に含めるかは判断である |
| **Y1** 1 つの型の状態を兄弟のファイルに分けた形 | module_cycles | 数え方の規則(構造上の辺の定義) | 2(6、7)。8〜10 も近い形 | 状態の型が兄弟のファイルの型をフィールドに持ち、兄弟のファイルが `impl` でその型に method を足す。impl どうしの辺だけを除く今の規則では、フィールドの参照と兄弟の型への参照が残る |
| **Y2** ファイル直下の `pub use` の辺 | module_cycles | 数え方の規則(SCIP の組み直しで入った) | 1(1 番の 15 モジュールの拡大) | 再公開の 1 行が、実際に呼ぶ子のモジュールの代わりに親のファイルのモジュールの辺になる。子から親への辺は構造上の辺として除かれるので、再公開の辺だけが循環を閉じる |
| **Y3** 定数が unit になったことによる辺 | module_cycles | 数え方の規則 | 1(4 番の 2 モジュールの拡大) | 子の設定が親の設定の定数を読み、親が子の設定の型を持つ |

## 7. 3 回目の引き継ぎの 14 件の追跡

3 回目 9 節の一覧(今回も上位 6 件、上位から外れた 5 件、新しい 3 件)の 14 件である。3 回目の 14 件の `問題あり`(9 個の原因)は、このうち 1〜6 と 12〜14 に当たる(1 は 5 行、12 は 2 行)。順位は、各クエリの既定の出力の中の順位(循環は大きさの順の番号)、括弧はテストを含む切り替え(`include_tests := true`、`unreferenced` は `include_structural := true`)での順位である。`size`、`complexity`、`interface`、`intent` の順位は引くだけで、その軸の判定は別の記録が行う。「最高」は、既定の出力でどのクエリでも最も良い順位である。

ソースは 3 回目の b3b94fc1 から変わっていない(`git diff --stat b3b94fc1 d6299e9b -- src crates preview-plugin` が空)。どの原因も直っていない。

| 3 回目 9 節 | 原因の unit | duplication | coupling | unreferenced | module_cycles | 他の軸(順位だけ) | 最高 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1. 4 本の送信路 | `TerminalSession.spawn`、`WorkspaceShell` 型、`WorkspaceShell.new` | 339(spawn の引数と型のフィールドの組、6 行) | 176(`WorkspaceShell`、12 ファイル) | なし | `src/workspace/mod.rs` を含む循環なし(`pane.rs` の 2 モジュールの循環 22 番は別の辺) | interface 37(spawn、7 引数)、size 70(型、102 行)、complexity 261 | interface 37 |
| 2. `reconcile` のビューの構築 | `WorkspaceShell.reconcile` | **117**(reconcile の中の自分との組、`session_lifecycle.rs:356-375` と 380-397。ボードの一覧と thread のビューの構築の 2 つの枝で、原因そのもの) | 305 | なし | なし | size 13(211 行)、complexity 65(16) | size 13 |
| 3. `AgentdState` の 2 つの lock | `AgentdState` 型、`reload_provider_config` | なし | 32(型、32 ファイル) | なし | 7 番の循環に入るが、原因の辺ではない | size 85 | coupling 32 |
| 4. `RigAgentConfig` の 3 種類の値 | `RigAgentConfig` 型 | なし | **9**(ファイルのモジュール `config` 経由)、型自身は 48 | なし | 4 番の循環に `config.rs` が入るが、閉じる辺は `list_model_ids` | size 73 | coupling 9 |
| 5. `start_approval_gate` | 同名の関数 | なし | 2,067 | なし | **1**(原因の辺) | interface 172、size 282 | module_cycles 1 |
| 6. テスト用の描画の写し違い | `render_agent_transcript`(テスト) | なし((1,083)) | なし((1,518)) | なし | なし | (complexity 17、size 63) | 既定ではなし。テストを含めて complexity 17 |
| 7. `commands.rs` のボードの処理 | `commands.rs:136-448` の method | 218(`open_board_organizer` と `spawn_agent_lookup_attach`、13 行) | 2,034 | なし | なし | complexity 114(`open_board_organizer`、12)、size 137 | complexity 114 |
| 8. `AgentdHandle` と `TerminaldHandle` の同じ操作 | `begin_reload`・`stop_and_wait`・`register_*` | **86**(`src/runtime/mod.rs:464-486` と 651-673、23 行、側はファイルのモジュール `runtime`) | 1,638 | なし | なし | — | duplication 86 |
| 9. 継続の 3 つの写し | `handle_continuation_check`、`handle_truncation_recovery`、`handle_memory_checkpoint` の断片 | **155**(`continuation.rs:287-303` と `turn.rs:327-343`、17 行) | なし | なし | 6 番の循環に `continuation.rs` が入るが、原因の辺ではない | — | duplication 155 |
| 10. real-git のテストの補助関数 | `setup.rs` の `tests.real_git`、`worktree.rs` の `tests.git`(テスト) | なし(**(7)**:`setup.rs:403-483` と `worktree.rs:450-560`、81 行。写されたテストの本体で、原因の行 338-346・434-447 の組は無い) | なし | なし | なし | (complexity 1,177) | 既定ではなし。テストを含めて duplication 7 |
| 11. 2 つの `expect_finished` | 同名の 2 つの関数(テスト) | なし((23)、`tools/bash/tests.rs:18-64` と `tools/tests.rs:139-182`、47 行) | なし | なし | なし | (complexity 520) | 既定ではなし。テストを含めて duplication 23 |
| 12. 4 つのリストのモーダルの写し | `render`、`modals.rs` の開く・閉じる・取り消す | **6**(`render.rs:1115-1193` と 1208-1286) | 2,047 | なし | なし | **size 3**(`render`、291 行)、complexity 304 | size 3、duplication 6 |
| 13. `append_event` による本番の INSERT の写し | `Store.append_event`(テスト)、`append_record_uncommitted` | なし((366)、`append.rs:125-145` と 189-209) | なし((594)) | なし | なし | size 123(本番の側だけ) | 既定では写しとしては届かない |
| 14. 設定の型のモデル一覧の取得 | `NamedProviderConfig.list_model_ids` | なし | 1,942 | なし | **4**(原因の辺) | — | module_cycles 4 |

まとめ:

- **既定の上位 10 件に入るもの**:5(循環 1)、12(size 3、duplication 6)、14(循環 4)、4(coupling 9、ただしファイルのモジュールの合算で、原因の型を指してはいない)の 4 件。
- **テストの切り替えで上位 10 件に入るもの**:10(duplication 7)。ただし組は原因の行(git の大域設定から切り離す修正が条件の内側に入った `scrub_git_env` と `git`)ではなく、隣の写されたテストの本体である。原因の行は 2 つの写しが食い違った箇所なので、その食い違い自体が jscpd の一致を切っている。
- **11〜200 位**:2(size 13、duplication 117 は原因の 2 つの枝そのもの)、3(coupling 32)、1(interface 37)、8(duplication 86)、7(complexity 114)、9(duplication 155)。
- **既定では届かないもの**:6、11、13 は原因の片側か両側がテストの unit で、本番の一覧から除かれる(`duplication` は片側がテストの組を除く)。13 は本番の側の `append_record_uncommitted` が size 123 に出るだけで、写しとしては届かない。
- **軸ごと**:`duplication` は写しの原因(2、8、9、12、10・11・13 はテストの切り替えで)に届くが、行数の降順なので、原因の組の多くは 80〜340 位にある。上位は D1 の表が占める。`coupling` は既知の原因のうち fan-in の高い型(3、4)にだけ届き、上位 10 件の中ではファイルのモジュールの合算(C1)を通してしか届かない。`unreferenced` はどの原因にも届かない(原因はどれも使われているコードである)。`module_cycles` は、置き場所を外れた 1 つの関数が循環を閉じる形(5、14)にだけ届き、3 回目と同じである。

## 8. 記録やクエリの見出しと食い違った点、気づいた点

- **最大の循環の 25 → 40 の中身**(設計文書「Rebuilt on SCIP」が「whether that count fits the purpose is not yet judged」と書く点)。15 モジュールの増加のうち 14 モジュールは、`persistence/event_log/mod.rs:23-25` のファイル直下の `pub use` の 1 本で結ばれていた(4.1)。この辺が無ければ、judge・`policy.rs`・tools の 26 モジュールと rig の 5 モジュールに分かれる。設計文書の「file-level `pub use` and impl headers now belong to the file module」の効果であり、判定(問題ありの理由になる辺)は変わらなかった。
- **3 回目の 5 番の循環の大きさ。** 3 モジュール(`config.rs`・`model_limits.rs`・`model_catalog.rs`)が、定数の参照で `auxiliary.rs`・`decision/config.rs` を加えた 5 モジュールになった。設計文書の「references to constants count」の効果である。
- **`unreferenced` の見出しの列挙に、cfg で無効な利用側が無い。** 見出しは「callbacks registered by name, FFI exports, uses a derive generates, macro bodies, a name used only inside a format string … an attribute's string」を挙げるが、上位 10 件のうち 3 件(8〜10)は、利用側が `#[cfg(target_family = "wasm")]` のモジュール(`src/preview/guest.rs`)にあることによる。クエリの欠陥ではなく、見出しの記述の漏れである。
- **`coupling` の見出しの `contract_candidate`。** 見出しは「a type many crates use and whose changes the compiler propagates to them (wire messages, identifiers, events), which the reading checks judged no obstacle」と書くが、既定の閾値 3 では、読み判定が契約の型として `問題なし` にした `horizon-agent` の型(`Event`、`Command`、`ToolCallRequest`、`ToolCallResult`、`MessageRole`)のどれにも付かない(どれも他の crate からの参照は 1〜2)。上位 10 件で付いたのは `SessionId` だけだった。クエリは見出しの定義どおりに動いており、欠陥ではない。
- **`coupling` の見出しの「Rows: every unit」。** ファイルのモジュールも unit として行になり、その fan-in は `unit_scope` により中身の合算である。見出しには書かれていない。中身が 1 つの型のファイルでは、ファイルと型の 2 行がほぼ同じ数で並ぶ(`session` と `SessionId`、`tool_result` と `ToolCallResult`、`horizon-workspace` の `session_id` と `SessionId`)。
- **`horizon-wire` の `SessionId` の文書。** 型の文書が「Horizon has its own shared `session::SessionId` … this crate cannot depend on it」と書くが、型は今 `horizon-wire` にあり、`horizon-agent` は再公開している(`crates/horizon-agent/src/contract.rs:11-17`)。モジュール文書は、wire の schema を変えないために文書を元の場所から一字一句写したと書いている(`crates/horizon-wire/src/session.rs:6-9`)。判定には影響しない。
- **クエリの欠陥は見つからなかった。**

## 9. 限界

- **読んだのは 1 人である。** 判定は Opus 1 体が 1 回読んだもので、オーナーの意図を知らない。前の判定を引いた候補は、前の判定の誤りを引き継ぐ。
- **精度の根拠は弱い。** 1 つのリポジトリの、軸ごとに 10 件の判定である。precision@10 の 0.1 と 0.2 の差は 1 件である。
- **`問題あり(小)` も `問題あり` として数えた。** 5 件のうち 4 件(duplication 2、循環 1・4)は費用の小さい判定である。
- **新しい原因の費用。** `duplication` 2 番の費用は、2 つのループの比較と `git log -L` の差分から導いた。2 つのループを別々に直して不具合になった例は見つけていない(8c52fe5a の寄せ直しは、食い違いを後から手で揃えた跡である)。
- **循環の辺の計算。** 4.1 の強連結成分は、`module_cycles` の出力(既定の `collapsed` の辺)から Python で求め直したもので、SQL で確かめてはいない。外した辺を外した後の循環が、まとめる規則の下で同じになるかは、出力の辺の集合の中で計算したかぎりである。
- **追跡の当て方。** 名前の無い原因(7〜10、12 の `modals.rs`)は、原因の行の範囲に始まる関数と method、または原因の行に重なる断片で探した。範囲の取り方で順位は変わりうる。
- **上位の取り方。** 3 モジュールの循環が 7 個あり、出力の順で 6 個を取った(11 番目の `src/theme/palette.rs` は判定していない)。
- **作業ツリー。** 測定は d6299e9b の複製で行い、索引・jscpd の報告・キャッシュ・CSV・計算に使った Python は `target/code-db/` の下にあり、コミットしていない。
