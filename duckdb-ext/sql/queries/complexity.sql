-- Question: which functions and methods branch the most, and which nest their control
-- flow the deepest?
-- Axis: unit complexity and nesting (docs/research/code-quality-axes-2026-10-29.md §3.3).
-- Rows: every function and method with its own syntax-tree counts (unit_complexity over
-- unit_own_ast: a nested fn's or an inline module's nodes belong to that unit; closure
-- bodies belong to the unit that writes them; macro arguments are tokens and count
-- nothing), most decisions first, then the deepest nesting.
-- Rules (unit_complexity): decisions are McCabe's decision points, one per if (else if
-- and if let included), while (while let included), for, match arm but one per match,
-- guard, `&&`, `||`, `?` and let-else; a bare `loop` is none (as rust-code-analysis
-- issue #1249 expects from McCabe), and a closure adds no entry of its own. cyclomatic
-- is decisions + 1. max_nesting is the most of if (an else-if chain is one level), match,
-- for, while and loop open at one node.
-- Columns: path, crate, kind, qualified_name, start_line, loc, cyclomatic, decisions,
-- max_nesting, if_count, else_if_count, match_count, arm_count, guard_count, for_count,
-- while_count, loop_count, try_count, and_count, or_count, let_else_count, return_count,
-- break_count, continue_count, closure_count, is_test.
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
)
SELECT u.path, u.crate, u.kind, u.qualified_name, u.start_line, u.loc, c.decisions + 1 AS cyclomatic,
       c.decisions, c.max_nesting, c.if_count, c.else_if_count, c.match_count, c.arm_count, c.guard_count,
       c.for_count, c.while_count, c.loop_count, c.try_count, c.and_count, c.or_count, c.let_else_count,
       c.return_count, c.break_count, c.continue_count, c.closure_count, u.is_test
FROM units u
CROSS JOIN params p
JOIN unit_complexity c USING (unit_id)
WHERE u.kind IN ('function', 'method') AND (p.include_tests OR NOT u.is_test)
ORDER BY c.decisions DESC, c.max_nesting DESC, u.loc DESC, u.path, u.start_line;
