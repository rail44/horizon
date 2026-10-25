-- Question: which units are referenced from many other files and also change often?
-- Fan-in: distinct files other than the unit's own that hold a reference
-- occurrence (SCIP role without Definition) of any symbol whose definition
-- occurrence lies within the unit's lines (a whole-file module definition lies
-- in no unit). Enclosing units (impl, module) include the symbols of
-- everything they contain. Needs views/scip.sql (a SCIP index); references from
-- documents that are not source files are not counted.
-- Churn: commits in unit_changes for the unit's (path, kind, qualified_name);
-- history under a previous path is not followed (a rename starts a new history).
-- Evidence: high fan-in combined with frequent change ("Unstable Interface",
-- Mo et al., TSE 2021, 19 projects) is the architecture anti-pattern with the
-- largest reported contribution to bug/change proneness; measured on Java/C/C++
-- files, not on Rust or on units below file level.
-- Parameters (DuckDB variables; default when unset):
--   kind NULL (all kinds; or a unit kind such as 'type'),
--   min_fan_in 1 (the least fan_in_files reported);
--   include_merges (00_load.sql, false) also counts merges against their first parent.
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
    SELECT uc.path, uc.unit_kind, uc.unit_qualified_name, count(DISTINCT uc.sha) AS commits
    FROM unit_changes uc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR getvariable('include_merges')
    GROUP BY ALL
)
SELECT f.path, f.language, u.kind, u.qualified_name, u.start_line, u.loc,
       fi.fan_in_files, fi.reference_occurrences, coalesce(ch.commits, 0) AS commits
FROM fan_in fi
JOIN unit_rows u ON u.unit_id = fi.unit_id
JOIN files f ON f.file_id = u.file_id
CROSS JOIN params
LEFT JOIN churn ch
  ON ch.path = f.path AND ch.unit_kind = u.kind AND ch.unit_qualified_name = u.qualified_name
WHERE fi.fan_in_files >= params.min_fan_in
  AND (params.kind IS NULL OR u.kind = params.kind)
ORDER BY fi.fan_in_files DESC, commits DESC, f.path, u.start_line, u.unit_id;
