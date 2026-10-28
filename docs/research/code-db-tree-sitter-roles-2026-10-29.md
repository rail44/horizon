# コードデータベースで tree-sitter の木が担う役割と、SCIP・git で代われるか(2026-10-29)

オーナーは 2026-10-29 に、今の木についての事実は SCIP を主にするとし、「SCIPが主でいいと想いますね。treesitter由来の木に残る役割をむしろ探した上で残すかどうかを判断したい。」と述べた。この記録は、SQL 層で `sitting_duck` の `read_ast`(tree-sitter の木)が今担っている役割を列挙し、役割ごとに SCIP か git の事実で代われるかを測る。木を残すかどうかの判断はオーナーのもので、ここでは推奨を書かない。

## 方法と環境

- 対象はブランチ `code-quality-survey` の 7e84967d(以下 HEAD)である。worker の作業ツリーの `target/code-db/clone` に `git clone --no-checkout` してから HEAD を取り出し、その clone で `duckdb-ext/build.sh` を動かし(47.6 s)、`rust-analyzer scip` で索引を作った。rust-analyzer は 1.99.1(58cf8f9 2026-10-13)、DuckDB CLI は 2.0.0(`~/.local/bin/duckdb2`)、sitting_duck は `00_load.sql` のとおり community から入れたもの。
- ビューは `run.sh` と同じ順で読み(`views/clones.sql` は jscpd の報告が要るので除いた)、`refresh.sql` の永続キャッシュを作ったうえで(履歴全体、冷えた状態から 82 s)、比べる行を別のデータベースファイルに写して問い合わせた。測定用の SQL と Python(SCIP の protobuf を直接数える小さな読み手)は `target/code-db/scripts/` に置き、コミットしていない。`duckdb-ext/` 以下は読んだだけで変えていない。
- 読んだもの:`docs/code-database-design.md`、`duckdb-ext/sql/README.md`、`views/*.sql`、`lang/rust.sql`、`queries/*.sql`、`refresh.sql`・`cache.sql` の該当箇所、`sitting-duck-evaluation-2026-10-24.md`、`code-db-scip-2026-10-25.md`、`code-quality-axes-2026-10-29.md`、`code-db-dogfood-reading-3-2026-10-28.md` 7 節、`code-db-dogfood-retrospective-3-2026-10-28.md` 第 2 部。
- 機械は他の worker と共有のオーナーのデスクトップで、索引の時間は他の負荷の下での値である。

## 要約

- **今の木について、unit の大半は SCIP の定義で置き換えられる。** 索引にある 509 ファイルの unit 8,816 個のうち 7,732 個(87.7%)は、同じファイル・同じ名前・同じ終わりの行の SCIP の定義と 1 対 1 に対応し、種類(function・method・type・module・macro)は対応したもの全件で一致した。対応しなかった 1,084 個は、impl 551(SCIP に impl の記号は無い)、`mod x;` の宣言 474、属性マクロ(`async_trait`、remoc の `rtc::remote`)の下のメソッド 28、cfg で無効なコードの関数 19、関連型 12(SCIP の種類が `AssociatedType`)である。
- **SCIP の `enclosing_range` は、前置きの doc コメントと属性を含む。** 対応した 7,732 個のすべてで、`enclosing_range` の始まりは `lead_start_line` と等しかった。前置きを持つ unit 5,450 個も例外なく等しい。前置きを除いた `start_line` は、範囲の中を行の規則(`//`、`#[`、空行を飛ばす)で読むと 7,725 / 7,732 で一致した。
- **限定名は、記述子から作ると 7,680 / 7,732(99.3%)一致した**(impl の名前の書き方と型引数をそろえた後)。違うのは、関数の中に書いた item 45 個(SCIP の記述子は囲む関数を含まない)と、トレイトの名前が記述子に出ないか別名になる trait impl のメソッド 7 個である。
- **テストの判定は SCIP の役割の印からは得られない**(`Test` と `Generated` の bit は全 301,687 出現で 0)。パスの規則と、SCIP の範囲の中の本文に対する正規表現(`#[test]` 系と `#[cfg(test)]`、範囲の包含で継承)を組むと、今の `unit_tests` のテスト 3,600 個のうち 3,598 個を拾い、テストでない 4,132 個のうち 7 個を誤って拾った。
- **履歴の側は SCIP では代われない形が残る。** blob の層は 6,605 個の blob の unit 171,301 行を持つ。`.rs` を変えたマージでないコミットは 899 個で、索引 1 つは冷えた状態で 167〜169 s・24〜30 MB、同じ clone で続けると 34〜255 s(依存の変わり方による)で、`target/debug` は 2.5 GB から 6.0 GB に増えた。HEAD の定義に hunk を逆にたどる行の追跡では、今ある unit の履歴は作れるが、消えた unit(`removed` 8,400 行、変更された同一性 19,673 個のうち HEAD にあるのは 8,189 個)と、系譜を通った履歴(`unit_history` 21,725 行のうち 7,394 行)と、比較の前の側が失われる。
- **頑健性。** 構文の誤りがあるファイルも rust-analyzer は索引にした。`Cargo.toml` が壊れていると索引は 1 つもできなかった(0.7 s で panic)。cfg で無効なファイル・関数(macOS 専用、wasm のゲスト)と、ルートの cargo ワークスペースの外のファイル(`duckdb-ext/`、`preview-plugin/`、`scripts/` の fixture)は索引に無い(26 ファイル)。tree-sitter は 535 ファイルすべてを、ビルド無しで解析した。

## 1. 目録:木を読むもの、木から導くもの

木(`nodes`・`scope_nodes`・`blob_nodes`)を直接読むのは `lang/rust.sql` の 3 つの規則だけで、他はその出力を読む。

| 役割 | 定義の場所 | 木から取るもの | 下流で使うもの |
| --- | --- | --- | --- |
| R1 unit の集合と種類 | `rust_units_over`(`units`、`blob_units`) | `function_item`・`function_signature_item`・`impl_item`・`struct/enum/union/type/trait_item`・`mod_item`・`macro_definition` と、親・祖父の型(method かどうか) | すべての unit の問い。種類は `unit_changes` の同一性 `(path, kind, qualified_name)`、`graph` の `kind = 'impl'`・`'module'`・`'type'` の絞り込み、系譜の種類の互換 |
| R2 名前と限定名 | 同上 | 名前のノード(識別子、impl は `for` の前後)の本文、囲む unit の名前を `.` で結ぶ | 同一性(`unit_changes`、系譜、`unit_at_before`、`compare_units`)、モジュール(`modules` の `module_path` は module unit の限定名から)、クエリの表示 |
| R3 行の範囲 | 同上 | ノードの `start_line`・`end_line` | 変更の帰属(`unit_change_sides` の `all`)、系譜の本文(`[start_line, end_line]` の行の集合の Jaccard)、出現の所属(`graph_unit_occurrences_over`)、fan-in(定義が範囲に収まる記号)、クローンの側(`contain`)、印の継承(印の範囲の包含) |
| R4 前置きの始まり `lead_start_line` | 同上(`lead_siblings`〜`rust_unit_leads`) | 直前の兄弟の `attribute_item`・`///` のコメントの並び | `innermost` の帰属の自分の行、クローンの側の `overlap` |
| R5 入れ子 `parent_unit_id`・`unit_id` の順 | 同上 | preorder の番号と `descendant_count` | `innermost` の帰属(内側の unit の行を引く)、`unit_contains`・`unit_ancestors`、最も内側の unit の選択(`max(unit_id)`)、`unit_module`、`same_type_unit_edges`、継承のリンク(限定名の前方一致) |
| R6 `loc` | 同上 | 範囲 + `source_lines`(本文) | `unit_churn`・`unit_dependencies`・`unit_fan_in_churn`・`compare_units` の列。後ろ向きの確認の P@100 0.75 |
| R7 印 `test` | `rust_tags` | `#[test]` 系・`#[cfg(test)]`(引数の木まで)・`#![cfg(test)]`、パスの規則 | `unit_tests`、辺の `src_is_test`/`dst_is_test`、`unit_cochange` の `include_tests`、`module_edges` の `test_unit_edges`、`clone_site_tags` |
| R8 印 `generated_or_macro` | `rust_tags` | `macro_invocation`・`macro_definition` の範囲 | 出現の `in_macro`、辺の `via_macro`(34,195 本のうち 5,573 本)、`macro_unit_edges`、`clone_site_tags` |
| R9 印 `trait_impl` | `rust_tags` | `impl_item` の直下の `for` | `clone_site_tags` だけ(232 個) |
| R10 impl の部分 `impl_parts` | `rust_impl_parts_over`(今の木、比較の前の側) | `for` の前後の子、最初の `type_arguments` までの頭 | `impl_targets` → `impl_for`(549 / 587)・`implements`(45 / 232)→ `same_type_unit_edges`(循環から除く辺の規則、M6) |
| R11 blob の unit | `rust_blob_units` → `blob_units` | 6,605 個の blob の R1〜R6 | `unit_change_sides`・`unit_changes`、系譜の像の範囲、`snapshot_units`(比較の前の側) |
| R12 比較の前の側の木 | `snapshot_nodes`・`snapshot_impl_parts` | blob の木 | 前の側のグラフ(`snapshot_module_edges` ほか)。前の側の参照は別の索引 `scip_before` から |

木を使わないものも確かめた。系譜の本文と類似度(`lineage_sources`、`blob_lines`)は `read_text` の行で、木は unit の行の範囲(R3)を通してだけ効く。`blob_blank_lines` と `source_lines` も本文の行だけである。クローンの側の解決(`clone_change_consistency`)は R3〜R5 を読む。`file_crate` と `modules` の `module_path` はパスの規則で、木は module unit(R1・R2)を通してだけ入る。キャッシュ(`refresh.sql`)は `nodes` と `blob_nodes` を写し、導出の鍵に sitting_duck の版を含める。

今は使っていないが、軸の記録(`code-quality-axes-2026-10-29.md` 5 節)が「規則を足す」とした導出は、どれも木を読む前提で書かれている:複雑さと入れ子の深さ(3.3)、引数の数と可視性(3.4)、文書の有無とコメントの密度(3.15)、unsafe・パニック・ジェネリクス(3.17)、TODO/FIXME(3.18)、M3 の網羅的な `match` の数え方。

## 2. 役割ごとの置き換え

### 2.1 rust-analyzer 1.99.1 が埋める SCIP の欄

HEAD の索引(30,385,734 byte、509 文書、出現 301,687、文書の記号 42,830、外部記号 9)を protobuf のまま数えた。

| メッセージ | 埋まっている欄 | 空の欄 |
| --- | --- | --- |
| Document | `relative_path`、`occurrences`、`symbols`、`language`、`position_encoding`(全 509) | `text` |
| Occurrence | `range`(全件)、`symbol`(全件)、`symbol_roles`(42,376 = 定義の数。参照は 0 で欄が省かれる)、`enclosing_range`(124,141) | `override_documentation`、`syntax_kind`、`diagnostics` |
| SymbolInformation | `symbol`・`kind`・`signature_documentation`(全 42,830)、`display_name`(42,483)、`enclosing_symbol`(29,680)、`documentation`(4,839) | `relationships` |

- `enclosing_range` は定義 42,376 件すべてにある。参照にも 81,765 件あるが、そのうち 80,061 件は同じ文書の同じ記号の定義の `enclosing_range` と同じ値で、参照を囲む範囲ではない(参照の位置がその範囲に入るのは 4,848 件)。
- `enclosing_symbol` は `local` の記号(変数、引数)だけにあり、unit になる記号には無い。
- `signature_documentation` の本文は宣言の頭である(例:`pub(crate) struct TerminalHost`、`fn spawn_agentd_at(socket_path: PathBuf, event_log_path: PathBuf) -> AgentdProcess`)。可視性(`pub` で始まるもの 5,100)、`async`(223)、`unsafe`(1)、引数と戻り値を含み、属性は含まない。
- `documentation` は doc コメントの本文(4,839 記号)。
- 役割の bit の `Test`・`Generated` は 0 件だった(`is_test`・`is_generated`)。
- 記号の package は 17 個で、`file_crate` がパスから決める crate(索引にあるファイルについて 17 個)と数が同じだった。

### 2.2 R1 unit の集合と種類

SCIP の定義のうち、種類が Function → function、Method・StaticMethod・TraitMethod → method、Struct・Enum・Union・TypeAlias・Trait → type、Module → module、Macro → macro のものを unit の候補とし、`units` と、同じファイル・`display_name = name`・名前の位置が unit の範囲の中・`enclosing_range` の終わりの行 = `end_line` で結んだ。

| unit の種類 | 索引にあるファイルの unit | SCIP と対応 | 対応しない | 対応しない理由 |
| --- | --- | --- | --- | --- |
| function | 4,759 | 4,741 | 18 | cfg で無効なコード(macOS 専用・非 Linux の分岐、`horizon-sandbox` の `main` 2 つ、`is_available` の片方など)。SCIP に定義が無い |
| method | 1,811 | 1,782 | 29 | 28 は属性マクロの下(`#[async_trait]` の `decide`・`search`、remoc の `#[rtc::remote]` のトレイトの `hello`・`drain` など、`src/preview/schema.rs` のトレイト):定義はあるが範囲が展開から写されてずれる(トレイト全体の範囲になるか、前後に 1〜2 行広がる)。1 は cfg で無効 |
| type | 995 | 983 | 12 | impl の中の関連型 `type Item = …`。SCIP では種類 `AssociatedType` で、範囲と名前は 12 件とも一致した |
| module | 694 | 220 | 474 | すべて 1 行の `mod x;` の宣言(4.4 節) |
| impl | 551 | 0 | 551 | SCIP に impl の記号は無い(2.6 節) |
| macro | 6 | 6 | 0 | — |
| 計 | 8,816 | 7,732 | 1,084 | |

対応したものの種類は、function と method の区別を含めて全件一致した。逆に、unit の候補の種類で unit と対応しない SCIP の定義は、ファイル自体のモジュール 509(範囲がファイル全体)、マクロが作った Struct 18(`src/workspace/mod.rs` の `actions!` の 16 個など。範囲はマクロの呼び出しの全体)、属性マクロの下の Method 11 と TraitMethod 17(上の 28 と同じもの)だった。

tree-sitter の unit に無く SCIP にあるもの:Constant 577 と StaticVariable 34(第 3 回の比較の評価が「レビューに要るのに出ていない」とした `const`/`static`)、マクロが作った item、フィールド 2,652 と enum の variant 1,147、関連型。どちらにも無いもの:クロージャ(木には `closure_expression` が 3,217 あるが unit にしていない。SCIP はクロージャを記号にしない)。

### 2.3 R3・R4 行の範囲と前置き

対応した 7,732 個で、`enclosing_range` の始まりと終わりを比べた。

| 種類 | 対応 | 始まり = `lead_start_line` | 前置きを持つ(`lead_start_line < start_line`) | 始まり = `start_line` |
| --- | --- | --- | --- | --- |
| function | 4,741 | 4,741 | 3,664 | 1,077 |
| method | 1,782 | 1,782 | 741 | 1,041 |
| type | 983 | 983 | 825 | 158 |
| module | 220 | 220 | 219 | 1 |
| macro | 6 | 6 | 1 | 5 |

終わりは対応の条件なので全件等しい。つまり SCIP の範囲は `[lead_start_line, end_line]` そのもので、木が別の規則(`lead_siblings` の連なり)で求めている前置きを最初から含む。逆に、前置きを除いた `start_line` は SCIP の欄には無い。範囲の中の行を上から読み、`//` で始まる行、`#[` で始まる行、空行を飛ばした最初の行を `start_line` とみなす規則は 7,725 / 7,732 で一致し、違う 7 個は複数行にわたる属性だった。

`all` の帰属、系譜の本文、出現の所属、fan-in は `[start_line, end_line]` を使う。SCIP の範囲に替えると、前置きの行がこれらに入る(帰属と類似度の値が変わる)。前置きを除く形を保つには、上の行の規則か木が要る。

### 2.4 R2 限定名

文書ごとに、範囲がファイル全体のモジュールの定義の記号(crate の根では `… crate/`)を接頭辞とし、記号からそれを除いた残りの記述子を名前の列に直した(`x/` → `x`、`T#` → `T`、`f().` → `f`、`impl#[T]` → `T`、`impl#[T][Tr]` → `Tr for T`)。両側の impl の名前から型引数と `a::b::` の経路を除いてそろえて比べた。

| 種類 | 対応 | 一致 | 違う(関数の中の item) | 違う(その他) |
| --- | --- | --- | --- | --- |
| function | 4,741 | 4,726 | 15 | 0 |
| method | 1,782 | 1,764 | 11 | 7 |
| type | 983 | 967 | 16 | 0 |
| module | 220 | 220 | 0 | 0 |
| macro | 6 | 3 | 3 | 0 |

- 関数の中の item:木は `parse_line.OkPayload`、SCIP は `OkPayload`(記述子に囲む関数が出ない)。同じファイルの別の関数の中に同じ名前があると、SCIP の名前は衝突する。
- その他 7:`src/preview/schema.rs` の属性マクロの下の trait impl 4 個は、SCIP の記述子が `impl#[PreviewHostRoot]` でトレイトを持たない。`AgentProvider for Provider` の 3 個は、SCIP では `Provider for Provider`(トレイトの別名での import)。
- 記述子の impl は、inherent でも trait でも `impl#[Type]method().`、`impl#[Type][Trait]method().` の形だった(依頼文の「`Type#method().`」とは違う。7 節)。
- そろえる前の文字列は、型引数や経路の書き方(`From<serde_json::Error> for StoreError` と `` `From<serde_json::Error>` for StoreError ``)で多く違う。同一性は文字列なので、木の名前と SCIP の名前を混ぜると、そろえる規則が同一性の一部になる。

### 2.5 R5 入れ子

SCIP には unit の親を指す欄が無い(`enclosing_symbol` は local だけ)。入れ子は範囲の包含か、記述子の前方一致で作れる。範囲の包含は、impl の範囲が無いので「method の親の impl」を作れず、method の親は type でもモジュールでもなく、無し(ファイル)になる。記述子からは「method が属する型の名前」(`impl#[T]`)が得られ、それは同じファイルの impl ではなく型そのものを指す。関数の中の item は、記述子でも包含でも親の関数に結べない(記述子が関数を含まず、範囲の包含なら結べる)。

### 2.6 impl の unit と R10 impl の部分

- impl の unit 551 個のうち、メンバーを 1 つも持たないもの(マーカーのトレイトの空の impl など)が 14、同じファイルに同じ名前の impl が複数あるものが 6 組 14 個だった。SCIP のメンバーの記述子から取れる impl の鍵(ファイルごとの `impl#[T]` / `impl#[T][Tr]`)は 527 個で、空の impl は消え、同じ名前の impl はまとまる。
- impl の範囲(頭、閉じ括弧、メソッドの間の行)は SCIP に無い。`innermost` の帰属が impl に付ける自分の行(1,579 行の impl の変更の行の元)と、`overlap` のクローンの側、印の範囲としての `#[cfg(test)] impl`、出現の所属(impl の頭の出現を源とする辺は 34,195 本のうち 618 本、出現 648)が、impl の範囲を前提にしている。
- `impl_for` を SCIP だけで作る形を測った。impl の unit のメソッドの記述子から型の名前を取り、同じ package の type の定義を名前で引くと、メソッドを持つ impl 526 個のうち 491 個は候補が 1 つで、今の `impl_for` とすべて一致した(食い違い 0)。31 個は同じ名前の型が package に複数あって決まらず、4 個は候補が無い。今の規則は頭の参照の出現の記号で解決するので曖昧さが無く、549 個を解決している。頭の位置(R10)が分かれば、今の規則はそのまま SCIP の出現で動く。頭の位置は木から取っている。
- `implements`(trait 側)も同じで、記述子のトレイトの名前は名前でしかない。今の解決数は 45 / 232(残りは外部のトレイト)。

### 2.7 `mod x;` の宣言とファイルのモジュール

- 木の module unit 694 個のうち 474 個は 1 行の `mod x;` で、SCIP はこの行を定義ではなく参照として持つ(472 / 474 個の行にモジュールの記号の参照の出現があった)。ファイル自体のモジュールは、SCIP では範囲がファイル全体の定義 509 個として現れる。
- 宣言を unit にしているのは今の木の規則で、読み判定 3 回目の N7(`mod x;` の宣言とそのファイルの item が co-change の組を作る)はこの規則から出ている。SCIP に移すと、宣言は unit でなくなり、ファイルのモジュールが unit になれる。
- `modules` の `module_path` は今パスの規則と module unit の限定名から作るが、SCIP の記述子はモジュールの経路(`#[path]` を含めた解決後の経路)をそのまま持つ。

### 2.8 R7 テストの印

対応した 7,732 個で、今の `unit_tests` と、木を使わない 3 つの規則を比べた。

| 規則 | 今のテスト 3,600 のうち拾う | 今のテストでない 4,132 のうち拾う |
| --- | --- | --- |
| パスの規則(今と同じ、パスだけ)+ 記述子に `tests/` のモジュール | 3,347 | 0 |
| パスの規則 + SCIP の範囲の本文への正規表現(`#[test]`・`#[x::test]`・`rstest`・`test_case`・`bench`、`#[cfg(test)]`・`#[cfg(all(test …`、`#![cfg(test)]`。`#[cfg(test)]` の付いた定義の範囲に含まれるものへ継承) | 3,598 | 7 |
| 3 つすべて | 3,598 | 7 |

- 拾えなかった 2 個は `#[cfg(test)] impl JsonValue` と `#[cfg(test)] impl Drop for Registration` のメソッドで、impl の範囲が SCIP に無いので継承できない。
- 誤って拾った 7 個は、doc コメントの本文に `#[cfg(test)]` と書いてある関数・型だった(`horizon-config/src/lib.rs` の `load` など)。コメントの行を除けば消える形である。
- 結合テスト(`tests/e2e.rs` など)の記号は crate の根に置かれ(`horizon-agentd 0.1.0 spawn_agentd().`)、記述子からはターゲットが分からない。パスの規則が要る。

### 2.9 R8 マクロの中の出現

SCIP はマクロの呼び出しを、マクロの名前の位置の参照の出現として持つ(記号が `!` で終わる参照 12,537 件。木の `macro_invocation` は 11,533)。呼び出しの広がり(引数の括弧の終わり)は無い。`in_macro` を SCIP と本文で作るなら、名前の位置から対応する括弧を本文で数える規則になる(文字列やコメントの中の括弧を区別しない近似)。役割の bit の `Generated` は 0 件で使えない。

### 2.10 R9 `trait_impl`

impl の記述子の 2 つ目の `[...]` が trait impl を表すが、印の範囲は impl の範囲なので、2.6 節と同じく範囲が無い。読むのは `clone_site_tags` だけである。

### 2.11 R6 `loc` と空行

`loc` は範囲と本文の行だけから計算する。SCIP の範囲(前置き込み)で数えると今の `loc` より大きくなり、前置きを除くには 2.3 節の行の規則が要る。空行(`blob_blank_lines`、`source_lines`)はもともと本文だけで、木を使わない。

### 2.12 crate と比較の前の側のグラフ

`file_crate` はこのリポジトリの配置をパスの規則で書いている(軸の記録 6 節が指摘した緊張)。SCIP の記号の package がその代わりになり、索引のあるファイルでは 17 個で数が一致した。比較の前の側(R12)は、参照の事実をすでに前の木の索引 `scip_before` から取っており、木から取っているのは unit(blob の unit)と impl の部分だけである。

## 3. 履歴の側

blob の層は、HEAD の範囲で 6,605 個の blob を解析し、unit 171,301 行を持つ(キャッシュの `blob_nodes` は 31,655,358 ノード、キャッシュファイル 653 MB)。`unit_changes` は 39,056 行(added 17,673、modified 12,983、removed 8,400)で、変更された同一性 19,673 個のうち HEAD に残るのは 8,189 個である。`unit_history` は 21,725 行のうち 7,394 行が系譜をたどった祖先から来ており(3,246 個の unit が祖先を持つ)、3,044 行がリンクのコミットである。

測った量:

| 量 | 値 |
| --- | --- |
| HEAD までのコミット | 1,636(マージでない 1,293、マージ 343) |
| `.rs` を変えたマージでないコミット | 899(`unit_changes` の sha は 895) |
| 第 1 親の列 / そのうち `.rs` を変えたもの | 1,139 / 792 |
| 索引 1 つ(新しい clone、冷えた `target/debug`) | HEAD 168.8 s・30.4 MB・`target/debug` 2.7 GB。e223bc20(2026-09-22)166.6 s・24.0 MB・2.5 GB |
| 同じ clone で続けた索引 | e223bc20^(`.rs` の差は索引の外の fixture だけ)34.3 s、e51ba9df(2 コミット前)33.7 s、dfb7056b(2026-08-02、`Cargo.lock` の差 3,414 行)91.6 s で `target/debug` 3.6 GB、64903ca5(2026-10-26)255.1 s で 6.0 GB |

選択肢と、それぞれのコストと失うもの:

1. **blob の側だけ木を残す。** 今のまま。コストは今の blob の層(冷えたキャッシュの構築を含めて履歴全体で 82 s、キャッシュ 653 MB)。失うものは無いが、今の木の unit を SCIP から作るなら、HEAD の unit(SCIP)と blob の unit(木)の同一性 `(path, kind, qualified_name)` が同じ文字列にならない。2.4 節の 52 個の違い、impl の名前の書き方、`mod x;` の扱い、`const` の有無がそのまま「HEAD にあるのに履歴が無い」「履歴にあるのに HEAD に無い」になり、系譜の `current_units`(HEAD の unit を同一性で履歴に結ぶ)と `unit_at_before` に効く。2 つの源の unit の規則をそろえる作業が要る。
2. **一部のコミットにだけ SCIP の索引を作る。** 比較の前の側はすでにこの形(`scip_before`)で、冷えた clone で 1 回約 2.8 分(167〜169 s)と 2.5〜2.7 GB の一時的なディスクで済む。全 899 コミットなら、同じ clone で続けて 1 個 34〜255 s として 8.5〜64 時間、索引だけで 899 × 18〜30 MB ≈ 16〜27 GB(DuckDB に写せば小さくなる)。古いコミットほど依存が違い `target/debug` が膨らむ(6.0 GB まで観測)。一部のコミット(比較の両端、タグ、月ごとなど)に限ると、その間のコミットの unit が無いので、`unit_changes`(コミットごとの hunk と両側の unit)と系譜(同じコミットの中のリンク)は作れない。
3. **HEAD の SCIP の定義に hunk を逆にたどる行の追跡(`git log -L` に近い形)。** HEAD の各定義の行の範囲を、各コミットの `-U0` の hunk で過去の行番号に写し、範囲に hunk が当たったコミットを数える。索引は HEAD の 1 つで済み、木は要らない。失うもの:消えた unit(`removed` 8,400 行、HEAD に無い同一性 11,484 個)とそれを使う問い、系譜(ファイルをまたぐ移動・改名・分割。今の履歴の 7,394 / 21,725 行は祖先から来ている。ファイルの改名は `-M` で追えるが、別のファイルへの本体の移動は追えない)、`added` と `modified` の区別(当たったことしか分からない)、比較の前の側(前の木の unit。前の木の索引で代われる)、前置きや入れ子による帰属の規則の過去の側。
4. **blob の unit を木でなく行の規則や正規表現で作る。** 関数・型・impl の見出しの行と括弧の数で unit を切る形。ビルドも木も要らないが、文字列・コメント・マクロの中の括弧、複数行のシグネチャ、`where` 句で境界がずれ、今の木の規則との一致は測っていない(この記録では組んでいない)。
5. **HEAD は SCIP、過去は 1 と同じ木で、HEAD の同一性だけ SCIP の名前に写す表を持つ。** 1 の同一性のずれを、HEAD での対応(2.2 節の 7,732 個の 1 対 1 の対応)で埋める形。対応しない 1,084 個(impl、`mod x;` など)は木の側の同一性のまま残る。

## 4. 頑健性

| 条件 | tree-sitter(`read_ast`) | rust-analyzer の SCIP |
| --- | --- | --- |
| 必要なもの | ファイルの本文だけ。拡張子で言語を決める | cargo が読めるプロジェクト、ツールチェーン、build script と proc macro の実行(依存のダウンロードとビルド。`target/debug` に 2.5〜6.0 GB) |
| 構文の誤り | 誤りのノードを入れて続ける。HEAD の 6,605 blob のうち `ERROR` を含むのは 5 個 | `mode.rs` に `let broken = (1 + ;` を入れても、そのファイルの定義は 88 → 89(壊れた `let` の local が 1 つ増えただけ)で、索引全体も残った |
| `Cargo.toml` の誤り | 影響なし | 索引は作られない(`cargo locate-project` の失敗で 0.7 s で panic) |
| cfg で無効なコード | すべてのファイルとすべての分岐を見る | 有効な cfg だけ。HEAD で関数 19 個と、ファイル 3 個(`horizon-sandbox/src/macos/` の 2 個、`src/preview/guest.rs`)が索引に無い |
| ワークスペースの外 | 見る | 見ない。`duckdb-ext/`(別の cargo ワークスペース)20 ファイル、`preview-plugin/src/lib.rs`、`scripts/refactor-audit/fixtures/*.rs` 2 個 |
| マクロ | 呼び出しの中は `token_tree` で、中の item は見えない | 展開して中の item と参照を記号にする(`actions!` の Struct 18 など)。属性マクロの下の item は範囲が展開から写され、28 個でずれた |
| 時間 | 535 ファイルを 0.5〜1.9 s(sitting_duck の評価) | 167〜255 s(冷えたとき約 2.8 分) |
| 他の言語 | 同じ `read_ast` が grammar のある言語を同じ形で返す。言語ごとの unit の規則(`lang/<language>.sql`)は書く必要がある | 言語ごとに別の索引器(`scip-go`、`scip-typescript`)とそのビルド環境が要る。この記録では Go・TypeScript の索引器を動かしておらず、成熟度は測っていない |

## 5. 表

| 役割 | 今の源 | SCIP で | git で | 構文だけ | 測った一致 | 木を外すと失うもの |
| --- | --- | --- | --- | --- | --- | --- |
| R1 unit の集合と種類(function・method・type・module・macro) | 木 | できる(定義と種類) | — | — | 7,732 / 8,265(impl を除く)、種類は対応分の全件 | cfg で無効なコード 19、属性マクロの下の範囲 28、関連型の種類の写し方 12、ワークスペースの外と無効なファイル 26 ファイル |
| R1 impl の unit | 木 | できない(記号が無い。メンバーから鍵 527) | — | 構文(または見出しの行の規則) | 鍵 527 / unit 551 | impl の範囲(自分の行、空の impl 14、同じ名前の impl の区別 6 組) |
| R1 `mod x;` の宣言 | 木 | 定義としてはできない(参照として 472 / 474) | — | — | — | 宣言を unit とする今の規則(N7 の元でもある) |
| R2 限定名 | 木 + 本文 | できる(記述子) | — | — | 7,680 / 7,732(そろえた後) | 関数の中の item の親の名前 45、属性マクロや別名の trait 名 7。同一性の文字列が変わる |
| R3 範囲 | 木 | できる(`enclosing_range`)。ただし前置き込み | — | — | 終わり全件、始まりは `lead_start_line` と全件 | 前置きを除いた範囲(行の規則で 7,725 / 7,732) |
| R4 `lead_start_line` | 木 | できる(範囲の始まり) | — | — | 7,732 / 7,732 | なし(対応分) |
| R5 入れ子 | 木 | 範囲の包含か記述子で一部 | — | — | 測っていない | method → impl の親、impl の自分の行を引く `innermost` の帰属 |
| R6 `loc` | 範囲 + 本文 | 範囲から(前置き込み) | — | — | — | 前置きを除く数え方(行の規則で代われる) |
| R7 テスト | 木 + パス | 役割の bit は使えない(0) | パスの規則 | 範囲の本文への正規表現 | 3,598 / 3,600、誤り 7 | `#[cfg(test)] impl` の中 2、コメントの中の誤り 7(除ける) |
| R8 マクロの中 | 木 | 呼び出しの名前の位置だけ(12,537) | — | 括弧を本文で数える近似 | 測っていない | 呼び出しの広がり。辺の `via_macro`(5,573 / 34,195 本)がそれで決まる |
| R9 `trait_impl` | 木 | 記述子の `[Trait]` | — | — | — | 印の範囲(impl の範囲) |
| R10 impl の部分 → `impl_for`・`implements` | 木 + SCIP の出現 | 名前で引く形なら 491 / 524 が一致、31 が曖昧 | — | 頭の位置は構文 | 食い違い 0 | 曖昧な 31 個、空の impl。`same_type_unit_edges`(71 本の module 辺に効く) |
| R11 blob の unit | 木(6,605 blob) | 1 コミット 34〜255 s、全 899 で 8.5〜64 時間 | hunk の逆追跡で今の unit の履歴だけ | — | 測っていない | 消えた unit、系譜(履歴の 7,394 / 21,725 行)、`added`/`modified`、前の側の unit |
| R12 比較の前の側 | 木(blob)+ `scip_before` | 前の木の索引でできる(今も参照はそうしている) | — | — | 測っていない | 系譜による対応(`unit_at_before`)は R11 を要る |
| `file_crate`・`module_path` | パス + module unit | 記号の package と記述子 | — | — | crate の数 17 = 17 | — |
| 今は無いもの | — | `const`/`static` 611、マクロが作った item、シグネチャ(可視性・async・引数)、doc コメント 4,839 | — | 複雑さ・入れ子の深さ・unsafe ブロック・パニック・コメント | — | (足せるもの) |

## 6. オーナーが決めること

推奨は書かない。判断の項目だけを挙げる。

1. 今の木の unit を SCIP に移すかどうか。移すなら、impl を unit として残すか(残すなら範囲をどこから取るか:木、見出しの行の規則、メンバーの範囲の外接)、`mod x;` の宣言とファイルのモジュールのどちらを module の unit にするか、`const`/`static`・マクロが作った item・関連型を unit に入れるか。
2. 範囲を前置き込み(SCIP のまま)にするか、前置きを除いた範囲を行の規則で作るか。`all` の帰属、系譜の類似度、出現の所属、`loc` の値が変わる。
3. 限定名(同一性)の書き方をどちらに合わせるか。混ぜるなら、そろえる規則が同一性の一部になる。
4. cfg で無効なコードとワークスペースの外のファイルを unit から外してよいか(HEAD で関数 19・ファイル 26)。
5. テストの判定を、パスの規則と範囲の本文への正規表現にするか(2 個の取りこぼしと、コメントを除く規則)。
6. マクロの中の出現(`via_macro`)を、本文の括弧で数える近似にするか、やめるか、木で残すか。
7. 履歴の unit を 3 節のどの形で持つか(木を blob にだけ残す、索引を一部のコミットに作る、HEAD からの行の追跡、行の規則、その組み合わせ)。それぞれが失うものは 3 節のとおり。
8. 将来の軸(複雑さ、入れ子の深さ、unsafe、パニック、コメント、M3 の網羅的な `match`)の事実を、木から取る前提のままにするか。SCIP のシグネチャで可視性・引数・`async`・`unsafe fn` は取れるが、関数の本体の中の形は取れない。
9. Go・TypeScript を足すときに、言語ごとの索引器とビルド環境を前提にしてよいか。

## 7. 記録や依頼文と食い違った点、気づいた点

- 依頼文は「impl のブロックは記号でなく、メソッドは型の下に掛かる(`Type#method().`)」とした。HEAD の索引では、メソッドの記述子は inherent impl でも `impl#[Type]method().`、trait impl で `impl#[Type][Trait]method().` だった(`impl#[ReportWriter]as_raw_fd().`、`impl#[HubClient][Drop]drop().`)。impl ブロックそのものの記号が無いことは依頼文のとおり。
- 依頼文の数(定義 42,376 すべてに `enclosing_range`、Constant 577、StaticVariable 34、package 17、`Test`・`Generated` が 0)は再測でも同じだった。加えて、参照にも `enclosing_range` が 81,765 件あり、ほとんどは参照先の定義の範囲だった。
- README と依頼文の索引の時間(160–280 s、2–4.5 分)は、冷えた clone では 167〜169 s だった。README の `target/debug` 約 2.7 GB は HEAD で 2.7 GB、e223bc20 で 2.5 GB だったが、同じ clone で古い依存のコミットを続けて索引にすると 6.0 GB まで増えた。
- 軸の記録 1 節の「SCIP の relationships(rust-analyzer 1.99.1 は書かない)」は今回も同じだった。`syntax_kind`・`diagnostics`・`override_documentation` と文書の `text` も書かれていない。
- `rust_tags` の正規表現に相当する本文の規則は、doc コメントの中の `#[cfg(test)]` を拾った。今の木の規則はコメントを属性として読まないので、この誤りは無い。

## 8. 限界

- 対応は名前と終わりの行で取った。名前の違う対応(マクロが名前を作る item など)は数えていない。
- 測ったのは HEAD の 1 つの木と、索引の時間を 5 つのコミットだけである。古い木の索引で unit の一致を測っていない。
- 行の追跡(3 節の 3)と行の規則による blob の unit(3 節の 4)は組んでおらず、一致は測っていない。
- `in_macro` の括弧の近似、`impl_for` の名前による解決の曖昧な 31 個の中身、入れ子の作り直しは測っていない。
- 他の worker が同時に `run.sh` と `lang/rust.sql`、`00_load.sql` を変えている。この記録は 7e84967d の SQL での値である。
