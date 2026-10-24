-- Question: in which kind of construct does each side of each clone pair sit?
-- A side carries every tag whose tagged node (in the same file) contains its
-- whole line range (match_mode = contain) or overlaps it (match_mode = overlap);
-- 'none' when no tag applies. Labels join sorted tags with '+'.
-- Evidence: in Rust, per-type copies of trait implementations are the most
-- common Type-1/2 clone kind, and inline tests contribute many more
-- (Pizzolotto et al. 2022, 15 crates, not peer reviewed). Whether separating
-- them improves review precision was not measured.
-- Parameters (DuckDB variables; default when unset):
--   match_mode 'contain' (or 'overlap'), clone_source NULL (all reports; or a
--   source label such as 'exact').
WITH params AS (
    SELECT coalesce(getvariable('match_mode'), 'contain') AS match_mode,
           CAST(getvariable('clone_source') AS VARCHAR) AS clone_source
),
tagged AS (
    SELECT t.file_id, t.tag, n.start_line, n.end_line
    FROM tags t JOIN nodes n USING (file_id, node_id)
),
sides AS (
    SELECT pair_id, 'a' AS side, file_a AS path, start_line_a AS s, end_line_a AS e FROM clone_pairs
    UNION ALL
    SELECT pair_id, 'b', file_b, start_line_b, end_line_b FROM clone_pairs
),
side_tags AS (
    SELECT s.pair_id, s.side,
           coalesce(nullif(array_to_string(list_sort(list_distinct(list(tg.tag))), '+'), ''), 'none') AS label
    FROM sides s
    JOIN files f ON f.path = s.path
    CROSS JOIN params
    LEFT JOIN tagged tg
      ON tg.file_id = f.file_id
     AND (CASE WHEN params.match_mode = 'overlap' THEN tg.start_line <= s.e AND tg.end_line >= s.s
               ELSE tg.start_line <= s.s AND tg.end_line >= s.e END)
    GROUP BY s.pair_id, s.side
)
SELECT cp.pair_id, cp.source,
       cp.file_a, cp.start_line_a, a.label AS tags_a,
       cp.file_b, cp.start_line_b, b.label AS tags_b,
       cp.tokens, cp.lines
FROM clone_pairs cp
JOIN side_tags a ON a.pair_id = cp.pair_id AND a.side = 'a'
JOIN side_tags b ON b.pair_id = cp.pair_id AND b.side = 'b'
CROSS JOIN params
WHERE params.clone_source IS NULL OR cp.source = params.clone_source
ORDER BY cp.source, cp.pair_id;
