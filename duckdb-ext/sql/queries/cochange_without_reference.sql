-- Question: which units in different files change together without referencing each
-- other?
-- Rows: cochange_without_reference (views/graph.sql): pairs of unit_cochange in
-- different files, with no unit edge between the two units in either direction and a
-- coupling (shared commits over the larger count) of at least min_coupling_pct. Only
-- the two units themselves are compared: an impl and a function whose methods
-- reference each other count as unreferenced. Needs views/graph.sql.
-- Evidence: change coupling without a structural dependency is the "Modularity
-- Violation" anti-pattern (Mo et al., TSE 2021, 19 projects, Java/C/C++ files), whose
-- reported contribution to bug and change proneness is small next to Unstable
-- Interface and Crossing; co-change thresholds change the result strongly (MDPI
-- Software 2024). Measured between files, not units. No threshold is validated.
-- Parameters (DuckDB variables; default when unset):
--   min_shared 3, min_coupling_pct 30, max_commit_files 50;
--   include_merges (00_load.sql, false) also counts merges against their first parent.
WITH unit_rows AS MATERIALIZED (
    SELECT u.unit_id, f.path, u.kind, u.qualified_name FROM units u JOIN files f USING (file_id)
)
SELECT a.path AS path_a, a.kind AS kind_a, a.qualified_name AS qualified_name_a,
       b.path AS path_b, b.kind AS kind_b, b.qualified_name AS qualified_name_b,
       p.shared_commits, p.commits_a, p.commits_b, p.coupling_pct
FROM cochange_without_reference p
JOIN unit_rows a ON a.unit_id = p.unit_a
JOIN unit_rows b ON b.unit_id = p.unit_b
ORDER BY p.coupling_pct DESC, p.shared_commits DESC, a.path, a.qualified_name, a.kind, b.path, b.qualified_name,
         b.kind, p.unit_a, p.unit_b;
