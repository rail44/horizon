-- Question: which pairs of current units change together in the same commits, and do
-- they reference each other?
-- Rows: unit_cochange (views/graph.sql): pairs changed together in at least
-- min_shared commits, after dropping commits that touch more than max_commit_files
-- files; coupling_pct divides the shared commits by the larger of the two units'
-- counts. nested marks a unit and one of its enclosing units (always changed
-- together); same_file pairs share a file; referenced is true when a unit edge
-- connects them in either direction. History under a previous path is not followed.
-- Needs views/graph.sql.
-- Evidence: co-change is among the best-supported clone-harm predictors (Mondal,
-- Juergens), but coupling results depend strongly on the threshold (MDPI Software
-- 2024), and co-change alone does not establish a design flaw; the studies count files,
-- not units. No threshold is validated.
-- Parameters (DuckDB variables; default when unset):
--   min_shared 3, max_commit_files 50;
--   include_merges (00_load.sql, false) also counts merges against their first parent.
WITH pairs AS MATERIALIZED (
    SELECT * FROM unit_cochange
),
linked AS MATERIALIZED (
    SELECT DISTINCT least(src_unit_id, dst_unit_id) AS unit_a, greatest(src_unit_id, dst_unit_id) AS unit_b
    FROM unit_edges WHERE NOT is_self
),
unit_rows AS MATERIALIZED (
    SELECT u.unit_id, f.path, u.kind, u.qualified_name FROM units u JOIN files f USING (file_id)
)
SELECT a.path AS path_a, a.kind AS kind_a, a.qualified_name AS qualified_name_a,
       b.path AS path_b, b.kind AS kind_b, b.qualified_name AS qualified_name_b,
       p.shared_commits, p.commits_a, p.commits_b, p.coupling_pct, p.same_file, p.nested,
       EXISTS (SELECT 1 FROM linked l WHERE l.unit_a = p.unit_a AND l.unit_b = p.unit_b) AS referenced
FROM pairs p
JOIN unit_rows a ON a.unit_id = p.unit_a
JOIN unit_rows b ON b.unit_id = p.unit_b
ORDER BY p.shared_commits DESC, p.coupling_pct DESC, a.path, a.qualified_name, a.kind, b.path, b.qualified_name,
         b.kind, p.unit_a, p.unit_b;
