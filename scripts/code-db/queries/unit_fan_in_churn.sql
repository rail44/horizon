-- Question: which units are referenced from many other files and also change often?
-- Fan-in: distinct files other than the unit's own that hold a reference
-- occurrence (SCIP role without Definition) of any symbol whose definition
-- occurrence lies within the unit's rows (a whole-file module definition lies
-- in no unit). Enclosing units (impl, module) include the symbols of
-- everything they contain. Requires a SCIP index.
-- Churn: non-merge commits in unit_changes for the unit's (path, kind, qualified_name).
-- Evidence: high fan-in combined with frequent change ("Unstable Interface",
-- Mo et al., TSE 2021, 19 projects) is the architecture anti-pattern with the
-- largest reported contribution to bug/change proneness; measured on Java/C/C++
-- files, not on Rust or on units below file level.
-- @param kind null
-- @param min_fan_in 1
-- @param include_merges 0
WITH defs AS (
    SELECT DISTINCT u.unit_id, o.symbol
    FROM units u
    JOIN scip_occurrences o
      ON o.file_id = u.file_id AND o.is_definition
     AND o.start_row >= u.start_row AND o.end_row <= u.end_row
),
fan_in AS (
    SELECT d.unit_id, count(DISTINCT o.file_id) AS fan_in_files, count(*) AS reference_occurrences
    FROM defs d
    JOIN units u ON u.unit_id = d.unit_id
    JOIN scip_occurrences o ON o.symbol = d.symbol AND o.is_reference AND o.file_id <> u.file_id
    GROUP BY d.unit_id
),
churn AS (
    SELECT uc.path, uc.unit_kind, uc.unit_qualified_name, count(DISTINCT uc.sha) AS commits
    FROM unit_changes uc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR $include_merges = 1
    GROUP BY ALL
)
SELECT f.path, u.kind, u.qualified_name, u.start_row + 1 AS start_line, u.loc,
       fi.fan_in_files, fi.reference_occurrences, coalesce(ch.commits, 0) AS commits
FROM fan_in fi
JOIN units u ON u.unit_id = fi.unit_id
JOIN files f ON f.file_id = u.file_id
LEFT JOIN churn ch
  ON ch.path = f.path AND ch.unit_kind = u.kind AND ch.unit_qualified_name = u.qualified_name
WHERE fi.fan_in_files >= $min_fan_in
  AND (CAST($kind AS VARCHAR) IS NULL OR u.kind = CAST($kind AS VARCHAR))
ORDER BY fi.fan_in_files DESC, commits DESC, f.path, u.start_row, u.unit_id
