# 出現の構文上の位置(occurrence_ast、2026-11-03)

## 問い

ドッグフーディングで「この enum のバリアント列挙はコンパイラに検査されているか」がクエリで答えられなかった。`CommandId` の全バリアントを参照する関数は 4 つあり、`command_enabled`(`crates/horizon-workspace/src/commands.rs`)と `WorkspaceShell::execute`(`src/workspace/commands.rs`)は網羅的な `match` のパターン(コンパイラが検査する)、`core_commands`(同 commands.rs、`vec![CommandSpec { id: CommandId::…, … }, …]`)と `command_for`(`src/keymap.rs`、文字列への `match` の腕の値 `Some(CommandId::…)`)は式(検査されない)。SCIP の出現(`unit_occurrences`)は構文上の文脈を持たないため、ソースを読む以外に区別できなかった。

## 作ったもの

`views/graph.sql` の `occurrence_ast`。`unit_occurrences` の各行(その列がそのままキー。この木では 320,168 行で全列の組が一意)に、

- `node_id`, `type`: 出現の範囲を含む最も内側の `file_ast` ノード。出現の開始位置から始まり終端が出現の終端以降のノードのうち最も深いもの(ほぼ識別子トークン)。そのようなノードがない出現(5,408 件)は出現を含む最も深いノード。rust-analyzer はオーバーロードされた演算子の出現を演算子の横の空白に置く(`64 * 1024` の `*` の両側 1 桁ずつ)ため `binary_expression` 4,894 件・`compound_assignment_expr` 374 件、行をまたぐ `.await` の 139 件など。
- `ancestor_ids`, `ancestor_types`: そのノードの親から外向きの祖先の列(内側が先頭)。出現の単位の範囲に収まる最後の祖先まで(ファイル単位ではファイルの根まで)。平均 7.2 段、最大 57 段、合計 2,298,460 要素。

ノード型は tree-sitter の文字列のまま(共通ビューは言語中立のデータとして持つ)。

検証: 無作為の 3,000 出現について、出現を含む全ノードを総当たりで求めた最深ノードと、単位の範囲内の祖先の列が、ビューと 3,000 件すべて一致。

## マクロ

マクロ引数はトークン木のまま。rust-analyzer は展開から出現をソースに戻すので、`vec![...]` 内のバリアント参照も識別子トークンの上に出現があり位置は特定できる(`core_commands` の 36 件すべて)。ただし祖先は `token_tree` の列から `macro_invocation` までで、間に式やパターンの構造はない(`[token_tree, token_tree, macro_invocation, let_declaration, block, function_item]`)。`matches!(x, A | B)` のパターンもトークンなので「パターン」には数えられない。トークン木内の出現は 53,720 件、うち識別子以外(`[` `]` `!` `self` など)5,251 件。`macro_rules!` 本体の中は 6 件。

## コスト

ライブ(キャッシュに入れない)。保存済みローディング上で `SELECT 1` が 0.7–1.1 s、ビュー全体を集計するコマンドが 4.1–4.2 s なので、ビュー自体は約 3 s(負荷平均 9–20 の時点)。全行を一時表に実体化しても 4.7 s。

最初の版は 11–15 s かかった。プロファイルでは、再帰の起点で `innermost` と `nodes` を `node_id` だけで結合してから `file_id` で絞る計画が選ばれていた(累計 37 s)のと、再帰の各段で出現の表を結合していた(7,800 万行の中間結果)のが原因で、`innermost` に `file_id` を持たせ、単位の範囲を再帰の行に載せて段ごとにはノードとだけキーで結合するようにした。`USING KEY` で再帰中にリストを伸ばす形も試したが 6.7 s で遅く、採らなかった。

キャッシュ層に加えると構築が約 3–4 s 延び、問い合わせは 0.1 s 程度になる見込み(未計測)。どちらにするかは判断事項として残す。

## 問いの SQL と結果

```sql
-- For each unit referencing at least 3 variants of one enum: how many of those references
-- sit in a match arm's pattern (the compiler checks a match's exhaustiveness) and how many
-- elsewhere (expressions, macro token trees).
WITH variants AS (
    SELECT DISTINCT e.symbol AS enum_symbol, e.display_name AS enum_name, v.symbol AS variant_symbol
    FROM scip_symbols e
    JOIN scip_symbols v ON v.kind = 'EnumMember' AND starts_with(v.symbol, e.symbol)
     AND regexp_full_match(substr(v.symbol, length(e.symbol) + 1), '[^#/.]+#')
    WHERE e.kind = 'Enum'
),
enum_sizes AS (
    SELECT enum_symbol, count(*) AS variants FROM variants GROUP BY enum_symbol
),
refs AS (
    SELECT o.unit_id, v.enum_symbol, v.enum_name, o.symbol, o.file_id, o.node_id,
           list_position(o.ancestor_types, 'match_pattern') AS pattern_level, o.ancestor_ids
    FROM occurrence_ast o JOIN variants v ON v.variant_symbol = o.symbol
    WHERE NOT o.is_definition
),
placed AS (
    -- In the pattern: below a match_pattern through its first child (a guard's condition
    -- is a later child of the same match_pattern).
    SELECT r.*, coalesce(r.pattern_level > 0 AND a.sibling_index = 0, false) AS in_pattern
    FROM refs r
    LEFT JOIN file_ast a ON a.file_id = r.file_id
     AND a.node_id = CASE WHEN r.pattern_level = 1 THEN r.node_id ELSE r.ancestor_ids[r.pattern_level - 1] END
)
SELECT f.path, u.qualified_name, p.enum_name, s.variants AS enum_variants,
       count(DISTINCT p.symbol) AS variants_referenced,
       count(*) FILTER (WHERE p.in_pattern) AS in_match_pattern,
       count(*) FILTER (WHERE NOT p.in_pattern) AS elsewhere
FROM placed p
JOIN units u USING (unit_id)
JOIN files f ON f.file_id = u.file_id
JOIN enum_sizes s USING (enum_symbol)
GROUP BY f.path, u.qualified_name, p.enum_name, s.variants
HAVING count(DISTINCT p.symbol) >= 3
ORDER BY p.enum_name = 'CommandId' DESC, variants_referenced DESC, f.path, u.qualified_name
```

ガード: tree-sitter-rust の `match_pattern` はパターンとガードを子に持つ(この木の `match_pattern` の子は sibling_index 0 がパターン、1 が `if` 186 件、2 が条件)。そのためパターンかどうかは「祖先に `match_pattern` があり、その下の経路の子が sibling_index 0」で判定している。

結果(先頭、`CommandId` の 36 バリアント):

| unit | 参照したバリアント | パターン内 | それ以外 |
| --- | ---: | ---: | ---: |
| `command_enabled` | 36 | 36 | 0 |
| `core_commands` | 36 | 0 | 36 |
| `command_for` | 36 | 0 | 36 |
| `WorkspaceShell.execute` | 36 | 36 | 0 |
| `tests.core_commands_have_stable_ids_and_titles` | 17 | 0 | 17 |
| `tests.resolves_known_command_ids` | 10 | 0 | 10 |
| `BoardListView.board_command` | 6 | 6 | 0 |

4 関数がパターン 2 つ・式 2 つに分かれる。リポジトリ全体では 669 行(単位×enum)、パターンだけ 193、それ以外だけ 393、混在 83。他の例: `Event` の 34 バリアントを `event_kind` など 5 関数はパターンで、`fold_until_terminal`(`crates/horizon-agent/src/tools/explore.rs`)はパターン以外で 40 件参照している(多くがマクロのトークン木内)。

## 提案(未実装): 言語層での中立な文脈分類

共通ビューは生のノード型のままとし、意味づけを使うなら `<language>_control_nodes` と同じく言語層に置く案。たとえば `<language>_occurrence_contexts(file_id, node_id, context)` で、出現の祖先のうち最初に該当するものを中立語に分類する: `match_pattern`(Rust: match_pattern の第 1 子の下、Go: `expression_case` の値)、`guard`、`let_pattern`(let / if let / while let / 引数パターン)、`macro_tokens`(Rust: token_tree、Go には無い)、`call_argument`(arguments)、`collection_element`(array_expression、Go の composite_literal)、`field_value`(field_initializer)、`type_position` など。語彙と、Go の switch の網羅性(Go のコンパイラは検査しない)をどう扱うかはオーナーの判断事項。
