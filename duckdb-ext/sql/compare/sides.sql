-- One side of a comparison (../README.md, Comparison): the rows of the current tree's
-- obstacle lists that `code-db compare` keeps of each snapshot. Read after the view files
-- and refresh.sql. `code-db compare` reads it for BEFORE and copies every side_* view into
-- a file with export.sql, then reads it again for AFTER, where compare/views.sql joins the
-- live side_* views to the copies attached as `before`. Both sides are derived by the same
-- SQL files with the same settings in one run.
--
-- side_units: every unit_quality column, and
--   descriptor: the unit's SCIP symbol without its scheme, manager, package and version
--     (`fn run` in `src/workspace/mod.rs` is `workspace/run().`): its place in the crate's
--     module tree, not its file;
--   identity_count: the units of the side with the same crate, kind and descriptor (more
--     than one: the crate roots and the root-level items of a package's several targets,
--     which SCIP all names `crate/`, and items in function bodies, whose descriptors leave
--     the function out);
--   text_hash: md5 of the unit's lines from start_line to end_line (whole lines, each
--     with its trailing whitespace removed, joined by a newline).
CREATE OR REPLACE VIEW side_units AS
WITH named AS MATERIALIZED (
    SELECT unit_id, regexp_extract(symbol, '^[^ ]+ [^ ]+ [^ ]+ [^ ]+ (.*)$', 1) AS descriptor
    FROM units
),
counted AS MATERIALIZED (
    SELECT q.*, n.descriptor, count(*) OVER (PARTITION BY q.crate, q.kind, n.descriptor) AS identity_count
    FROM unit_quality q JOIN named n USING (unit_id)
),
unit_lines AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, end_line FROM units
),
lines AS MATERIALIZED (
    SELECT file_id, line, regexp_replace(text, '\s+$', '') AS text FROM source_lines
),
hashes AS (
    SELECT u.unit_id, md5(string_agg(l.text, chr(10) ORDER BY l.line)) AS text_hash
    FROM unit_lines u JOIN lines l ON l.file_id = u.file_id AND l.line BETWEEN u.start_line AND u.end_line
    GROUP BY u.unit_id
)
SELECT c.*, h.text_hash
FROM counted c LEFT JOIN hashes h USING (unit_id);

-- side_modules(module, crate, is_test): every module (modules.module: a file's path, or
-- `<path>::<names>`) and whether its module unit is a test.
CREATE OR REPLACE VIEW side_modules AS
SELECT m.module, m.crate, coalesce(u.is_test, false) AS is_test
FROM modules m LEFT JOIN units u ON u.unit_id = m.module_unit_id;

-- side_clones(source, unit_1, unit_2, path_1, path_2, is_test, pairs, lines, tokens):
-- jscpd's pairs grouped by their report and their two innermost units (clone_sides), the
-- smaller unit_id first; a side outside every unit has a NULL unit and its file's path.
-- is_test: a side is a test unit (the duplication query leaves such pairs out by
-- default). pairs counts the pairs between the two units, lines and tokens are their sums.
CREATE OR REPLACE VIEW side_clones AS
WITH sides AS (
    SELECT s.pair_id, s.side, s.unit_id,
           coalesce(f.path, CASE s.side WHEN 'a' THEN p.file_a ELSE p.file_b END) AS path,
           coalesce(u.is_test, false) AS is_test
    FROM clone_sides s
    JOIN clone_pairs p USING (pair_id)
    LEFT JOIN files f USING (file_id)
    LEFT JOIN units u USING (unit_id)
),
pairs AS (
    SELECT p.pair_id, p.source, p.lines, p.tokens,
           a.unit_id AS u1, a.path AS p1, b.unit_id AS u2, b.path AS p2, a.is_test OR b.is_test AS is_test,
           coalesce(a.unit_id, -1) < coalesce(b.unit_id, -1)
               OR (coalesce(a.unit_id, -1) = coalesce(b.unit_id, -1) AND a.path <= b.path) AS ordered
    FROM clone_pairs p
    JOIN sides a ON a.pair_id = p.pair_id AND a.side = 'a'
    JOIN sides b ON b.pair_id = p.pair_id AND b.side = 'b'
)
SELECT source,
       CASE WHEN ordered THEN u1 ELSE u2 END AS unit_1, CASE WHEN ordered THEN u2 ELSE u1 END AS unit_2,
       CASE WHEN ordered THEN p1 ELSE p2 END AS path_1, CASE WHEN ordered THEN p2 ELSE p1 END AS path_2,
       bool_or(is_test) AS is_test, count(*) AS pairs, sum(lines) AS lines, sum(tokens) AS tokens
FROM pairs
GROUP BY source, unit_1, unit_2, path_1, path_2;

-- side_module_edges: module_edges with production_unit_edges, its unit edges with no test
-- unit at either end (unit_edges - test_unit_edges).
CREATE OR REPLACE VIEW side_module_edges AS
SELECT *, unit_edges - test_unit_edges AS production_unit_edges FROM module_edges;

-- side_crate_edges: crate_edges with production_unit_edges as above.
CREATE OR REPLACE VIEW side_crate_edges AS
SELECT *, unit_edges - test_unit_edges AS production_unit_edges FROM crate_edges;

-- side_cycles: module_cycles (cycle_edges in force), with test_members, the members whose
-- module unit is a test.
CREATE OR REPLACE VIEW side_cycles AS
WITH cycles AS MATERIALIZED (
    SELECT * FROM module_cycles
),
tests AS (
    SELECT c.cycle, list(m.module ORDER BY m.module) AS test_members
    FROM (SELECT cycle, unnest(members) AS module FROM cycles) c
    JOIN side_modules m ON m.module = c.module AND m.is_test
    GROUP BY c.cycle
)
SELECT c.*, coalesce(t.test_members, []) AS test_members
FROM cycles c LEFT JOIN tests t USING (cycle);

-- side_layer_violations: module_layer_violations with the edge's counts (module_edges).
CREATE OR REPLACE VIEW side_layer_violations AS
SELECT v.src_layer, v.dst_layer, v.src_module, v.dst_module, e.unit_edges, e.test_unit_edges,
       e.unit_edges - e.test_unit_edges AS production_unit_edges, e.occurrences
FROM module_layer_violations v JOIN module_edges e USING (src_module, dst_module);
