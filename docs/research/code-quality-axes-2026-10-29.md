# コード品質の軸と、コードデータベースから導けるもの(2026-10-29 調査)

オーナーは 2026-10-29 に、コード品質を「継続的にメンテナンスをする上での障害の少なさや機能の足しやすさ」と述べ、思いついた軸としてサイズ・再利用性・意図の明確さの 3 つを挙げた。この記録は、その 3 つのほかに、コードデータベース(`docs/code-database-design.md`、ブランチ `code-quality-survey` の fdfeb492)が持つ事実から**導ける**品質の軸を列挙する。観察の記録であり、どの軸を採るかの推奨は書かない。採否は、この記録をもとに依頼側がオーナーと相談する。

前提として決まっていること(2026-10-28/29 のオーナーの決定。依頼文による):品質は好みではない。重複と置き場所は構造上の性質である。人の判断やプロジェクトの方向に依存するもの(意図した層構造、どの型が契約か、何を障害と見なすか)は、ビューやマクロ(長く残る仕組み)ではなく、設定かデータ(短命で置き換えられるもの)に置く。信号は、この調査のあとで軸を中心に組み直してよい。

## 方法と表記

- 読んだもの:`docs/code-database-design.md`、文献調査 `docs/research/code-quality-analysis-trends-2026-10-24.md`(以下「2026-10-24 の調査」。節はその見出しで引く)、`duckdb-ext/sql/README.md`、`duckdb-ext/sql/lang/rust.sql`、`duckdb-ext/sql/views/history.sql` と `views/graph.sql` の該当箇所、`duckdb-ext/sql/queries/*.sql` の見出し、3 回目の読み判定と後ろ向きの確認の記録。
- ノードの型の確認:`~/.local/bin/duckdb2 -unsigned -no-agent` で `sitting_duck` を `00_load.sql` と同じく `LOAD` し、`read_ast(…, source := 'full', peek := 'none', context := 'none')` をこのリポジトリの数ファイルに読み取り専用でかけた(`src/terminal/session.rs`、`crates/horizon-agent/src/tools/bash/exec/plain.rs`、`crates/horizon-agent/src/persistence/projection/duckdb/shared_store.rs`、`crates/horizon-terminal-core/src/contract.rs`、`src/keymap.rs`、`crates/horizon-terminal-core/src/**/*.rs` の 22 ファイル、`crates/horizon-sandbox-runtime/src/report.rs`)。キャッシュもクローンも使っていない。結果は 1 節にまとめた。
- 文献:2026-10-24 の調査が扱っているものはその節を引き、やり直していない。扱っていないものだけを新たに Web で確かめた。表記は 2026-10-24 の調査に合わせる。★ は一次資料を直接読んだもの(abstract だけなら「★(abstract)」)、無印は検索結果の抜粋や二次資料によるもの、[prior knowledge] は今回取得し直していないもの。今回の検索は合わせて十数回で、「見つからなかった」は「存在しない」の証明ではない。
- 後ろ向きの確認の数字は、3 回目の記録(`code-db-dogfood-retrospective-3-2026-10-28.md`)の最後の列、`production`、1 コミット以上の P@100 で引く:`unit_churn` 0.71、クローンの片側 0.66、fan-in × churn 0.47、co-change without reference 0.40、fan-in 0.32、`loc` 0.75(3 コミット以上ではそれぞれ 0.32、0.18、0.15、0.11、0.06、0.29)。正解は 9 月のスイープが変えた unit で、スイープは古い監査のサイズとクローンの信号で対象を選んだので、これは成果の測定ではなく 2 つの見方の一致である(設計文書の Dogfooding の節)。

## 1. データベースが持つ事実と、ノードの型の確認

軸の導出は、次の事実の上でしかできない。

| 層 | 持つもの | 持たないもの |
| --- | --- | --- |
| ファイル | `files`(path、language、mode、blob_id)。対象は `languages` の拡張子(既定は `.rs` だけ) | `Cargo.toml`、`.md`、設定ファイル(依存の宣言や文書は事実に入っていない) |
| 構文 | `nodes`:`read_ast` の全列(type、行・列・バイト位置、`parent_id`、`depth`、`sibling_index`、`children_count`、`descendant_count`、`scope`) | ノードの本文(バイト列から切り出す)、tree-sitter のフィールド名、名前付きノードかどうかの印(`lang/rust.sql` 冒頭の注記)。`scope` 列は Rust では NULL だった |
| unit | `units`:function、method、impl、type、module、macro。`loc`(空でない行)、入れ子、`lead_start_line` | `const`/`static`、フィールド、`use`、クロージャは unit でない |
| 印 | `tags`:`test`、`trait_impl`、`generated_or_macro`(マクロの呼び出しと定義のすべて) | 歴史の blob の印 |
| 履歴 | `commits`(sha、author_time、author_name、subject、parent_count、is_merge)、`file_changes`(added、deleted)、`hunks`、`unit_changes`、系譜(`unit_successions`、`unit_lineage`、`unit_history`) | コミット本文と trailer(`Co-Authored-By` など)、committer、mailmap、PR やレビューの境界、課題管理の情報(欠陥か機能かのラベル) |
| 参照 | `scip_*`(定義と参照の出現)、`unit_edges`、`unit_fan_in`、`module_edges`、`crate_edges`、`module_cycles`、`impl_for`、`implements` | SCIP の relationships(rust-analyzer 1.99.1 は書かない)。トレイト経由の呼び出しはトレイトのメソッドに解決され、impl のメソッドには届かない。`local` のシンボルは除外。ファイル単位の `pub use` は unit の外 |
| 履歴 × 構造 | `unit_cochange`、`edge_cochange`、`cochange_without_reference` | — |
| クローン | jscpd の 2 つの報告(`clone_pairs`)、`clone_change_consistency`、`clone_site_tags` | jscpd の対象は `jscpd.sh` の既定で `src`、`crates`、`preview-plugin/src` だけ(`duckdb-ext/` の unit にはクローンの事実が無い) |
| 比較 | `compare_units`、`compare_graph`、`compare_cycles` | — |
| 実行 | なし | テストの結果、カバレッジ、ミューテーション、ビルド時間、実行時の情報 |

`read_ast` のノードの型の確認(上のファイルで観察したもの):

- **制御構造**:`if_expression`、`else_clause`、`match_expression`、`match_arm`、`while_expression`、`loop_expression`、`for_expression`、`let_condition`、`try_expression`(子に `?`)、`closure_expression`、`async_block`、`await_expression`、`return_expression`、`break_expression`、`continue_expression` がある。`binary_expression` の演算子は、名前の無いトークンが型として出る(`&&`、`||`、`==` など、1 番目の兄弟)。`depth` 列があり、22 ファイルでの最大は 26 だった。
- **unit の境界と引数**:`parameters` の子に `parameter` と `self_parameter`(と `(`、`,`)がある。
- **可視性**:`visibility_modifier` の子は `pub`、`crate`、`super`、`(`、`)` で、`pub` と `pub(crate)`・`pub(super)` は子の型だけで区別できる(本文の切り出しは要らない)。
- **unsafe**:`unsafe_block` がある。`unsafe fn` は `function_item` の子の `function_modifiers` の下に `unsafe` トークンとして出る(`report.rs:130` の `pub unsafe fn from_raw_fd` で確認。子の並びは `visibility_modifier function_modifiers fn identifier parameters -> type_identifier block`)。`function_modifiers` には `async`、`const` も出る。
- **呼び出し**:`method_call_expression` という型は無い。メソッド呼び出しは `call_expression` の最初の子が `field_expression` で、その中の `field_identifier` がメソッド名である(`call_expression` の最初の子の型は `field_expression`、`identifier`、`scoped_identifier`、`generic_function` だった)。`.unwrap()` を数えるには `field_identifier` の本文を切り出す必要がある。`rust_tags` が属性の識別子でしているのと同じ手順である。
- **マクロ**:`macro_invocation` の子に `identifier`(または `scoped_identifier`)、`!`、`token_tree` がある。`panic!` などはマクロ名の本文を切り出せば区別できる。
- **コメントと文書**:`line_comment`、`block_comment`、`doc_comment`(文書コメントの本文部分、`line_comment` の子)、`outer_doc_comment_marker`(`///`)、`inner_doc_comment_marker`(`//!`)がある。`attribute_item`、`inner_attribute_item` もある。
- **型の抽象**:`type_parameters`(子に `type_parameter`、`lifetime_parameter`)、`where_clause`、`where_predicate`、`trait_bounds`、`generic_type`、`type_arguments`、`dynamic_type`(`dyn`)、`abstract_type`(`impl Trait`)、`bounded_type`、`lifetime`、`reference_type` がある。見たファイルでは数が少なかった(22 ファイルで `type_parameters` 6、`where_clause` 1、`dynamic_type` 3、`abstract_type` 3)。
- **unit でない項目**:`const_item`、`static_item`(後者は見たファイルに無かった。tree-sitter-rust の型名としては [prior knowledge])、`use_declaration`。

## 2. 枠組み:3 つの整理のしかた

### 2.1 ISO/IEC 25010 の保守性の副特性

ISO/IEC 25010:2011 は保守性を modularity(モジュール性)、reusability(再利用性)、analysability(解析性)、modifiability(修正性)、testability(試験性)に分ける(無印。規格本体は読んでいない。TMAP と Sonar の解説ページによる)。2023 年の改訂でも保守性は 9 つの特性の 1 つとして残るが、副特性の一覧が 2011 年版と同じかは確認できなかった(無印)。

前身の ISO 9126 は analysability、changeability、stability、testability だった。Heitlager, Kuipers & Visser(QUATIC 2007, ★ 本文 PDF)によれば、規格が挙げる保守性の測度はどれも保守作業の時間などの事後の観察で、「明日の天気を今日と同じと予報する」ようなもので、ソースコードから予測する力がない。オーナーの 3 軸との対応は、サイズと意図の明確さが analysability に、再利用性が reusability にほぼ重なり、modularity・modifiability・testability は 3 軸の外にある。

### 2.2 SIG の保守性モデル

**2007 年版**(Heitlager et al., ★ 本文 PDF)。ソースコードの性質は volume、complexity per unit、duplication、unit size、unit testing の 5 つで、ISO 9126 の副特性への対応は図 5 の表による(analysability ← volume・duplication・unit size・unit testing、changeability ← complexity・duplication、stability ← unit testing、testability ← complexity・unit size・unit testing。PDF から取り出した文字列は表の罫線を失っており、列の割り当ては行ごとの × の数と [prior knowledge] で補った)。記録しておくべき点:

- unit の循環的複雑度は平均せず、SEI の区分(1〜10、11〜20、21〜50、50 超)ごとに「その区分の unit に入る行の割合」で集約する。複雑度の平均はべき分布の外れ値を隠し、総和は総行数と強く相関するので意味が無い、という理由である。
- 等級の境界は「経験に基づく」もので、多くのシステムの評価で専門家の意見と一致した、とだけ書かれている。結果変数での検証ではない。
- コメント行の数について「一般に保守性とまったく関係が無い」と述べる。多くはコメントアウトしたコードで、複雑だから文書が増えることもある、という理由である。測定ではなく実務者の主張である。
- 論文の版は簡略版で、実際のモデルはモジュール化、アーキテクチャへの準拠、関心の分離なども含むと書かれている。

**2018 年版**。Wijnmaalen, Chen, Bijlsma & Oprescu(SATTOSE 2019, ★ 本文 PDF。著者 4 人のうち 2 人が SIG 所属)によれば、性質は volume、duplication、unit size、unit complexity、unit interfacing、module coupling、component balance、component independence の 8 つで、ISO 25010 の副特性への対応は「専門家の意見に基づく」。component balance と component independence は、25010 で増えた modularity のために足された。生の値は SIG 内部のベンチマークに対する 1〜5 の星に変換される(3 が市場の平均)。表 1 の対応は × の数(analysability 4、modifiability 3、testability 3、modularity 3、reusability 2)まで読めたが、列は PDF の文字列から復元できなかった。

**結果変数との照合**。この論文は Bijlsma, Ferreira, Luijten & Visser(Software Quality Journal 20(2), 2012, 無印。この論文経由)の再現で、同じ 10 の OSS システムの複数のスナップショットについて、保守性の星と課題の解決の速さの星の順位相関を出している。欠陥の解決について、元の研究と 2018 年版での値は次のとおりだった(★、表 5)。

| 性質 | 元(2010 年のモデル) | 2018 年版 |
| --- | --- | --- |
| volume | 0.33 | 0.39 |
| duplication | 0.34 | 0.38 |
| unit size | 0.53 | 0.53 |
| unit complexity | 0.54 | 0.50 |
| unit interfacing | 0.19(p = 0.042) | 0.05(p = 0.640) |
| module coupling | 0.55 | 0.36 |
| component balance | — | −0.34 |
| component independence | — | 0.16(p = 0.201) |
| 保守性の総合 | 0.64 | 0.33 |

機能追加(enhancement)の解決については、元の研究で duplication 0.02(p = 0.448)、unit interfacing 0.10(p = 0.213)、volume 0.61、module coupling 0.69 だった(★、表 6 の左)。読み方の注意(執筆者の推論):システムは 10 しかなく、スナップショットは独立でない。結果変数も SIG の星に変換された値である。著者たちも modularity の負の相関(−0.30)を「直感に反する」と書いている。

ここから言えること:unit size と unit complexity は版をまたいで 0.5 前後で安定している。unit interfacing と component independence は有意でない。新しく足された構造の性質は、期待と逆の向きになったものがある。

### 2.3 プロセスとプロダクト

2026-10-24 の調査の「ホットスポットの考え方そのものより、独立したプロセスメトリクス研究のほうが強い支持を持つ」の節が扱っている。要点は 2 つある。欠陥予測ではプロセス(履歴)のメトリクスが静的メトリクスを大きく上回る(Majumder et al. 2022、★(abstract)、同じ研究グループ)。静的メトリクスはリリース間でほとんど変わらないので、モデルが毎回同じファイルを指す(Rahman & Devanbu, ICSE 2013, 無印。今回 ICSE 2013 のページの抜粋で、12 プロジェクト・85 リリース、「stasis」という主張を再確認したが、本文は読んでいない)。

このデータベースでの観察:後ろ向きの確認では `loc`(プロダクト)が P@100 で 0.75 となり、`unit_churn`(プロセス)の 0.71 以上だった。これは 2026-10-24 の調査と矛盾しない。正解が欠陥ではなく、サイズの信号で対象を選んだスイープだからである(上の注意)。Rahman & Devanbu の stasis は `loc` の順位にそのまま当てはまる。`loc` の上位はスイープの前後でほとんど同じ unit を指し続ける(推論。測っていない)。

## 3. 候補の軸

各軸について、(a) 表す性質、(b) 文献、(c) オーナーの 3 軸との関係、(d) オーナーの目的との関係、(e) 導けるか、(f) 判断に依存する部分を書く。(e) の区分は、**今導ける**(既存のビューで書ける)、**規則か事実を足せば導ける**(何を足すか)、**導けない**(人の入力、つまりデータが要る)の 3 つである。

### 3.1 規模(volume)

- (a) システム全体、crate、モジュールの大きさ。読んで把握すべきものの総量である。
- (b) SIG の両方の版にある(2.2)。欠陥の解決との相関は 0.33〜0.39、機能追加では 0.61(★、Wijnmaalen 2019)。サイズは多くのメトリクスの交絡要因である(2026-10-24 の調査の「関数単位では CC と LOC は冗長でないが、集約すると size に支配される」の節。El Emam 2001 は取得していない)。
- (c) サイズ。
- (d) 規模そのものは機能の量でもあるので、それだけでは障害を示さない。分布(3.7)や成長の速さと組み合わせて意味を持つ(推論)。
- (e) **今導ける**。`units.loc` か `source_lines` を `file_crate`・`unit_module` で合計する。成長の速さは、`compare_units` の両側の `loc`、または `blob_units` を `git_tree` の各時点で合計すれば出る。
- (f) 何をシステムに含めるか(`exclude_patterns`、`languages`、テストを数えるか)。

### 3.2 unit の大きさ(unit size)

- (a) 関数・メソッドの行数。1 つの unit を理解し変更するのに読む量である。
- (b) SIG の両方の版にあり、欠陥の解決との相関は 0.53 で版をまたいで安定している(★)。関数単位では LOC と CC は冗長でない(2026-10-24 の調査の同じ節、Landman et al.、無印)。Clippy の `too_many_lines` は pedantic で、閾値は 100(同調査の「Clippy の size・複雑度 lint の位置づけ」の節、★)。
- (c) サイズ。
- (d) 3 回目の読み判定で `問題あり` とされた原因の多くは、大きな unit や型の中にあった(`Render for WorkspaceShell.render` の 4 つの写しなど)。大きさは障害の場所を指すが、障害の中身は指さない。
- (e) **今導ける**(`units.loc`)。後ろ向きの確認では P@100 0.75 で、どの履歴の信号以上だった(3 コミット以上では 0.29 で、churn の 0.32 に次ぐ)。impl・型・モジュールの大きさは、入れ子の unit を含む `loc` で今出る。innermost の帰属との対応(impl の「自分の行」と中身の行)は設計文書の Unit granularity の未決のとおりである。
- (f) 閾値(2026-10-24 の調査の「閾値はベンチマークに相対的で、言語ごとに違う」の節:Rust のベンチマークは無い。プロジェクト内のパーセンタイルが最も近い)。SIG のように「区分ごとの行の割合」で集約するか、順位で見るか。テストやマクロ由来の unit を数えるか。

### 3.3 unit の複雑さ(循環的・cognitive・入れ子の深さ)

- (a) unit の中の分岐と入れ子。読むときに保持すべき状態の量と、試すべき経路の数である。
- (b) 2026-10-24 の調査の 2 つの節(「cognitive complexity は時間を予測するが、正しさや欠陥はほとんど予測しない」と「関数単位では CC と LOC は冗長でないが…」)のとおり、cognitive は理解時間と r = 0.54、正しさとは −0.13 で(★)、従来のメトリクスを上回らない(無印)。SIG の unit complexity は欠陥の解決と 0.50〜0.54(★)。入れ子の深さについて、Clippy は `cognitive_complexity` を restriction に置き、代わりに `excessive_nesting` と `too_many_lines` を勧めている(★、同調査)。Rust での妥当性の研究は無い。
- (c) 3 軸の外。意図の明確さ(読みやすさ)とサイズの両方に関わる。
- (d) 分岐の多い unit は、機能を足すたびに分岐が増える場所であることが多い(推論)。
- (e) **規則を足せば導ける**。`lang/rust.sql` に、unit のノードの範囲(`node_id` から `node_id + descendant_count` まで。入れ子の unit とクロージャを除くかは選択)の中の判断ノードを数える規則を置く。数える型は `if_expression`、`match_arm`、`while_expression`、`loop_expression`、`for_expression`、`let_condition`、`try_expression`、`binary_expression` の子の `&&`/`||` で、すべて `read_ast` にあった(1 節)。入れ子の深さは、制御構造の祖先の数(`descendant_count` の区間の結合か、`depth` の差)で出る。本文の切り出しは要らない。既存の信号はこれを測っていない。
- (f) 数え方そのものが選択になる。rust-code-analysis の #1249 の 4 つの食い違い(`match` の最後の腕、裸の `loop`、`let … else`、`&&`/`||` の混在。2026-10-24 の調査の「固定版と最新版の差」の節、★ issue)は、SQL で規則を書けば、そのまま自分たちが決めることになる。`?` を分岐と数えるか、クロージャや `async` ブロックを外側の unit に含めるかも同じである。これらは数え方(仕組み)であり、閾値(判断)とは分けられる。

### 3.4 unit の interface(引数の数、公開面、可視性)

- (a) unit を呼ぶ側が知るべきことの量と、外から見える面の広さ。
- (b) SIG の unit interfacing は欠陥の解決と 0.19(p = 0.042)から、2018 年版では 0.05(p = 0.64)になり、機能追加では 0.10(p = 0.213)で有意でない(★)。SIG の版をまたいで最も支持の弱い性質である。Clippy の `too_many_arguments` は complexity 群で、閾値は 7(★、2026-10-24 の調査)。公開面の大きさについては、2026-10-24 の調査は cargo-public-api と cargo-semver-checks の効果の研究を未調査としている。
- (c) 再利用性(SIG 2018 では reusability に対応する、[prior knowledge])と意図の明確さ。
- (d) 公開面が広いほど、変更が外へ伝わる経路が多い。後の 3.6 の契約型(M3)はこの面の上で起きる(推論)。
- (e) 引数の数は**規則を足せば導ける**(`parameters` の子の `parameter`/`self_parameter` を数える。1 節)。可視性は**規則を足せば導ける**(unit のノードの子の `visibility_modifier` と、その子の `crate`/`super`/`(` の有無)。trait impl のメソッドは可視性を持たない(トレイトに従う)。「crate の外から実際に使われている面」は**今導ける**(`unit_edges` のうち `file_crate` が違う辺の宛先)。宣言上の公開 API(`pub use` による再公開を含む)は導けない。ファイル単位の `pub use` は unit の外にあり、SCIP にも relationships が無い(1 節)。新しい事実(cargo-public-api の出力の読み込みなど)が要る。
- (f) 閾値。どの公開を意図したものとするか(ライブラリの API か、内部の都合か)はプロジェクトの判断で、wire の型は意図した公開の例である(3.6)。

### 3.5 重複(duplication)

- (a) 同じか似たコードが複数の場所にあること。片方だけ直される危険と、読む量の増加。
- (b) 2026-10-24 の調査の「どのクローンを統合する価値があるかの証拠は古く、結論も割れている」と「Rust 固有の歪み」の節:害の予測子として支持があるのは、共変更、遅延伝播、既存の不整合、開発者の認識で、長さは支持されない。Rust で最も多い Type-1/2 は型ごとのトレイト実装の写しである。SIG の duplication(システム全体の重複行の割合)は、欠陥の解決と 0.34〜0.38、機能追加とは 0.02(p = 0.448)だった(★)。
- (c) 再利用性(オーナーの 3 軸の「再利用性」の裏面)。
- (d) 3 回目の読み判定の、一方だけ直された写しを後から写した事例(モーダルの開閉、0a921404 → d39ea089、1c27f1ab)は、障害がそのまま表に出た例である。
- (e) **今導ける**。`clone_pairs`、`clone_change_consistency`(片側だけの変更)、`clone_site_tags`(トレイト実装やテストの中か)。後ろ向きの確認でクローンの片側は P@100 0.66。SIG 型の「重複行の割合」も `clone_pairs` の行を unit・モジュールに集約すれば出る。
- (f) jscpd の `--min-tokens 70 --min-lines 5 --mode weak` と、対象のパス(`jscpd.sh` の既定は `src`、`crates`、`preview-plugin/src`)。`exclude_patterns` とは別の対象範囲になっている。意図した重複(`ViewKindState` は `ViewKind` の永続化の写しで、型の文書がそう宣言している。3 回目の読み判定の co-change の 3〜6 番)。言語の定型(トレイト実装の写し、`#[derive]` で済まない手書き実装)。

### 3.6 結合と fan-in/fan-out(module coupling、Unstable Interface、契約型)

- (a) ある unit・モジュールにどれだけ多くが依存し、それがどれだけ多くに依存するか。変更の波及の経路である。
- (b) SIG の module coupling は欠陥の解決と 0.55 → 0.36、機能追加と 0.69(★)。Mo et al.(TSE 2021)の Unstable Interface(高い fan-in と高い変更頻度)と Crossing は、アーキテクチャのアンチパターンの中で寄与が最も大きい(2026-10-24 の調査の「アーキテクチャスメル研究は…」の節、無印)。
- (c) 3 軸の外(modularity)。fan-in は再利用の結果でもあるので、再利用性とは表裏である。多く使われる unit は再利用されている unit である。
- (d) fan-in が高く変わりやすい unit は、変更が多くの場所に伝わる場所である。ただし、3 回目の読み判定では fan-in × churn(`unit_fan_in_churn`)の上位 10 件のうち 6 件が契約の型(M3:`Event`、`ToolCallResult`、`Command` など。皆が使い、伝播をコンパイラが強制する)で、`問題なし` だった。
- (e) **今導ける**。fan-in は `unit_fan_in`(他ファイルの参照数)と `unit_fan_in_churn`、fan-out は `unit_edges` を出側で数える。Crossing は `unit_edges` の入と出と `unit_history` の組み合わせで書ける。M3 の印(型のコミットのうち、その型に辺を張る unit も同じコミットで変わった割合)は、3 回目の読み判定が書いた形で `edge_cochange` から出せる。後ろ向きの確認では fan-in 0.32(基準率 0.29 とほぼ同じ)、fan-in × churn 0.47。
- (f) どの型が契約か(M3)。リポジトリには wire の型の一覧がすでにある(`crates/horizon-acp/schema/acp-ext-wire.json`、`crates/horizon-terminal-core/schema/terminal-wire.json`、`crates/horizon-board/schema/log-wire.json`)。これは契約の一部を名指しするデータとして読めるが、契約がこの 3 つに限られるか(`Event` や `CommandId` を含むか)は判断である。トレイト経由の呼び出しが impl に届かないこと(1 節)は fan-in を数えるときの限界で、判断ではない。

### 3.7 部品の釣り合い(component balance)

- (a) システムが、大きさの揃った少数の部品に分かれているか。1 つの巨大な部品と多くの小さな部品、という形を避けられているか。
- (b) SIG 2018 で足された性質で、欠陥の解決と −0.34(p = 0.001)だった。評価が良いほど解決が遅いという逆の向きである(★)。ほかに検証は見つけていない。
- (c) 3 軸の外(modularity)。サイズの集約のしかたの 1 つでもある。
- (d) 不明。
- (e) **今導ける**。`file_crate` か `unit_module` ごとに `loc` を合計し、部品の数と大きさの偏り(Gini 係数など)を出す。
- (f) 「部品」が何か。crate か、トップレベルのディレクトリか、モジュールか。SIG の論文自身も、部品の定義の曖昧さを外部の確認が要る点として挙げている(★)。このデータベースでは、`file_crate` がこのリポジトリのディレクトリ配置(`crates/<name>/…` など)を `views/graph.sql` の中に書き込んでいる(7 節)。

### 3.8 部品の独立性・循環・層構造(component independence、architecture smells)

- (a) 部品の境界を越える依存の少なさ、循環の無さ、意図した依存の向きが守られているか。
- (b) SIG の component independence は欠陥の解決と 0.16(p = 0.201)で有意でない(★)。Mo et al. の Package Cycle、Modularity Violation、Crossing はアンチパターンに入るが、寄与が大きいのは Unstable Interface と Crossing である(2026-10-24 の調査、無印)。Rust ではモジュール単位の循環の害は検証されていない(同調査)。
- (c) 3 軸の外(modularity)。
- (d) 層を逆に辿る依存があると、下の層に機能を足すときに上の層を知る必要が出る(推論)。
- (e) 循環は**今導ける**(`module_cycles`、`crate_cycles`)。Modularity Violation は `cochange_without_reference` として**今導ける**(後ろ向きの確認で P@100 0.40。3 回目の読み判定の上位 10 件はすべて `問題なし`)。部品の境界を越える依存の割合も `module_edges`・`crate_edges` で**今導ける**。**層の違反は導けない**。意図した層(どのモジュールがどれに依存してよいか)はコードに書かれていないので、人の入力が要る。
- (f) 意図した層構造。標準の例は `board_flow` ↔ `session` の循環で、ボードの入口を `session/` に置くのか、`board_flow/` をボードの唯一の置き場とするのかというオーナーの意図によって、問題なしにも層の逆転にもなる(1 回目の読み判定 4 番、3 回目まで `判断できない`)。`cycle_edges := 'collapsed'` は、親子のモジュールの辺と、1 つの型の impl をファイルに分けた辺を循環から外す。これは「そうした依存は構造上の問題ではない」という Rust の慣習についての判断を、数え方の形で持っている(8 節)。

### 3.9 変更頻度(churn)

- (a) unit がどれだけ頻繁に変わってきたか。変更の集まる場所で、これからも変わる見込みが高い場所である(推論)。
- (b) 2.3 のとおり。ホットスポット(churn × 複雑度)の独立した検証は無い(2026-10-24 の調査)。
- (c) 3 軸の外(履歴)。
- (d) 変更が集まる unit は、機能の追加のたびに触られている unit である可能性が高い。3 回目の読み判定では、churn の上位から一つの大きな unit や型の中の原因に届いた。
- (e) **今導ける**(`unit_churn`、系譜を通した `unit_history`)。後ろ向きの確認で P@100 0.71。
- (f) 期間(`since`、`since_time`。モデルの重要度は時間とともに変わる、McIntosh & Kamei [prior knowledge]、2026-10-24 の調査)。一斉の衛生作業(N3)を数えるか。マージを数えるか(`include_merges`)。系譜の閾値(`lineage_threshold`)。

### 3.10 共変更結合(co-change coupling)

- (a) 参照の有無にかかわらず一緒に変わる unit の組。見えない依存である。
- (b) 2026-10-24 の調査の「共変更結合はパラメータに強く依存する」の節:閾値を上げるとリンクが急減する(MDPI 2024、無印)。アーキテクチャスメルを含む組は共変更率が高い(Bochicchio et al.、無印)。
- (c) 3 軸の外(modularity と履歴)。
- (d) 1 つの機能を足すのに、離れた場所を同時に直す必要がある、という形の障害に直接対応する(推論)。ただし、網羅的な `match` のように、コンパイラが強制する共変更(M3・N6)は障害ではないと判定されてきた。
- (e) **今導ける**(`unit_cochange`、`file_cochange`、`edge_cochange`、`cochange_without_reference`)。
- (f) `min_shared`、`min_coupling_pct`、`max_commit_files`、`basis`。テストの組を外すか(`include_tests`、M5)。リンクのコミット(`cochange_link_commits`)。

### 3.11 変更の広がりと、追加と修正の比(change spread、機能の足しやすさ)

- (a) 1 つのコミット(や 1 つの機能)が、いくつの unit・モジュール・crate を触るか。また、そのコミットの変更のうち、新しい unit の追加と既存の unit の修正がそれぞれどれだけを占めるか。後者はオーナーの「機能の足しやすさ」に最も直接に近い代理である(推論)。足すべき機能が既存のコードの修正なしに新しい unit として足せるなら、足しやすい。
- (b) 変更の散らばりについては、Hassan(ICSE 2009、無印)が、コード変更の散らばりをエントロピーで測り、従来の変更メトリクスより良く欠陥を予測したと報告している。2026-10-24 の調査は「変更の散らばりや波及効果のツール」を未調査としている。追加と修正の比を、機能の足しやすさや開放閉鎖原則の指標として検証した研究は、今回 1 回の検索では見つからなかった。近いものは 2 つあった。1 つは Feathers の「closure of code」(ブログ 2011、無印、検証なし)。もう 1 つは Szeged のグループによる、バージョン管理の操作の種類(追加・更新・削除)と保守性の変化の研究で(無印)、比を指標として扱ったものではない。逆向きの観察として、追加だけで育つコード(既存の unit への追記で機能が増える形、「cumulative code」と呼ぶブログがある、無印)は、それ自体が障害の形でもある。JIT 欠陥予測では追加行数だけのモデルが強い(Zeng et al. 2021、2026-10-24 の調査の同じ節、無印)。
- (c) 3 軸の外(履歴)。オーナーの目的の後半(機能の足しやすさ)そのものに対応する。
- (d) 2 回目の読み判定の `WorkspaceShell.new` は、事象を 1 つ足すと 3 箇所が 1 行ずつ増える、という形の障害だった。これは「機能の追加が既存の unit の修正を伴う」形で、この軸が数えるものに当たる(推論)。
- (e) **今導ける**。`unit_changes` を `sha` で集めると、コミットごとの `added`/`modified`/`removed` の unit 数、触ったモジュール数(`unit_module`)と crate 数が出る。エントロピーは `file_changes` の `added + deleted` から出る。unit ごとに「その unit を修正したコミットのうち、別の unit を追加したコミットの割合」を出せば、「機能を足すたびに触られる unit」(上の `WorkspaceShell.new` の形)を指せる。
- (f) どのコミットが「機能の追加」か。事実には件名(`subject`)しか無く、ラベルも PR の境界も無い。分類は人の入力か、件名の慣習の取り決め(データ)が要る。一斉の衛生作業(N3)と移動のコミット(系譜のリンク)を外すか。エージェントのコミットの粒度が、コミットあたりの広がりを変えるかは定量化されていない(2026-10-24 の調査の「エージェントのコミットと squash は…」の節)。

### 3.12 安定性と年齢

- (a) unit がいつ作られ、最後にいつ変わったか。長く変わっていないコードは安定しているか、放置されているかのどちらかである。
- (b) 年齢と欠陥の関係は Graves et al.(TSE 2000)の [prior knowledge] で、今回取得していない。2026-10-24 の調査は直接扱っていない。
- (c) 3 軸の外(履歴)。
- (d) 不明(向きが一意でない)。
- (e) **今導ける**。`unit_history` を `commits.author_time` に結合して、最初と最後のコミットの時刻を出す。系譜を通すので、移動や改名で年齢がリセットされない。
- (f) 「古い」の基準。系譜の閾値。

### 3.13 所有と bus factor

- (a) unit やモジュールの変更が、何人に、どれだけ偏っているか。知識の集中である。
- (b) Bird et al.(ESEC/FSE 2011, ★(abstract)):Windows Vista と 7 で、低い専門性の貢献者の数と、最大の所有者の割合が、リリース前の欠陥とリリース後の障害に関係した。Greiler, Herzig & Czerwonka(MSR 2015、無印)は Microsoft の 4 製品で再現した。Avelino et al.(ICPC 2016, ★(abstract)):GitHub の 133 システムで truck factor を推定し、65 % が 2 以下だった。
- (c) 3 軸の外(組織)。
- (d) オーナー 1 人とエージェントで開発しているこのリポジトリでは、人で数えると退化する見込みが高い(推論。測っていない)。「どのエージェントのセッションが書いたか」なら意味を持ち得るが、それは事実に無い。
- (e) 作者名でなら**今導ける**(`unit_history` と `commits.author_name`)。ただし `author_name` は `%an` で、mailmap も無い。エージェントの区別には**事実を足す必要がある**:`git_commits` にコミット本文か trailer(`Co-Authored-By`)の列を足す(新しい列)。
- (f) 誰を同じ人と見なすか(mailmap 相当のデータ)。エージェントと人を分けるか。

### 3.14 テスト(比率、テストからの fan-in、アサート、ミューテーション)

- (a) 変更したときに壊れたことに気づける程度。testability。
- (b) SIG 2007 の unit testing はカバレッジとアサートの数で、アサートの数はカバレッジの確認にだけ使い、等級は無い(★)。ミューテーションスコアの妥当性は研究どうしで対立している(2026-10-24 の調査の「テストの感度:cargo-mutants と…」の節)。Rust のテストスメルや、ミューテーションスコアと欠陥の関係の研究は未調査(同調査)。
- (c) 3 軸の外(testability)。
- (d) 機能を足したときに既存の振る舞いを壊していないと確かめられることは、足しやすさの前提である(推論)。
- (e) テストの比率(テストの unit の行と本番の行)は**今導ける**(`tags` の `test`、`unit_tests`)。本番の unit ごとに、それを参照するテストの unit の数(テストからの fan-in)も、`unit_edges` の `src_is_test AND NOT dst_is_test` で**今導ける**。ただし `tests/` 下の統合テストは別のターゲットで、同じパッケージのシンボルが曖昧になる場合がある(`ambiguous`)。アサートの数は**規則を足せば導ける**(`macro_invocation` のマクロ名の本文を切り出し、`assert`、`assert_eq` などを数える)。カバレッジとミューテーションは**事実を足す必要がある**:cargo-mutants の出力か LCOV を `read_json` などで読む。jscpd の報告と同じく、外の道具を走らせた結果になる。
- (f) テストの方針(M5、`include_tests`)。何をテストと見なすか。`rust_tags` は `#[test]`、`rstest`、`test_case`、`bench` の属性と、`tests/`、`tests.rs`、`*_tests.rs`、`test_*.rs` のパスを使う。これは言語とこのリポジトリの慣習の混合である。

### 3.15 意図の明確さ:文書の有無、コメント、命名

- (a) コードが何のために何をするかが、読み手に伝わるか。オーナーの 3 軸の 1 つである。
- (b) 2026-10-24 の調査の「計算された理解性スコアは実際の理解を捉えない」の節:どのメトリクスも理解性を捉えず、組み合わせても実用に足りない(Scalabrino、無印)。SIG 2007 はコメント行の数を保守性と無関係と述べる(★、主張であって測定ではない)。命名について、Butler et al.(CSMR 2010、無印)は、欠陥のある識別子名を持つ Java のクラスが、静的解析で低品質とされたコードと結び付くと報告した。結果変数は静的解析で、保守の工数ではない。文書の有無を保守性の結果変数と照らした研究は、今回は探していない。
- (c) 意図の明確さ。
- (d) 意図が読み取れないと、機能を足す人(エージェントを含む)が既存の約束を壊す。ただし、それを測る妥当な方法の証拠は弱い。
- (e) 文書の有無は**規則を足せば導ける**。`lead_start_line` がすでに先頭の文書と属性の範囲を出しており、`outer_doc_comment_marker` の有無で、文書があるかを区別できる(1 節)。公開の unit のうち文書の無いものの割合も、3.4 の可視性の規則と合わせれば出る。コメントの密度は `line_comment`/`block_comment` の数で**規則を足せば導ける**。命名は、機械的な性質(長さ、略語、同じ概念に違う語を使っているか)なら本文の切り出しで導けるが、名前が意図に合っているかは**導けない**(人の判断か、2026-10-24 の調査が不安定と記録した LLM の判定が要る)。文書とコードの食い違いも**導けない**。
- (f) どの unit に文書が要るか(公開の unit だけか)。何を良い名前とするか(語彙の取り決めはデータになる)。

### 3.16 死んだコード(参照ゼロ)

- (a) どこからも使われていない unit。読む量を増やし、変更のときに無駄に直される。
- (b) Romano, Vendome, Scanniello & Poshyvanyk(TSE 46(1), 2020、無印。インタビューと 4 つの実験。abstract の抜粋だけ)は、死んだコードは広く存在し、避けるべきだと結論している。効果量は読んでいない。
- (c) サイズ(不要な量)と意図の明確さ。
- (d) 不明。
- (e) **今導ける(近似)**。`unit_edges` で入る辺が無く、テストでなく、入口でない unit。ただし誤検出の仕組みがはっきりしている。トレイト経由の呼び出しはトレイトのメソッドに解決され impl のメソッドに届かないので、トレイト実装のメソッドは参照ゼロに見える。マクロの本体からの参照は SCIP に無い。ほかに `main`、テスト、GPUI のコールバック、`#[derive]`、FFI、wasm の出口などの入口がある。Rust のコンパイラの `dead_code` 警告は非公開の項目をすでに見つけるので、この軸が新たに見つけるのは「ワークスペースのどこからも使われていない `pub` の項目」に限られる(推論)。
- (f) 入口の一覧(言語の慣習とプロジェクトの配置の混合)。意図して残しているもの(将来の API、プレビュー用のサンプル)。

### 3.17 Rust 固有:unsafe、パニック、マクロ、ジェネリクス

- (a) 失敗の扱いの明示性(`unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`unreachable!`)、安全性の境界(`unsafe`)、メタプログラミング(マクロ)、抽象化の深さ(ジェネリクス、トレイト境界、`dyn`、`impl Trait`)。
- (b) 2026-10-24 の調査の「Rust 固有の品質研究は unsafe 中心で、スメル目録は存在しない」の節:`unwrap`・`clone`・ライフタイムの数を欠陥や工数と照らした検証は見つからなかった。cargo-geiger は unsafe の量を測る道具で、保守性の指標ではない。マクロは、クローン検出の結果を歪めるものとして扱われている(同調査の「Rust 固有の歪み」の節)。
- (c) 3 軸の外。抽象化の深さは再利用性と意図の明確さの両方に関わる。抽象は再利用のための手段だが、過剰になると読みにくくなる(推論。AI 生成コードの過剰な抽象を厳密に調べた研究は見つからなかった、2026-10-24 の調査)。
- (d) 本番コードのパニックは、機能を足したときの想定外の入力で落ちる場所である(推論)。
- (e) マクロの使用は**今導ける**(`tags` の `generated_or_macro`、`units` の `macro`)。`unsafe` は**規則を足せば導ける**(`unsafe_block`、`function_modifiers` の下の `unsafe`。1 節)。パニックの数は**規則を足せば導ける**(`call_expression` → `field_expression` → `field_identifier` の本文が `unwrap`/`expect`、`macro_invocation` のマクロ名。本文の切り出しは `rust_tags` の識別子と同じ手順)。テストの印と組み合わせれば本番コードに絞れる。ジェネリクスと抽象は**規則を足せば導ける**(`type_parameters`、`where_clause`、`trait_bounds`、`dynamic_type`、`abstract_type`)。
- (f) どのパニックを許すか(`expect` で不変条件を書く慣習、テストでの `unwrap`)。これは言語の慣用とプロジェクトの方針で、データになる。`unsafe` をどこに許すか(サンドボックスの FFI など)。

### 3.18 自己申告の技術的負債(TODO/FIXME コメント)

- (a) 書いた人自身が不完全だと印を付けたコード。
- (b) Potdar & Shihab(ICSME 2014)以降の SATD 研究は [prior knowledge] で、今回取得していない。2026-10-24 の調査も扱っていない。
- (c) 意図の明確さ(意図の不足を書き手が明示したもの)。
- (d) 不明。
- (e) **規則を足せば導ける**(`line_comment`/`block_comment` の本文を切り出し、語で探す)。
- (f) どの語を負債の印とするか(データ)。

### 3.19 再利用性そのものについて

オーナーの 3 軸の「再利用性」は、データベースの上では 1 つの測度にならない。表に出るのは次の 3 つの側面である。重複(3.5、再利用の欠如)、fan-in(3.6、再利用の結果。ただし結合でもある)、interface の小ささと抽象(3.4、3.17、再利用のしやすさ)。SIG 2018 の reusability は unit size と unit interfacing から作られ([prior knowledge])、欠陥の解決とは 0.46 だった(★)。この 3 つは向きが一致しない。再利用が多いほど fan-in が高くなり、結合の信号では悪く見える。どの側面を「再利用性」と呼ぶかは判断である。

## 4. 判断とプロジェクトに依存する部分を、データとして置く形

軸ごとの (f) に出た判断を、種類ごとにまとめる。どれも、ビューが読むデータとして置ける。置き方の選択肢とその帰結を並べる(どれを選ぶかは書かない)。

| 判断の種類 | 例 | 置き方の選択肢 | 帰結 |
| --- | --- | --- | --- |
| 閾値 | unit の大きさ・複雑さの区分、`min_shared`、`min_coupling_pct`、`max_commit_files`、`lineage_threshold` | (1) DuckDB の変数(今の形)。(2) TOML の設定ファイル。(3) `duckdb-ext/sql/` 下の CSV。(4) 閾値を持たず、プロジェクト内のパーセンタイルや順位だけを出す | (1) はセッションごとで、記録に残らない。(2)(3) はリポジトリに残り差分が見えるが、読み込む仕組みが要る。(4) は閾値がベンチマーク相対だという証拠(2026-10-24 の調査)と整合するが、「どこからが問題か」は読み手に残る |
| 意図した層構造 | `board_flow` ↔ `session`、どのモジュールがどこに依存してよいか | (1) キャッシュの中の表(`allowed_module_edges(src_pattern, dst_pattern, note)`)。(2) TOML。(3) CSV | 層の定義はモジュールの改名で古くなる。パターン(パスの GLOB)で書けば改名に一部耐えるが、意図とずれた一致が起きる。キャッシュの表はリポジトリの外にあり、共有されない |
| 契約の型(M3) | `Event`、`CommandId`、wire の型 | (1) 型の名前の一覧(CSV か TOML)。(2) wire の schema の成果物(`crates/*/schema/*-wire.json`)を読む。(3) 印の列だけを出し(3 回目の読み判定の案)、一覧は持たない | (2) はすでに生成され、ゲートで最新に保たれているが、wire の外の契約は含まない。(3) は判断をすべて読み手に残す |
| 意図した重複 | `ViewKindState`(永続化の写し)、型ごとのトレイト実装 | (1) 組の一覧(パスと名前の組)。(2) ソースの中の印(属性やコメント)を規則で読む。(3) `clone_site_tags` のような分類で一括に分ける | (1) はコードの移動で古くなる。系譜でたどれば一部は追える。(2) はコードの中に方針を書くことになる。(3) は言語の定型には効くが、個別の意図は表せない |
| テストの方針 | M5(`include_tests`)、テストの判定のパスと属性 | (1) 変数(今の形)。(2) テストの判定の規則のうち、パスの部分を CSV や TOML に出す | 今はパスの規則が `lang/rust.sql` の中にあり、言語の知識とリポジトリの慣習が混ざっている |
| 言語の慣用 | `expect` を許すか、`cycle_edges` が外す構造上の辺、入口の一覧 | (1) `lang/<language>.sql` に置く(言語の知識はそこだけ、という今の規約)。(2) 言語ごとの設定ファイル | 言語の慣用はプロジェクトをまたいでほぼ共通で、長く残る。一方で、何を許すかはプロジェクトが決める部分もある |
| 対象範囲 | `exclude_patterns`、`languages`、`include_untracked`、jscpd の対象パス、`file_crate` の配置 | (1) 変数(今の形)。(2) リポジトリごとの設定ファイル(`target/code-db/` か、リポジトリの直下) | 今は範囲の定義が 3 箇所(変数、`jscpd.sh`、`views/graph.sql` の `file_crate`)に分かれている。`duckdb-ext/` は unit の範囲に入るが jscpd の範囲に入らない |
| 何を障害とするか | 軸ごとの向き(fan-in は良いか悪いか)、軸の組み合わせ方 | (1) 重みや向きの表。(2) 持たず、軸ごとの順位を並べて出す | 組み合わせのモデル(設計文書の「Deferred consultations」)はこれに当たる |
| 機能の追加の分類 | 3.11 のコミットの分類 | (1) コミットごとのラベルの表(sha とラベル)。(2) 件名の慣習の規則。(3) 分類しない | (1) は人の手間が要る。(2) はこのリポジトリの件名に慣習が無いので、今は効かない |
| 同一人物の扱い | 3.13 | mailmap 相当の対応表 | — |

### 4.1 今のパラメータの分類:仕組みと判断

README の Parameters の表の変数を、仕組み(どう数えるか、どこから読むか、どれだけ資源を使うか)と、判断(何を数えるか、何を問題とするか)に分ける。境界にあるものは「混合」とし、理由を書く。

| 変数 | 分類 | 理由 |
| --- | --- | --- |
| `repo` | 仕組み | 入力の場所 |
| `memory_limit`、`threads`、`temp_limit`、`temp_free_bytes` | 仕組み | 資源 |
| `cache_path`、`scip_index`、`blob_dir`、`clone_reports` | 仕組み | 入力と出力の場所 |
| `scope_paths`、`lineage_scope` | 仕組み | `refresh.sql` の差分更新の内部 |
| `blob_include_merges` | 仕組み | 歴史の blob の網羅性 |
| `compare_before`、`scip_before` | 仕組み | 問いの引数(何と比べるか) |
| `path`、`unit_kind`、`qualified_name`、`depth`、`direction`、`crate`、`kind`、`match_mode`、`clone_source` | 仕組み | 問いの引数や表示の絞り込み |
| `attribution` | 仕組み | 変更をどの unit の責任とするかの数え方。既定はオーナーが決めた(2026-10-27) |
| `cochange_link_commits`、`clone_before_both`、`clone_sides` | 仕組み | 数え方の規則(移動を共変更と数えない、両側がそろってから数える、側の解決) |
| `basis` | 仕組み | 結合の分母の定義 |
| `exclude_patterns`、`languages`、`include_untracked` | 判断 | 何をプロジェクトのコードとするか |
| `since`、`since_time` | 判断 | どこまでの履歴を今に関係するとするか |
| `include_tests` | 判断 | テストの方針(M5) |
| `min_shared`、`min_coupling_pct`、`max_commit_files` | 判断 | 閾値。どれも検証されていない(2026-10-24 の調査) |
| `min_fan_in` | 判断 | 表示の閾値 |
| `include_merges` | 混合 | 数え方だが、マージでの衝突の解決を「変更」と見なすかの判断を含む |
| `lineage_threshold` | 混合 | 数え方(gix の改名の既定に合わせた 0.5)だが、どこまで書き換えても同じコードと見なすかの判断である |
| `cycle_edges` | 混合 | 数え方の規則だが、親子のモジュールの辺と、型の impl の分割を問題としないという慣用の判断を持つ |

パラメータでない場所にある判断:`file_crate`(このリポジトリのディレクトリ配置、`views/graph.sql`)、テストの判定のパスと属性名(`lang/rust.sql`)、`generated_or_macro` がすべてのマクロ呼び出しであること(`lang/rust.sql`)、jscpd の `--min-tokens 70 --min-lines 5 --mode weak` と対象パス(`jscpd.sh`)、`local` のシンボルの除外(`views/graph.sql`。これは仕組み)。

## 5. 表

証拠の強さは、文献が結果変数(欠陥、保守の工数、変更しやすさ)との関係について言える程度を、次の 4 段階で書く。強:複数の独立した研究がある。中:結果変数との関係があるが、単一のグループか、ベンダーか、小さい標本。弱:関係が小さい、有意でない、または結果が割れている。なし:結果変数との検証が見つからない。Rust での検証はどの軸にも無い(2026-10-24 の調査)。後ろ向きの確認の値は P@100(`production`、1 コミット以上)。

| 軸 | 性質 | 証拠の強さ | オーナーの軸 | 導出 | 判断の部分 |
| --- | --- | --- | --- | --- | --- |
| 3.1 規模 | 読むべき総量 | 中(SIG ★。交絡要因) | サイズ | 今(`units.loc` の合計) | 対象範囲、テストを含むか |
| 3.2 unit の大きさ | 1 unit を読む量 | 中(SIG で 0.53 安定 ★) | サイズ | 今(`units.loc`。P@100 0.75) | 閾値、集約のしかた |
| 3.3 unit の複雑さ・入れ子 | 分岐と状態の量 | 弱〜中(理解時間 r = 0.54、正しさは無関係 ★。SIG 0.50〜0.54 ★) | 外(サイズと意図に跨る) | 規則を足す(`lang/rust.sql`、型は確認済み) | 数え方(#1249 の 4 点、`?`、クロージャ)、閾値 |
| 3.4 interface | 引数・公開面・可視性 | 弱(SIG で有意でない ★) | 再利用性・意図 | 引数と可視性は規則を足す。使われている公開面は今。宣言上の公開 API は事実を足す | 閾値、意図した公開 |
| 3.5 重複 | 写しの存在と片側の変更 | 中(不整合・共変更は支持、長さは不支持。2026-10-24 の調査) | 再利用性 | 今(`clone_*`。P@100 0.66) | jscpd の設定と範囲、意図した重複、言語の定型 |
| 3.6 結合・fan-in/out | 波及の経路 | 中(SIG 0.36〜0.69 ★。Mo et al. 無印) | 外(modularity。再利用性と表裏) | 今(fan-in 0.32、× churn 0.47)。M3 の印も今 | 契約の型 |
| 3.7 部品の釣り合い | 部品の大きさの偏り | 弱(SIG 2018 で逆向き ★) | 外(modularity) | 今(合計と偏り) | 部品の定義(`file_crate`) |
| 3.8 独立性・循環・層 | 境界を越える依存、循環、層の向き | 弱(SIG で有意でない ★。循環の Rust 検証なし) | 外(modularity) | 循環・越境・Modularity Violation は今(0.40)。層の違反は導けない | 意図した層、`cycle_edges` の慣用 |
| 3.9 churn | 変更の集中 | 中〜強(欠陥予測、2026-10-24 の調査) | 外(履歴) | 今(0.71) | 期間、N3、マージ、系譜の閾値 |
| 3.10 共変更 | 見えない依存 | 中(パラメータに強く依存) | 外(modularity・履歴) | 今 | 閾値、テスト、リンクのコミット |
| 3.11 変更の広がり・追加と修正の比 | 機能の足しやすさ | 広がり:弱〜中(Hassan 2009 無印)。比:なし(検索 1 回で見つからず) | 外(目的に直結) | 今(`unit_changes` を sha で集約) | 機能の追加の分類、N3、移動のコミット |
| 3.12 安定性・年齢 | 作られた時期・最後の変更 | なし(今回は未取得) | 外(履歴) | 今 | 「古い」の基準 |
| 3.13 所有・bus factor | 知識の集中 | 中(Bird ★(abstract)、Greiler 無印、Avelino ★(abstract)) | 外(組織) | 作者名なら今(退化の見込み)。エージェントの区別は事実を足す(trailer の列) | 同一人物の扱い |
| 3.14 テスト | 壊れたことに気づけるか | 弱(ミューテーションは対立) | 外(testability) | 比率とテストからの fan-in は今。アサートは規則を足す。カバレッジ・ミューテーションは事実を足す | テストの方針(M5)、テストの判定 |
| 3.15 文書・コメント・命名 | 意図の伝わりやすさ | 弱〜なし(理解性スコアは理解を捉えない。SIG はコメント無関係と主張) | 意図の明確さ | 文書の有無とコメントの密度は規則を足す。命名の適否と文書の食い違いは導けない | どこに文書が要るか、語彙 |
| 3.16 死んだコード | 使われない unit | 弱(Romano 無印、効果量未読) | サイズ・意図 | 今(近似。トレイト実装のメソッドが誤検出) | 入口の一覧、意図して残すもの |
| 3.17 Rust 固有 | unsafe・パニック・マクロ・抽象 | なし(Rust の検証なし) | 外(抽象は再利用性・意図に跨る) | マクロは今。unsafe・パニック・ジェネリクスは規則を足す | 許すパニック、unsafe の置き場 |
| 3.18 SATD | 書き手が印を付けた負債 | 未確認([prior knowledge]) | 意図の明確さ | 規則を足す(コメントの本文) | 印とする語 |

### 5.1 2026-10-24 の調査の「未検証・未調査」のうち、この問いに関わるもの

- Rust で静的メトリクスやスメルを理解・変更・欠陥と照らした研究が無い(3.2〜3.4、3.17 のすべて)。`unwrap`・`clone`・unsafe・ライフタイムの数の検証も無い(3.17)。
- 2022〜2026 年の、CC が LOC を超えて予測するかの研究、MI の検証、ROC に基づく閾値の研究(3.2、3.3、4 節の閾値)。
- El Emam et al. 2001 のクラスサイズの交絡(3.1、軸をサイズで割るかどうか)。
- Rahman & Devanbu 2013 の効果量、Kamei et al. 2013、McIntosh & Kamei 2018([prior knowledge] のまま。2.3、3.9、3.11)。
- squash とエージェントのコミットが churn・結合・欠陥予測に与える影響(3.9〜3.11、3.13)。最大 changeset サイズの効果(`max_commit_files`)。
- 変更の散らばりや波及効果のツールは一次資料で確認していない(3.11)。
- 循環の効果量(3.8)。
- Rust のミューテーションスコアと欠陥の関係、Rust のテストスメル(3.14)。
- cargo-semver-checks、cargo-public-api、cargo-udeps、cargo-machete の効果の研究(3.4、3.16)。
- 欠陥予測の順位がリファクタリング候補の順位として良いかの評価が無い(どの軸にも共通。後ろ向きの確認の正解がスイープの選択であることと同じ問題)。

今回新たに残ったもの:ISO/IEC 25010:2023 の保守性の副特性の一覧(規格本体を読んでいない)。SIG 2018 の性質と副特性の対応の列(PDF の表を復元できなかった)。Bijlsma et al. 2012、Greiler et al. 2015、Romano et al. 2020、Hassan 2009、Butler et al. 2010 の本文。年齢(Graves et al.)と SATD(Potdar & Shihab)の一次資料。文書の有無を保守性の結果変数と照らした研究と、追加と修正の比を機能の足しやすさとして検証した研究(前者は探しておらず、後者は 1 回の検索で見つからなかった)。

## 6. 2026-10-24 の調査や設計文書と食い違う点、緊張のある点

- **直接に矛盾する点は見つからなかった。** 後ろ向きの確認で `loc` が `unit_churn` 以上だったこと(2.3)は、プロセスがプロダクトを上回るという調査の結論と矛盾しない。正解が欠陥ではなく、サイズの信号で選ばれたスイープだからである。設計文書の Dogfooding の節もそう注記している。
- **プロジェクトに依存する判断が、ビューの中にある。** `file_crate`(`views/graph.sql`)は、このリポジトリのディレクトリ配置を SQL に書いている。2026-10-28/29 の決定(プロジェクトに依存するものはビューではなく設定かデータに置く)と緊張がある。テストの判定のパスの規則(`lang/rust.sql`)も、言語の慣習とこのリポジトリの慣習が混ざっている。
- **対象範囲の定義が 3 箇所にある**(4 節の表)。`duckdb-ext/` の unit はクローンの事実を持たないので、クローンの信号ではいつも 0 に見える。
- **複雑度の規則を SQL で書くと、#1249 の数え方の問題が自分たちの決定になる。** 2026-10-24 の調査は rust-code-analysis の数え方の偏りを記録したが、代わりの規則も同じ選択を迫られる。
- **SIG の結果は、このデータベースの重複の信号とは別の量を測っている。** SIG の duplication(システム全体の重複行の割合)は機能追加の解決とほぼ無関係だった(0.02、★)。データベースの信号は、片側だけの変更という履歴の量である。これは調査の「長さではなく不整合と共変更」という結論と整合し、矛盾はしない。

## 参照した資料(今回新たに確かめたもの)

- Heitlager, Kuipers & Visser, "A Practical Model for Measuring Maintainability — a preliminary report", QUATIC 2007 — https://www.di.uminho.pt/~joost/publications/HeitlagerKuipersVisser-Quatic2007.pdf(★ 本文。PDF の文字列から読んだ)
- Wijnmaalen, Chen, Bijlsma & Oprescu, "The Relation between Software Maintainability and Issue Resolution Time: A Replication Study", SATTOSE 2019 — https://ceur-ws.org/Vol-2510/sattose2019_paper_11.pdf(★ 本文。同上)
- Bird, Nagappan, Murphy, Gall & Devanbu, "Don't Touch My Code! Examining the Effects of Ownership on Software Quality", ESEC/FSE 2011 — https://www.microsoft.com/en-us/research/?p=161465(★(abstract))
- Avelino, Passos, Hora & Valente, "A Novel Approach for Estimating Truck Factors", ICPC 2016 — https://arxiv.org/abs/1604.06766(★(abstract))
- Greiler, Herzig & Czerwonka, "Code Ownership and Software Quality: A Replication Study", MSR 2015 — https://www.microsoft.com/en-us/research/publication/code-ownership-and-software-quality-a-replication-study/(無印)
- Rahman & Devanbu, "How, and Why, Process Metrics Are Better", ICSE 2013 — https://2013.icse-conferences.org/content/how-and-why-process-metrics-are-better.html(無印)
- Hassan, "Predicting faults using the complexity of code changes", ICSE 2009, DOI 10.1109/ICSE.2009.5070510(無印)
- Romano, Vendome, Scanniello & Poshyvanyk, "A Multi-Study Investigation into Dead Code", TSE 46(1), 2020, DOI 10.1109/TSE.2018.2842781(無印)
- Butler, Wermelinger, Yu & Sharp, "Exploring the Influence of Identifier Names on Code Quality: An Empirical Study", CSMR 2010(無印)
- ISO/IEC 25010 の解説:https://www.sonarsource.com/resources/library/iso-iec-25010-explained/ 、https://www.tmap.net/?p=7672(無印)
- 追加と修正の比に近いもの:https://www.gbgames.com/2011/01/25/measuring-the-closure-of-code/ 、https://www.inf.u-szeged.hu/~ferenc/publication/fhf14 、https://hackernoon.com/cumulative-code-smell-6d5344646c46(無印)
