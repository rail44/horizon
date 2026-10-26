-- Question: which units are referenced from many other files and also change often?
-- Fan-in: distinct files other than the unit's own that hold a reference
-- occurrence (SCIP role without Definition) of any symbol whose definition
-- occurrence lies within the unit's lines (a whole-file module definition lies
-- in no unit). Enclosing units (impl, module) include the symbols of
-- everything they contain. Needs views/scip.sql (a SCIP index); references from
-- documents that are not source files are not counted.
-- Churn: the commits of the unit's lineage (unit_history, views/lineage.sql: its own
-- and those of the units it continues across renames, moves and moved bodies).
-- Rows are ordered by fan_in_files x commits (fan_in_x_commits), then fan-in.
-- Evidence: high fan-in combined with frequent change ("Unstable Interface",
-- Mo et al., TSE 2021, 19 projects) is the architecture anti-pattern with the
-- largest reported contribution to bug/change proneness; measured on Java/C/C++
-- files, not on Rust or on units below file level.
-- Parameters (DuckDB variables; default when unset):
--   kind NULL (all kinds; or a unit kind such as 'type'),
--   min_fan_in 1 (the least fan_in_files reported);
--   include_merges (00_load.sql, false) also counts merges against their first parent;
--   lineage_threshold (views/lineage.sql, 0.5).
WITH params AS (
    SELECT CAST(getvariable('kind') AS VARCHAR) AS kind,
           coalesce(CAST(getvariable('min_fan_in') AS BIGINT), 1) AS min_fan_in
),
unit_rows AS MATERIALIZED (
    SELECT unit_id, file_id, kind, qualified_name, start_line, end_line, loc FROM units
),
occurrences AS MATERIALIZED (
    SELECT file_id, symbol, start_line, end_line, is_definition, is_reference
    FROM scip_occurrences WHERE file_id IS NOT NULL
),
defs AS MATERIALIZED (
    SELECT DISTINCT u.unit_id, u.file_id, o.symbol
    FROM unit_rows u
    JOIN occurrences o
      ON o.file_id = u.file_id AND o.is_definition
     AND o.start_line >= u.start_line AND o.end_line <= u.end_line
),
fan_in AS (
    SELECT d.unit_id, count(DISTINCT o.file_id) AS fan_in_files, count(*) AS reference_occurrences
    FROM defs d
    JOIN occurrences o ON o.symbol = d.symbol AND o.is_reference AND o.file_id <> d.file_id
    GROUP BY d.unit_id
),
churn AS (
    SELECT h.unit_id, count(DISTINCT h.sha) AS commits
    FROM unit_history h
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
    GROUP BY h.unit_id
)
SELECT f.path, f.language, u.kind, u.qualified_name, u.start_line, u.loc,
       fi.fan_in_files, fi.reference_occurrences, coalesce(ch.commits, 0) AS commits,
       fi.fan_in_files * coalesce(ch.commits, 0) AS fan_in_x_commits
FROM fan_in fi
JOIN unit_rows u ON u.unit_id = fi.unit_id
JOIN files f ON f.file_id = u.file_id
CROSS JOIN params
LEFT JOIN churn ch ON ch.unit_id = u.unit_id
WHERE fi.fan_in_files >= params.min_fan_in
  AND (params.kind IS NULL OR u.kind = params.kind)
ORDER BY fan_in_x_commits DESC, fi.fan_in_files DESC, commits DESC, f.path, u.start_line, u.unit_id;
