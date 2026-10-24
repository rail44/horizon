# コードデータベースのスパイク結果(2026-10-24)

オーナーの指示は「スパイクしてみましょう。できれば対象になる言語は非依存として、goやtypescriptなどの別プロジェクトでの活用が出来る形を目指したいです。」である。その前段の案は「ast-grepのような構文木と、依存グラフをそれぞれそのままグラフDBとして保存したものduckdbでクエリして解析させるのはどうか」であった。実装は worker(Opus)が `scripts/code-db/` に行い(worker ブランチ `worktree-agent-a3786800584507bd3` の fec909ce、`code-quality-survey` へは 4ad82864 として取り込み)、本記録は依頼側のセッションが自分の作業ツリーで再現した数値を記す。設計と限界の説明は `scripts/code-db/README.md`、問いの文献上の根拠は [code-quality-analysis-trends-2026-10-24.md](code-quality-analysis-trends-2026-10-24.md)、既存の DuckDB 拡張との対応は [duckdb-extension-options-2026-10-24.md](duckdb-extension-options-2026-10-24.md) にある。

## 再現した構築

| 項目 | 値 |
| --- | --- |
| 入力 | Rust 515 ファイル、1,585 コミット(マージ含む)、jscpd 5.3.1 の 2 レポート(識別子保持・正規化)、rust-analyzer 1.99.1 の SCIP 索引(30 MB、生成 159 s) |
| 時間 | 22.2 s(構文 5.2 / 履歴 15.2 / クローンと SCIP 1.7)、ピーク RSS 0.82 GB |
| 行数 | nodes 1,514,979 / units 8,878 / tags 14,414 / commits 1,585 / file_changes 8,966 / hunks 34,690 / unit_changes 40,332 / clone_pairs 1,502 / scip_symbols 42,808 / scip_occurrences 301,687 |
| parse cache | 12,451 lookups、5,893 hits(47%)、6,558 blob を parse |
| テスト | `build.py verify` 15 件 OK(fixture リポジトリは Rust・Go・TypeScript)。`refactor-audit/run.py verify` 27 件 OK(worker 確認、既存ツールは無変更) |
| 決定性 | 同じ木からの 2 回の構築で表の内容が一致(worker 確認) |

依存は `duckdb==2.0.0`、`tree-sitter==0.25.2`、`tree-sitter-rust==0.24.2`、`tree-sitter-go==0.25.0`、`tree-sitter-typescript==0.23.2` で、`build.py.lock` にハッシュつきで固定されている。SCIP は protobuf パッケージも別バイナリも使わず、`scip.py` が wire format を直接読む。

## クエリの出力(上位)

`unit_churn`(非マージコミット数の多い unit):

| unit | kind | loc | commits |
| --- | --- | --- | --- |
| `src/terminal/session.rs` `TerminalSession` | impl | 398 | 46 |
| `src/workspace/session_lifecycle.rs` `WorkspaceShell` | impl | 490 | 44 |
| `src/workspace/commands.rs` `WorkspaceShell` | impl | 928 | 42 |
| `crates/horizon-agent/src/tools/catalog.rs` `definitions` | function | 3 | 42 |
| `crates/horizon-agent/src/tools/state.rs` `ToolSessionState` | impl | 223 | 37 |
| `TerminalSession.spawn` | method | 99 | 32 |
| `WorkspaceShell.execute` | method | 204 | 30 |

全 unit で著者は最大 2 名。

`file_cochange_thresholds`(非マージ、1 コミット 50 ファイル以下、共有 3 コミット以上):

| 閾値 % | 対の数(max 基準) | 対の数(average 基準 = Code Maat) |
| --- | --- | --- |
| 10 | 1,503 | 1,809 |
| 20 | 791 | 1,125 |
| 30 | 400 | 632 |
| 40 | 238 | 384 |
| 50 | 139 | 233 |

30% での上位は `src/agent/model/tests.rs`–`types.rs`(4/4/4、100%)、`src/preview/tests.rs`–`watch.rs`(3、100%)、`src/theme/oklab.rs`–`oklab/tests.rs`(3、100%)、`horizon-cli` の `cli.rs`–`commands.rs`(12 共有、85.7%)、`src/preview/e2e.rs`–`registry.rs`(21 共有、80.8%)。

`clone_change_consistency`(片側だけが変更されたコミット数の多いクローン対):

| 側 A | 側 B | A のみ | B のみ | 両側 |
| --- | --- | --- | --- | --- |
| `TerminalSession.spawn`(method) | `src/workspace/mod.rs` `WorkspaceShell`(type、正規化、6 行) | 28 | 17 | 4 |
| `judge/chat.rs` mod `tests` | `judge/mod.rs` mod `tests`(exact、19 行) | 0 | 25 | 1 |
| `Store.project_event` | `tools/explore.rs` `fold_until_terminal`(正規化、28 行) | 11 | 8 | 9 |
| `Provider for MockProvider.start_session` | `mock.rs` `respond_to_user_message`(正規化、17 行) | 15 | 3 | 3 |
| `frame/fold.rs` `apply_agent_event_to_frame` | `Store.project_event`(exact、12 行) | 4 | 12 | 8 |
| `src/keymap.rs` `command_for` | `src/terminal/input.rs` `term_key_code`(正規化、15 行) | 16 | 0 | 0 |

全体では、クローン側 2,980 件のうち 761 件は囲む unit が無い(断片が複数の unit にまたがる)。両側が unit に解決した対は 437、片側のみの変更が 1 回以上ある対は 498(worker 測定)。

`clone_site_tags_summary`(囲むタグの組み合わせ、containment):

| source | 組み合わせ | 対 | 割合 |
| --- | --- | --- | --- |
| exact | test / test | 228 | 84.4% |
| exact | none / none | 25 | 9.3% |
| exact | trait_impl / trait_impl | 9 | 3.3% |
| normalized | test / test | 911 | 73.9% |
| normalized | none / none | 259 | 21.0% |
| normalized | trait_impl / trait_impl | 17 | 1.4% |

`unit_fan_in_churn`(参照元ファイル数の多い unit):

| unit | fan-in ファイル数 | 参照 occurrence | commits |
| --- | --- | --- | --- |
| `horizon-wire/src/session.rs` `SessionId`(type) | 107 | 979 | 1 |
| `horizon-agent/src/contract.rs` `Event`(type) | 89 | 3,079 | 21 |
| `SessionId`(impl) | 78 | 569 | 1 |
| `contract.rs` `ToolCallId` | 68 | 425 | 1 |
| `contract.rs` `ToolCallRequest` | 64 | 1,103 | 6 |

## 出力から見えること(観察であり推奨ではない)

- impl ブロックとモジュールが churn の上位を占める。内側のどこかが変わると囲む unit も「変更」になる設計のためで、unit の粒度(impl やモジュールを数えるか、関数とメソッドだけにするか)は未決である。
- `tools/catalog.rs::definitions` は現在 3 行だが 42 コミットを持つ。cf9a2daa で分割される前の約 350 行の関数の履歴が、同じ (path, kind, qualified_name) に積まれている。同一性を名前で取ると、現在の形と履歴が一致しない例である。
- fan-in の上位は 1 コミットの id 型である。これらはファイル移動(`horizon-wire` への切り出しなど)で履歴が切れたためで、fan-in × churn を使うには rename をまたぐ同一性が前提になる。先行例はいずれもこれを照合問題として扱っている(拡張側の調査記録)。
- test / test の対が exact で 84%、正規化で 74% を占める。Pizzolotto et al. 2022 の Rust での観察(インラインテストが件数を膨らませる)と同じ傾向だが、本リポジトリ 1 件の数値である。overlap 判定にすると `generated_or_macro` を含む組み合わせが最大になる(`assert!` などのマクロ呼び出しにほぼ全ての断片が重なるため)。
- 共変更の対の数は閾値 10 ポイントごとに約半減する。Code Maat の average 基準と max 基準で 1.3〜1.7 倍の差が出る。
- 正規化クローンの上位には、メソッド本体と struct 定義のように種類の異なる構文の対が並ぶ。識別子とリテラルの正規化が「真陽性だが無用」な報告を生む側である。
- クローン対の片側のみの変更(例: `judge/chat.rs` と `judge/mod.rs` の tests 間で 0 / 25 / 1)は、調査で唯一支持のある有害性の予測子を、このリポジトリで初めて数えた値である。ただし unit 単位(module `tests`)への解決なので、断片そのものの変更かは区別できていない。

## 未決の設計点

- unit の粒度と、囲む unit への変更の伝播。
- コミットをまたぐ unit の同一性(rename・移動・分割)。
- `hunks` は git の 1 始まり、他は 0 始まりで、変換は loader 側で済ませている。SCIP との結合も同じ規約に揃えてある。
- `sitting_duck`(構文木)と `duck_tails`(git、2.0 バイナリ未)への置き換え余地。hunks と SCIP の行単位 reader には既存拡張が無い。
- Go と TypeScript は fixture でのみ検証し、実リポジトリでは未検証。
- jscpd 5.3.1 の `--mode weak` の意味は未確認のまま(拡張側の調査記録と同じ)。
