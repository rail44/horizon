-- Question: how many units of each language and kind sit inside each tagged region?
-- A unit carries a tag when a tagged node of its file contains the unit's node
-- (including the node itself); 'none' counts units without any tag. A unit
-- with several tags is counted once per tag. For checking adapters only.
-- Evidence: none; this is an adapter sanity check, not a metric.
WITH tagged AS (
    SELECT t.file_id, t.tag, n.start_byte, n.end_byte
    FROM tags t JOIN nodes n USING (file_id, node_id)
),
unit_tags AS (
    SELECT u.unit_id, f.language, u.kind, tg.tag
    FROM units u
    JOIN files f USING (file_id)
    JOIN nodes n ON n.file_id = u.file_id AND n.node_id = u.node_id
    LEFT JOIN tagged tg
      ON tg.file_id = u.file_id AND tg.start_byte <= n.start_byte AND tg.end_byte >= n.end_byte
)
SELECT language, kind, coalesce(tag, 'none') AS tag, count(DISTINCT unit_id) AS units
FROM unit_tags
GROUP BY ALL
ORDER BY language, kind, tag
