-- The comparison of two snapshots as differences of their obstacle lists (../README.md,
-- Comparison): the live side_* views (compare/sides.sql) are AFTER, the tables of the
-- database attached as `before` (compare/export.sql) are BEFORE. Rows are identified by
-- what the obstacle lists name them by, never by a correspondence between units: a unit by
-- unit_key (crate, kind, SCIP descriptor), a clone pair by its two units' keys, a module
-- edge by its two modules, a cycle by its members. A rename or a move to another module
-- is a disappeared and an appeared row. Nothing is thresholded: every view gives the
-- values on both sides, the difference and a status, kept, appeared or disappeared; every
-- row says whether it is test code.

-- compare_column_rules(column_name, axis, rule): how a unit_quality column reads as an
-- obstacle, for compare_unit_axes. rule:
--   'lower': a number, an obstacle's size (fewer lines, pairs, files, parameters is better);
--   'null_is_better': a value marks the obstacle (cycle: the module is in a cycle);
--   'true_is_better': false marks the obstacle (has_doc);
--   'unreferenced_is_worse': the value 'unreferenced' marks the obstacle (reference_status);
--   'none': a change, neither better nor worse (layer, visibility, references).
-- A unit_quality column not listed here is read as 'lower' when it is a number and as
-- 'none' otherwise, with its own name as its axis.
CREATE OR REPLACE VIEW compare_column_rules AS
SELECT * FROM (VALUES
    ('loc', 'size', 'lower'),
    ('clone_pairs', 'duplication', 'lower'),
    ('exact_pairs', 'duplication', 'lower'),
    ('normalized_pairs', 'duplication', 'lower'),
    ('fan_in_files', 'coupling', 'lower'),
    ('fan_in_crates', 'coupling', 'lower'),
    ('fan_out_files', 'coupling', 'lower'),
    ('fan_out_crates', 'coupling', 'lower'),
    ('cycle', 'independence', 'null_is_better'),
    ('layer', 'independence', 'none'),
    ('visibility', 'interface', 'none'),
    ('parameters', 'interface', 'lower'),
    ('has_doc', 'intent', 'true_is_better'),
    ('debt_lines', 'intent', 'lower'),
    ('references', 'unreferenced', 'none'),
    ('reference_status', 'unreferenced', 'unreferenced_is_worse')
) AS t(column_name, axis, rule);

-- compare_obstacle(rule, value): whether a column's value (as text) counts as an
-- obstacle under its rule (compare_column_rules); NULL for 'none' and for a NULL number.
CREATE OR REPLACE MACRO compare_obstacle(rule, value) AS
    CASE rule WHEN 'lower' THEN TRY_CAST(value AS DOUBLE) > 0
              WHEN 'null_is_better' THEN value IS NOT NULL
              WHEN 'true_is_better' THEN value = 'false'
              WHEN 'unreferenced_is_worse' THEN value = 'unreferenced' END;

-- compare_units(unit_key, crate, kind, qualified_name, status, is_test, text_changed,
-- ambiguous, paired_by, before_unit_id, after_unit_id, path_before, path_after,
-- module_before, module_after, start_line_before, start_line_after, loc_before,
-- loc_after, before, after): one row per unit on either side, a BEFORE and an AFTER unit
-- paired when they have the same crate, kind and descriptor (side_units). A descriptor
-- several units share on a side (ambiguous) pairs its units by path first, in line order
-- within a file (paired_by 'path'), then the rest in path and line order ('order'); a
-- descriptor one unit has on each side pairs them wherever they are (paired_by NULL).
-- status: kept (paired), appeared (AFTER only), disappeared (BEFORE only). unit_key:
-- `crate kind descriptor`, with ` #n` after the n-th of an ambiguous descriptor (n in path
-- and line order). is_test: AFTER's flag, BEFORE's for a disappeared unit; text_changed:
-- whether the text hashes differ (kept units only). before_unit_id, after_unit_id: the
-- unit_id on each side (each side numbers its own units). before and after are the whole
-- side_units rows as structs (NULL on the missing side).
CREATE OR REPLACE VIEW compare_units AS
WITH b AS MATERIALIZED (
    SELECT * FROM before.side_units
),
a AS MATERIALIZED (
    SELECT * FROM side_units
),
b_in_path AS MATERIALIZED (
    SELECT unit_id, crate, kind, descriptor, path,
           row_number() OVER (PARTITION BY crate, kind, descriptor, path ORDER BY start_line, unit_id) AS n
    FROM b
),
a_in_path AS MATERIALIZED (
    SELECT unit_id, crate, kind, descriptor, path,
           row_number() OVER (PARTITION BY crate, kind, descriptor, path ORDER BY start_line, unit_id) AS n
    FROM a
),
by_path AS MATERIALIZED (
    SELECT bp.unit_id AS before_unit_id, ap.unit_id AS after_unit_id
    FROM b_in_path bp JOIN a_in_path ap USING (crate, kind, descriptor, path, n)
),
b_rest AS MATERIALIZED (
    SELECT unit_id, crate, kind, descriptor,
           row_number() OVER (PARTITION BY crate, kind, descriptor ORDER BY path, start_line, unit_id) AS n
    FROM b WHERE unit_id NOT IN (SELECT before_unit_id FROM by_path)
),
a_rest AS MATERIALIZED (
    SELECT unit_id, crate, kind, descriptor,
           row_number() OVER (PARTITION BY crate, kind, descriptor ORDER BY path, start_line, unit_id) AS n
    FROM a WHERE unit_id NOT IN (SELECT after_unit_id FROM by_path)
),
pairs AS MATERIALIZED (
    SELECT before_unit_id, after_unit_id, 'path' AS paired_by FROM by_path
    UNION ALL
    SELECT br.unit_id, ar.unit_id, 'order' FROM b_rest br JOIN a_rest ar USING (crate, kind, descriptor, n)
),
rows AS (
    SELECT p.before_unit_id, p.after_unit_id, p.paired_by FROM pairs p
    UNION ALL
    SELECT unit_id, NULL, NULL FROM b WHERE unit_id NOT IN (SELECT before_unit_id FROM pairs)
    UNION ALL
    SELECT NULL, unit_id, NULL FROM a WHERE unit_id NOT IN (SELECT after_unit_id FROM pairs)
),
joined AS (
    SELECT r.*, b, a,
           coalesce(a.crate, b.crate) AS crate, coalesce(a.kind, b.kind) AS kind,
           coalesce(a.descriptor, b.descriptor) AS descriptor,
           coalesce(a.identity_count, 1) > 1 OR coalesce(b.identity_count, 1) > 1 AS ambiguous
    FROM rows r
    LEFT JOIN b ON b.unit_id = r.before_unit_id
    LEFT JOIN a ON a.unit_id = r.after_unit_id
)
SELECT concat_ws(' ', j.crate, j.kind, j.descriptor)
           || CASE WHEN j.ambiguous
                   THEN ' #' || row_number() OVER (PARTITION BY j.crate, j.kind, j.descriptor
                                                   ORDER BY coalesce(j.a.path, j.b.path),
                                                            coalesce(j.a.start_line, j.b.start_line),
                                                            j.after_unit_id, j.before_unit_id)
                   ELSE '' END AS unit_key,
       j.crate, j.kind, coalesce(j.a.qualified_name, j.b.qualified_name) AS qualified_name,
       CASE WHEN j.before_unit_id IS NULL THEN 'appeared' WHEN j.after_unit_id IS NULL THEN 'disappeared'
            ELSE 'kept' END AS status,
       coalesce(j.a.is_test, j.b.is_test) AS is_test,
       CASE WHEN j.before_unit_id IS NOT NULL AND j.after_unit_id IS NOT NULL
            THEN j.a.text_hash IS DISTINCT FROM j.b.text_hash END AS text_changed,
       j.ambiguous, CASE WHEN j.ambiguous THEN j.paired_by END AS paired_by,
       j.before_unit_id, j.after_unit_id,
       j.b.path AS path_before, j.a.path AS path_after, j.b.module AS module_before, j.a.module AS module_after,
       j.b.start_line AS start_line_before, j.a.start_line AS start_line_after,
       j.b.loc AS loc_before, j.a.loc AS loc_after,
       j.b AS before, j.a AS after
FROM joined j;

-- compare_unit_axes(unit_key, crate, kind, qualified_name, status, is_test, text_changed,
-- column_name, axis, rule, before_value, after_value, delta, change, effect, obstacle_before,
-- obstacle_after): one row per unit of compare_units and axis column of unit_quality
-- (every column but the identity: unit_id, path, crate, module, kind, qualified_name,
-- start_line, end_line, is_test; read from the view, so columns added to unit_quality
-- appear here). Values are text; delta is after - before for a number present on both
-- sides. change: unchanged, increased, decreased (numbers), changed (anything else), NULL
-- for a unit on one side. effect (kept units): improved or regressed by the column's rule
-- (compare_column_rules), NULL when the rule is 'none' or the change is between two values
-- the rule does not order. obstacle_before/after: the value counts as an obstacle under the
-- rule (a number above 0; a cycle; has_doc false; 'unreferenced'); NULL for 'none'.
CREATE OR REPLACE VIEW compare_unit_axes AS
WITH types AS MATERIALIZED (
    SELECT column_name, regexp_matches(data_type, '^(TINYINT|SMALLINT|INTEGER|BIGINT|HUGEINT|UTINYINT|USMALLINT|UINTEGER|UBIGINT|UHUGEINT|FLOAT|DOUBLE|DECIMAL)')
               AS numeric
    FROM duckdb_columns()
    WHERE database_name = current_database() AND schema_name = 'main' AND table_name = 'unit_quality'
),
rules AS MATERIALIZED (
    SELECT t.column_name, coalesce(r.axis, t.column_name) AS axis,
           coalesce(r.rule, CASE WHEN t.numeric THEN 'lower' ELSE 'none' END) AS rule, t.numeric
    FROM types t LEFT JOIN compare_column_rules r USING (column_name)
    WHERE t.column_name NOT IN ('unit_id', 'path', 'crate', 'module', 'kind', 'qualified_name', 'start_line',
                                'end_line', 'is_test')
),
b_long AS MATERIALIZED (
    SELECT * FROM (
        SELECT unit_id, COLUMNS(* EXCLUDE (unit_id, path, crate, module, kind, qualified_name, start_line, end_line,
                                           is_test, descriptor, identity_count, text_hash))::VARCHAR
        FROM before.side_units)
    UNPIVOT INCLUDE NULLS (value FOR column_name IN (COLUMNS(* EXCLUDE (unit_id))))
),
a_long AS MATERIALIZED (
    SELECT * FROM (
        SELECT unit_id, COLUMNS(* EXCLUDE (unit_id, path, crate, module, kind, qualified_name, start_line, end_line,
                                           is_test, descriptor, identity_count, text_hash))::VARCHAR
        FROM side_units)
    UNPIVOT INCLUDE NULLS (value FOR column_name IN (COLUMNS(* EXCLUDE (unit_id))))
),
units AS MATERIALIZED (
    SELECT unit_key, crate, kind, qualified_name, status, is_test, text_changed, before_unit_id, after_unit_id
    FROM compare_units
),
joined AS (
    SELECT u.*, r.column_name, r.axis, r.rule, r.numeric, b.value AS before_value, a.value AS after_value
    FROM units u
    CROSS JOIN rules r
    LEFT JOIN b_long b ON b.unit_id = u.before_unit_id AND b.column_name = r.column_name
    LEFT JOIN a_long a ON a.unit_id = u.after_unit_id AND a.column_name = r.column_name
),
valued AS (
    SELECT *,
           CASE WHEN numeric AND status = 'kept' THEN TRY_CAST(after_value AS DOUBLE) - TRY_CAST(before_value AS DOUBLE) END AS delta
    FROM joined
)
SELECT unit_key, crate, kind, qualified_name, status, is_test, text_changed, column_name, axis, rule,
       before_value, after_value, delta,
       CASE WHEN status <> 'kept' THEN NULL
            WHEN before_value IS NOT DISTINCT FROM after_value THEN 'unchanged'
            WHEN delta > 0 THEN 'increased' WHEN delta < 0 THEN 'decreased'
            ELSE 'changed' END AS change,
       CASE WHEN status <> 'kept' OR before_value IS NOT DISTINCT FROM after_value THEN NULL
            WHEN rule = 'lower' THEN CASE WHEN delta < 0 THEN 'improved' WHEN delta > 0 THEN 'regressed' END
            WHEN rule = 'null_is_better' THEN CASE WHEN before_value IS NULL THEN 'regressed'
                                                   WHEN after_value IS NULL THEN 'improved' END
            WHEN rule = 'true_is_better' THEN CASE WHEN before_value = 'false' AND after_value = 'true' THEN 'improved'
                                                   WHEN before_value = 'true' AND after_value = 'false' THEN 'regressed' END
            WHEN rule = 'unreferenced_is_worse' THEN CASE WHEN after_value = 'unreferenced' THEN 'regressed'
                                                          WHEN before_value = 'unreferenced' THEN 'improved' END
       END AS effect,
       CASE WHEN status = 'appeared' THEN NULL ELSE compare_obstacle(rule, before_value) END AS obstacle_before,
       CASE WHEN status = 'disappeared' THEN NULL ELSE compare_obstacle(rule, after_value) END AS obstacle_after
FROM valued;

-- compare_clones(source, key_a, key_b, unit_a, unit_b, path_a, path_b, status, is_test,
-- pairs_before, pairs_after, pairs_delta, lines_before, lines_after): jscpd's pairs per
-- report and pair of units (side_clones) on either side, the units identified by their
-- unit_key (compare_units; a side outside every unit by `(no unit) <path>`) in sorted
-- order. A pair between the same two units on both sides is kept, whatever its fragments;
-- pairs_delta is the change in the number of pairs between them. is_test: a side is a
-- test unit on either snapshot.
CREATE OR REPLACE VIEW compare_clones AS
WITH keys AS MATERIALIZED (
    SELECT unit_key, qualified_name, before_unit_id, after_unit_id FROM compare_units
),
keyed AS (
    SELECT 'before' AS side, c.source, c.is_test, c.pairs, c.lines,
           coalesce(k1.unit_key, '(no unit) ' || c.path_1) AS k1, k1.qualified_name AS q1, c.path_1 AS p1,
           coalesce(k2.unit_key, '(no unit) ' || c.path_2) AS k2, k2.qualified_name AS q2, c.path_2 AS p2
    FROM before.side_clones c
    LEFT JOIN keys k1 ON k1.before_unit_id = c.unit_1
    LEFT JOIN keys k2 ON k2.before_unit_id = c.unit_2
    UNION ALL
    SELECT 'after', c.source, c.is_test, c.pairs, c.lines,
           coalesce(k1.unit_key, '(no unit) ' || c.path_1), k1.qualified_name, c.path_1,
           coalesce(k2.unit_key, '(no unit) ' || c.path_2), k2.qualified_name, c.path_2
    FROM side_clones c
    LEFT JOIN keys k1 ON k1.after_unit_id = c.unit_1
    LEFT JOIN keys k2 ON k2.after_unit_id = c.unit_2
),
sorted AS (
    SELECT side, source, is_test, pairs, lines,
           CASE WHEN k1 <= k2 THEN k1 ELSE k2 END AS key_a, CASE WHEN k1 <= k2 THEN k2 ELSE k1 END AS key_b,
           CASE WHEN k1 <= k2 THEN q1 ELSE q2 END AS unit_a, CASE WHEN k1 <= k2 THEN q2 ELSE q1 END AS unit_b,
           CASE WHEN k1 <= k2 THEN p1 ELSE p2 END AS path_a, CASE WHEN k1 <= k2 THEN p2 ELSE p1 END AS path_b
    FROM keyed
)
SELECT source, key_a, key_b,
       coalesce(any_value(unit_a) FILTER (WHERE side = 'after'), any_value(unit_a)) AS unit_a,
       coalesce(any_value(unit_b) FILTER (WHERE side = 'after'), any_value(unit_b)) AS unit_b,
       coalesce(any_value(path_a) FILTER (WHERE side = 'after'), any_value(path_a)) AS path_a,
       coalesce(any_value(path_b) FILTER (WHERE side = 'after'), any_value(path_b)) AS path_b,
       CASE WHEN NOT bool_or(side = 'before') THEN 'appeared' WHEN NOT bool_or(side = 'after') THEN 'disappeared'
            ELSE 'kept' END AS status,
       bool_or(is_test) AS is_test,
       coalesce(sum(pairs) FILTER (WHERE side = 'before'), 0) AS pairs_before,
       coalesce(sum(pairs) FILTER (WHERE side = 'after'), 0) AS pairs_after,
       coalesce(sum(pairs) FILTER (WHERE side = 'after'), 0) - coalesce(sum(pairs) FILTER (WHERE side = 'before'), 0)
           AS pairs_delta,
       coalesce(sum(lines) FILTER (WHERE side = 'before'), 0) AS lines_before,
       coalesce(sum(lines) FILTER (WHERE side = 'after'), 0) AS lines_after
FROM sorted
GROUP BY source, key_a, key_b;

-- compare_edges(level, src, dst, status, is_test, nested_modules, unit_edges_before,
-- unit_edges_after, unit_edges_delta, production_unit_edges_before,
-- production_unit_edges_after, production_unit_edges_delta, test_unit_edges_before,
-- test_unit_edges_after, occurrences_before, occurrences_after, occurrences_delta): module
-- edges (level 'module', side_module_edges; src and dst are modules.module) and crate
-- edges (level 'crate') on either side, identified by their two ends. Counts are 0 on the
-- side without the edge. is_test: no production unit edge on either side (every unit
-- edge has a test unit at one end). nested_modules: module_edges' flag (NULL for crates).
CREATE OR REPLACE VIEW compare_edges AS
WITH sides AS (
    SELECT 'module' AS level, coalesce(a.src_module, b.src_module) AS src, coalesce(a.dst_module, b.dst_module) AS dst,
           b.src_module IS NOT NULL AS in_before, a.src_module IS NOT NULL AS in_after,
           coalesce(a.nested_modules, b.nested_modules) AS nested_modules,
           b.unit_edges AS ub, a.unit_edges AS ua, b.production_unit_edges AS pb, a.production_unit_edges AS pa,
           b.test_unit_edges AS tb, a.test_unit_edges AS ta, b.occurrences AS ob, a.occurrences AS oa
    FROM side_module_edges a
    FULL JOIN before.side_module_edges b ON a.src_module = b.src_module AND a.dst_module = b.dst_module
    UNION ALL
    SELECT 'crate', coalesce(a.src_crate, b.src_crate), coalesce(a.dst_crate, b.dst_crate),
           b.src_crate IS NOT NULL, a.src_crate IS NOT NULL, NULL,
           b.unit_edges, a.unit_edges, b.production_unit_edges, a.production_unit_edges,
           b.test_unit_edges, a.test_unit_edges, b.occurrences, a.occurrences
    FROM side_crate_edges a
    FULL JOIN before.side_crate_edges b ON a.src_crate = b.src_crate AND a.dst_crate = b.dst_crate
)
SELECT level, src, dst,
       CASE WHEN NOT in_before THEN 'appeared' WHEN NOT in_after THEN 'disappeared' ELSE 'kept' END AS status,
       coalesce(pb, 0) = 0 AND coalesce(pa, 0) = 0 AS is_test, nested_modules,
       coalesce(ub, 0) AS unit_edges_before, coalesce(ua, 0) AS unit_edges_after,
       coalesce(ua, 0) - coalesce(ub, 0) AS unit_edges_delta,
       coalesce(pb, 0) AS production_unit_edges_before, coalesce(pa, 0) AS production_unit_edges_after,
       coalesce(pa, 0) - coalesce(pb, 0) AS production_unit_edges_delta,
       coalesce(tb, 0) AS test_unit_edges_before, coalesce(ta, 0) AS test_unit_edges_after,
       coalesce(ob, 0) AS occurrences_before, coalesce(oa, 0) AS occurrences_after,
       coalesce(oa, 0) - coalesce(ob, 0) AS occurrences_delta
FROM sides;

-- compare_cycles(status, cycle_before, cycle_after, crate, modules_before, modules_after,
-- modules_delta, members_added, members_removed, unit_edges_before, unit_edges_after,
-- is_test, split, merged, members_before, members_after): module cycles (side_cycles) on
-- either side, matched by their members: a BEFORE cycle and an AFTER cycle that share a
-- module form one kept row; a cycle sharing no module with the other side is appeared or
-- disappeared. Cycles are named by their least member on each side (a name changes when
-- that member leaves). split: the BEFORE cycle shares members with several AFTER cycles;
-- merged: the AFTER cycle shares members with several BEFORE cycles. is_test: every
-- member on both sides is a test module.
CREATE OR REPLACE VIEW compare_cycles AS
WITH b AS MATERIALIZED (
    SELECT * FROM before.side_cycles
),
a AS MATERIALIZED (
    SELECT * FROM side_cycles
),
links AS MATERIALIZED (
    SELECT DISTINCT bm.cycle AS cycle_before, am.cycle AS cycle_after
    FROM (SELECT cycle, unnest(members) AS module FROM b) bm
    JOIN (SELECT cycle, unnest(members) AS module FROM a) am USING (module)
),
link_counts AS MATERIALIZED (
    SELECT cycle_before, cycle_after,
           count(*) OVER (PARTITION BY cycle_before) AS afters, count(*) OVER (PARTITION BY cycle_after) AS befores
    FROM links
),
matched AS (
    SELECT cycle_before, cycle_after, afters > 1 AS split, befores > 1 AS merged FROM link_counts
    UNION ALL
    SELECT b.cycle, NULL, false, false FROM b WHERE b.cycle NOT IN (SELECT cycle_before FROM links)
    UNION ALL
    SELECT NULL, a.cycle, false, false FROM a WHERE a.cycle NOT IN (SELECT cycle_after FROM links)
)
SELECT CASE WHEN r.cycle_before IS NULL THEN 'appeared' WHEN r.cycle_after IS NULL THEN 'disappeared' ELSE 'kept' END
           AS status,
       r.cycle_before, r.cycle_after, coalesce(a.crate, b.crate) AS crate,
       coalesce(b.modules, 0) AS modules_before, coalesce(a.modules, 0) AS modules_after,
       coalesce(a.modules, 0) - coalesce(b.modules, 0) AS modules_delta,
       list_filter(coalesce(a.members, []), lambda m: NOT list_contains(coalesce(b.members, []), m)) AS members_added,
       list_filter(coalesce(b.members, []), lambda m: NOT list_contains(coalesce(a.members, []), m)) AS members_removed,
       coalesce(b.unit_edges, 0) AS unit_edges_before, coalesce(a.unit_edges, 0) AS unit_edges_after,
       len(coalesce(b.test_members, [])) = len(coalesce(b.members, []))
           AND len(coalesce(a.test_members, [])) = len(coalesce(a.members, [])) AS is_test,
       r.split, r.merged, b.members AS members_before, a.members AS members_after
FROM matched r
LEFT JOIN b ON b.cycle = r.cycle_before
LEFT JOIN a ON a.cycle = r.cycle_after;

-- compare_layer_violations(src_layer, dst_layer, src_module, dst_module, status, is_test,
-- unit_edges_before, unit_edges_after, unit_edges_delta): module edges against the
-- declared layers (side_layer_violations; both sides read AFTER's settings) on either
-- side. is_test: no production unit edge on either side.
CREATE OR REPLACE VIEW compare_layer_violations AS
SELECT coalesce(a.src_layer, b.src_layer) AS src_layer, coalesce(a.dst_layer, b.dst_layer) AS dst_layer,
       coalesce(a.src_module, b.src_module) AS src_module, coalesce(a.dst_module, b.dst_module) AS dst_module,
       CASE WHEN b.src_module IS NULL THEN 'appeared' WHEN a.src_module IS NULL THEN 'disappeared' ELSE 'kept' END
           AS status,
       coalesce(b.production_unit_edges, 0) = 0 AND coalesce(a.production_unit_edges, 0) = 0 AS is_test,
       coalesce(b.unit_edges, 0) AS unit_edges_before, coalesce(a.unit_edges, 0) AS unit_edges_after,
       coalesce(a.unit_edges, 0) - coalesce(b.unit_edges, 0) AS unit_edges_delta
FROM side_layer_violations a
FULL JOIN before.side_layer_violations b ON a.src_module = b.src_module AND a.dst_module = b.dst_module;
