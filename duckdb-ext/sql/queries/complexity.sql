-- Question: which functions and methods branch the most, and which nest their control
-- flow the deepest?
-- Axis: unit complexity and nesting (docs/research/code-quality-axes-2026-10-29.md §3.3).
-- Rows: every function and method with its own syntax-tree counts (unit_complexity over
-- unit_own_ast: a nested fn's or an inline module's nodes belong to that unit; closure
-- bodies belong to the unit that writes them; macro arguments are tokens and count
-- nothing), most decisions first, then the deepest nesting.
-- Rules (unit_complexity, lang/<language>.sql, <language>_control_nodes): decisions are
-- McCabe's decision points (Rust: one per if, else if, while, for, match arm but one per
-- match, guard, `&&`, `||`, `?` and let-else; Go: one per if, else if, for with a clause
-- or a condition, case but the default, `&&` and `||`); a bare loop is none, and a
-- closure adds no entry of its own. cyclomatic is decisions + 1. max_nesting is the most
-- of if (an else-if chain is one level), match or switch, and the loops open at one node.
-- Columns: path, package, kind, qualified_name, start_line, loc, cyclomatic, decisions,
-- max_nesting, then one <construct>_count column per control construct the units of the
-- result hold, in their language's words and in alphabetical order (unit_constructs:
-- Rust's if_count, else_if_count, match_count, arm_count, guard_count, try_count,
-- let_else_count, closure_count, …; Go's switch_count, case_count, select_count,
-- goto_count, func_literal_count, …; if_count counts no else if), then is_test.
-- Evidence: weak to medium. Cognitive complexity predicts understanding time (r = 0.54)
-- but hardly correctness; SIG's unit complexity correlates 0.50 to 0.54 with defect
-- resolution; no Rust study (survey of 2026-10-24). Over this repository's 3,425 non-test
-- functions and methods (docs/research/code-db-unit-ast-2026-10-29.md), Spearman with
-- loc is 0.69 for decisions and 0.64 for max_nesting (0.87 between the two).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Parameters (DuckDB variables; default when unset): include_tests false.
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) AS include_tests
),
shown AS MATERIALIZED (
    SELECT u.* FROM units u, params p
    WHERE u.kind IN ('function', 'method') AND (p.include_tests OR NOT u.is_test)
),
-- The constructs of the shown units, one column each (PIVOT reads no common table
-- expression of its query, so it repeats shown's condition).
counts AS (
    PIVOT (SELECT c.unit_id, c.construct || '_count' AS construct, c.count
           FROM unit_constructs c JOIN units u USING (unit_id)
           WHERE u.kind IN ('function', 'method')
             AND (coalesce(CAST(getvariable('include_tests') AS BOOLEAN), false) OR NOT u.is_test))
    ON construct USING sum(count) GROUP BY unit_id
)
SELECT u.path, u.package, u.kind, u.qualified_name, u.start_line, u.loc, c.decisions + 1 AS cyclomatic,
       c.decisions, c.max_nesting, coalesce(COLUMNS(k.* EXCLUDE (unit_id)), 0)::BIGINT, u.is_test
FROM shown u
JOIN unit_complexity c USING (unit_id)
LEFT JOIN counts k USING (unit_id)
ORDER BY c.decisions DESC, c.max_nesting DESC, u.loc DESC, u.path, u.start_line;
