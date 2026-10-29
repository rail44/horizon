-- Question: what does a unit reference, and what references it, up to a few steps away?
-- The unit is given by its file, kind and qualified name; every unit with that identity
-- is a starting point (two items of one name inside different functions of a file are
-- both). Steps follow
-- unit_edges (views/graph.sql) without self edges: `out` from a unit to the units
-- defining the symbols it references (callees, used types), `in` the other way
-- (callers, users). A neighbor is reported at its least distance in each direction,
-- with the edges that reach it from the units one step nearer: their symbols (display
-- names), unit edges and occurrences, and the neighbor's fan_in_files (unit_fan_in,
-- views/quality.sql). Needs the SCIP indexes.
-- Evidence: none for the walk itself; it is a reading aid. Edges inherit the limits of
-- views/graph.sql (local symbols excluded, calls through a trait reach the trait's
-- method, macro call sites as rust-analyzer resolves them).
-- Parameters (DuckDB variables; default when unset):
--   path, unit_kind, qualified_name (required: the unit, as units lists it),
--   depth 1, direction 'both' ('in', 'out' or 'both').
WITH RECURSIVE params AS (
    SELECT CAST(getvariable('path') AS VARCHAR) AS path,
           CAST(getvariable('unit_kind') AS VARCHAR) AS unit_kind,
           CAST(getvariable('qualified_name') AS VARCHAR) AS qualified_name,
           coalesce(CAST(getvariable('depth') AS INTEGER), 1) AS depth,
           coalesce(CAST(getvariable('direction') AS VARCHAR), 'both') AS direction
),
checked AS (
    SELECT CASE
        WHEN path IS NULL OR unit_kind IS NULL OR qualified_name IS NULL
            THEN error('unit_dependencies: set the variables path, unit_kind and qualified_name')
        WHEN direction NOT IN ('in', 'out', 'both')
            THEN error('unit_dependencies: direction is in, out or both')
        ELSE true END AS ok
    FROM params
),
edges AS MATERIALIZED (
    SELECT src_unit_id, dst_unit_id, symbols, occurrences FROM unit_edges WHERE NOT is_self
),
roots AS MATERIALIZED (
    SELECT u.unit_id
    FROM units u CROSS JOIN params p CROSS JOIN checked
    WHERE u.path = p.path AND u.kind = p.unit_kind AND u.qualified_name = p.qualified_name AND checked.ok
),
-- (direction, unit, distance): set semantics, so each iteration adds only new rows.
walk(direction, unit_id, distance) AS (
    SELECT d.direction, r.unit_id, 0
    FROM roots r CROSS JOIN params p
    CROSS JOIN (VALUES ('out'), ('in')) d(direction)
    WHERE p.direction IN (d.direction, 'both')
    UNION
    SELECT w.direction, CASE w.direction WHEN 'out' THEN e.dst_unit_id ELSE e.src_unit_id END, w.distance + 1
    FROM walk w
    JOIN edges e ON (w.direction = 'out' AND e.src_unit_id = w.unit_id)
                 OR (w.direction = 'in' AND e.dst_unit_id = w.unit_id)
    WHERE w.distance < (SELECT depth FROM params)
),
reached AS MATERIALIZED (
    SELECT direction, unit_id, min(distance) AS distance
    FROM walk
    GROUP BY ALL
),
-- The edges into each neighbor from the units one step nearer in the same direction.
steps AS MATERIALIZED (
    SELECT n.direction, n.unit_id, n.distance,
           CASE n.direction WHEN 'out' THEN e.src_unit_id ELSE e.dst_unit_id END AS from_unit_id,
           e.src_unit_id, e.dst_unit_id, e.occurrences
    FROM reached n
    JOIN reached prev ON prev.direction = n.direction AND prev.distance = n.distance - 1
    JOIN edges e ON (n.direction = 'out' AND e.src_unit_id = prev.unit_id AND e.dst_unit_id = n.unit_id)
                 OR (n.direction = 'in' AND e.dst_unit_id = prev.unit_id AND e.src_unit_id = n.unit_id)
    WHERE n.distance > 0
),
names AS MATERIALIZED (
    SELECT symbol, any_value(display_name) AS display_name FROM scip_symbols GROUP BY symbol
),
step_symbols AS (
    SELECT s.direction, s.unit_id,
           list(DISTINCT coalesce(nullif(nm.display_name, ''), r.symbol) ORDER BY coalesce(nullif(nm.display_name, ''), r.symbol)) AS symbols
    FROM steps s
    JOIN unit_references r ON r.src_unit_id = s.src_unit_id AND r.dst_unit_id = s.dst_unit_id
    LEFT JOIN names nm ON nm.symbol = r.symbol
    GROUP BY ALL
),
step_counts AS (
    SELECT direction, unit_id, count(DISTINCT from_unit_id) AS from_units, count(*) AS unit_edges,
           sum(occurrences) AS occurrences
    FROM steps
    GROUP BY ALL
)
SELECT n.direction, n.distance, u.path, u.kind, u.qualified_name, u.start_line, u.loc,
       c.from_units, c.unit_edges, c.occurrences, s.symbols, coalesce(fi.fan_in_files, 0) AS fan_in_files
FROM reached n
JOIN step_counts c USING (direction, unit_id)
JOIN step_symbols s USING (direction, unit_id)
JOIN units u ON u.unit_id = n.unit_id
LEFT JOIN unit_fan_in fi ON fi.unit_id = n.unit_id
ORDER BY n.direction DESC, n.distance, c.occurrences DESC, u.path, u.start_line, u.unit_id;
