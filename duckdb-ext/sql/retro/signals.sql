-- Retrospective, step 2. Run on a checkout of the baseline commit, with its SCIP index:
--   duckdb-ext/sql/run.sh sql BASE_CLONE duckdb-ext/sql/retro/signals.sql \
--       retro_changes=/abs/changes.parquet retro_reviews=/abs/reviews.json \
--       retro_record_preserves=/abs/duckdb-ext/sql/retro/september-2026-record-preserves.csv \
--       retro_record_targets=/abs/duckdb-ext/sql/retro/september-2026-record-targets.csv \
--       retro_ranks_out=/abs/ranks.csv [retro_scope=production] [retro_min_commits=N] \
--       [retro_lineage_changes=/abs/lineage-changes.parquet] \
--       [retro_lineage_preserves=/abs/lineage-preserves.parquet] [retro_all_ranks_out=/abs/all.csv]
-- (signals_by_name.sql runs this file with the history signals counted by identity.)
-- Needs views/graph.sql (the SCIP index of the baseline).
--
-- Identity. A unit is (path, kind, qualified_name), the identity unit_changes uses; the
-- unit rows sharing it (two `impl Foo` blocks) are one identity, with loc summed.
-- Scope: retro_scope 'all' (default) keeps every identity; 'production' keeps function
-- and method identities that are not tests (no `test` tag contains any of their rows,
-- and the path is not a tests.rs, *_tests.rs or tests/ file).
--
-- Ground truth (retro_truth): `refactored` is every baseline identity that the range's
-- unit changes (retro_changes, written by changes.sql) record as modified or removed in
-- at least retro_min_commits (default 1) commits;
-- `preserve` is every baseline identity that a review of retro_reviews (the old audit's
-- ledger, target file/owner/name) or a row of retro_record_preserves (path, name) names.
-- With retro_lineage_changes and retro_lineage_preserves (written by lineage.sql on the
-- end checkout) the truth is by name or through lineage: a change also counts for every
-- baseline identity its lineage walk reaches at the range's start, and a preserve source
-- also names every baseline identity reached from the units it names; commits are
-- counted once per identity either way. A refactored source then reads "... in N commits
-- (M by name)" when lineage added commits, and a preserve source " via lineage" when
-- only lineage maps it.
-- A review with owner '<free>' names a unit whose qualified name is the name, or a
-- function whose qualified name ends in '.name' (inside an inline module); a review with
-- an owner names the method `owner.name` (possibly inside an inline module). A record
-- row names a unit by its last name segment (owner '<any>': the records write
-- `hub::ingest`, not the impl), any function or method ending in '.name' included.
--
-- Signals: the shipped query files, read as text and run through query(), ranked by
-- their own order (ties broken as the query breaks them), each identity at the rank
-- of its first row (fan_in by fan_in_files then commits, since unit_fan_in_churn orders
-- by their product). Pair signals rank an identity at the first pair it is a side of (for
-- clone_change_consistency, any unit of a side's set, side_units_a and side_units_b,
-- which follow clone_sides).
-- History signals count the commits of each unit's lineage (views/lineage.sql); the
-- ground truth counts unit_changes by identity, or through lineage (above). module_cycles is a set (the units of every module in a cycle), with no order.
-- retro_all_ranks_out (default /dev/null) receives every ranked identity of the scope.

SET VARIABLE retro_scope = coalesce(getvariable('retro_scope'), 'all');
SET VARIABLE retro_min_commits = coalesce(getvariable('retro_min_commits'), 1);
SET VARIABLE retro_all_ranks_out = coalesce(getvariable('retro_all_ranks_out'), '/dev/null');
-- The lineage files, or empty relations of their shape when unset.
SET VARIABLE retro_lineage_changes_sql = CASE WHEN getvariable('retro_lineage_changes') IS NULL
    THEN 'SELECT NULL::VARCHAR AS sha, NULL::VARCHAR AS change_kind, NULL::VARCHAR AS path, NULL::VARCHAR AS kind,
                 NULL::VARCHAR AS qualified_name WHERE false'
    ELSE 'SELECT sha, change_kind, path, kind, qualified_name FROM read_parquet('''
         || replace(getvariable('retro_lineage_changes'), '''', '''''') || ''')' END;
SET VARIABLE retro_lineage_preserves_sql = CASE WHEN getvariable('retro_lineage_preserves') IS NULL
    THEN 'SELECT NULL::VARCHAR AS source, NULL::VARCHAR AS source_path, NULL::VARCHAR AS owner, NULL::VARCHAR AS name,
                 NULL::VARCHAR AS variant, NULL::VARCHAR AS path, NULL::VARCHAR AS kind, NULL::VARCHAR AS qualified_name
          WHERE false'
    ELSE 'SELECT source, source_path, owner, name, variant, path, kind, qualified_name FROM read_parquet('''
         || replace(getvariable('retro_lineage_preserves'), '''', '''''') || ''')' END;

-- The query files' text (without the final semicolon).
SET VARIABLE retro_q_churn = (SELECT rtrim(trim(content), ';') FROM read_text('duckdb-ext/sql/queries/unit_churn.sql'));
SET VARIABLE retro_q_fan_in = (SELECT rtrim(trim(content), ';') FROM read_text('duckdb-ext/sql/queries/unit_fan_in_churn.sql'));
SET VARIABLE retro_q_clones = (SELECT rtrim(trim(content), ';') FROM read_text('duckdb-ext/sql/queries/clone_change_consistency.sql'));
SET VARIABLE retro_q_cochange = (SELECT rtrim(trim(content), ';') FROM read_text('duckdb-ext/sql/queries/cochange_without_reference.sql'));

CREATE OR REPLACE TEMP TABLE retro_units AS
WITH rows_ AS (
    SELECT f.path, u.kind, u.qualified_name, u.loc, u.start_line, t.is_test
    FROM units u JOIN files f USING (file_id) JOIN unit_tests t USING (unit_id)
)
SELECT path, kind, qualified_name, sum(loc) AS loc, min(start_line) AS start_line,
       bool_or(is_test OR regexp_matches(path, '(^|/)tests(/|\.rs$)|_tests\.rs$')) AS is_test
FROM rows_ GROUP BY path, kind, qualified_name;

CREATE OR REPLACE TEMP TABLE retro_scope_units AS
SELECT * FROM retro_units
WHERE getvariable('retro_scope') = 'all'
   OR (getvariable('retro_scope') = 'production' AND kind IN ('function', 'method') AND NOT is_test);

-- Ground truth.
CREATE OR REPLACE TEMP TABLE retro_reviews AS
SELECT r.target.file AS path, r.target.owner AS owner, r.target.name AS name,
       r.target.variant AS variant, r.commit AS review_commit
FROM (SELECT unnest(reviews) AS r FROM read_json(getvariable('retro_reviews'), maximum_object_size := 100000000))
WHERE r.decision = 'preserve';

CREATE OR REPLACE TEMP TABLE retro_preserve_sources AS
SELECT 'reviews.json ' || substr(review_commit, 1, 8) AS source, path, owner, name, variant FROM retro_reviews
UNION ALL
SELECT record, path, '<any>', name, NULL FROM read_csv(getvariable('retro_record_preserves'), header := true);

CREATE OR REPLACE TEMP TABLE retro_preserve_map_by_name AS
SELECT s.source, s.path AS source_path, s.owner, s.name, s.variant, u.kind, u.qualified_name, u.path AS unit_path
FROM retro_preserve_sources s
LEFT JOIN retro_units u
  ON u.path = s.path
 AND CASE WHEN s.owner = '<free>'
          THEN (u.qualified_name = s.name AND u.kind IN ('function', 'type', 'module', 'macro'))
               OR (u.kind = 'function' AND u.qualified_name LIKE '%.' || s.name)
          WHEN s.owner = '<any>'
          THEN u.qualified_name = s.name OR (u.kind IN ('function', 'method') AND u.qualified_name LIKE '%.' || s.name)
          ELSE u.kind = 'method' AND (u.qualified_name = s.owner || '.' || s.name
                                      OR u.qualified_name LIKE '%.' || s.owner || '.' || s.name)
     END;

-- Baseline identities a preserve source reaches through lineage (none without the file),
-- added to the by-name map; a source's unmapped row is dropped when lineage maps it.
CREATE OR REPLACE TEMP TABLE retro_preserve_map AS
WITH lineage AS (
    SELECT DISTINCT l.source, l.source_path, l.owner, l.name, l.variant, u.kind, u.qualified_name, u.path AS unit_path
    FROM query(getvariable('retro_lineage_preserves_sql')) l
    JOIN retro_units u ON u.path = l.path AND u.kind = l.kind AND u.qualified_name = l.qualified_name
),
both_ AS (
    SELECT *, false AS via_lineage FROM retro_preserve_map_by_name WHERE qualified_name IS NOT NULL
    UNION ALL
    SELECT l.*, true FROM lineage l
    WHERE NOT EXISTS (SELECT 1 FROM retro_preserve_map_by_name m
                      WHERE m.source = l.source AND m.source_path = l.source_path AND m.owner = l.owner
                        AND m.name = l.name AND m.variant IS NOT DISTINCT FROM l.variant
                        AND m.unit_path = l.unit_path AND m.kind = l.kind AND m.qualified_name = l.qualified_name)
)
SELECT * FROM both_
UNION ALL
SELECT m.*, false FROM retro_preserve_map_by_name m
WHERE m.qualified_name IS NULL
  AND NOT EXISTS (SELECT 1 FROM both_ b
                  WHERE b.source = m.source AND b.source_path = m.source_path AND b.owner = m.owner
                    AND b.name = m.name AND b.variant IS NOT DISTINCT FROM m.variant);

-- Each change counts for its own identity and, through lineage, for the baseline
-- identities its walk reaches.
CREATE OR REPLACE TEMP TABLE retro_truth_changes AS
SELECT sha, change_kind, path, unit_kind AS kind, unit_qualified_name AS qualified_name, true AS by_name
FROM read_parquet(getvariable('retro_changes'))
WHERE change_kind IN ('modified', 'removed')
UNION ALL
SELECT sha, change_kind, path, kind, qualified_name, false
FROM query(getvariable('retro_lineage_changes_sql'));

CREATE OR REPLACE TEMP TABLE retro_truth AS
SELECT u.path, u.kind AS unit_kind, u.qualified_name, 'refactored' AS decision,
       'unit_changes ' || string_agg(DISTINCT c.change_kind, '+' ORDER BY c.change_kind)
       || ' in ' || count(DISTINCT c.sha) || ' commits'
       || CASE WHEN count(DISTINCT c.sha) FILTER (WHERE c.by_name) < count(DISTINCT c.sha)
               THEN ' (' || count(DISTINCT c.sha) FILTER (WHERE c.by_name) || ' by name)' ELSE '' END AS source
FROM retro_units u
JOIN retro_truth_changes c
  ON c.path = u.path AND c.kind = u.kind AND c.qualified_name = u.qualified_name
GROUP BY ALL
HAVING count(DISTINCT c.sha) >= getvariable('retro_min_commits')
UNION ALL
SELECT unit_path, kind, qualified_name, 'preserve',
       string_agg(DISTINCT source || CASE WHEN via_lineage THEN ' via lineage' ELSE '' END, ', '
                  ORDER BY source || CASE WHEN via_lineage THEN ' via lineage' ELSE '' END)
FROM retro_preserve_map
WHERE qualified_name IS NOT NULL
GROUP BY unit_path, kind, qualified_name;

-- Signal rankings over every identity (scope applied in the evaluation).
CREATE OR REPLACE TEMP TABLE retro_q_churn AS
SELECT row_number() OVER () AS ord, * FROM query(getvariable('retro_q_churn'));
CREATE OR REPLACE TEMP TABLE retro_q_fan_in AS
SELECT row_number() OVER () AS ord, * FROM query(getvariable('retro_q_fan_in'));
CREATE OR REPLACE TEMP TABLE retro_q_clones AS
SELECT row_number() OVER () AS ord, * FROM query(getvariable('retro_q_clones'));
CREATE OR REPLACE TEMP TABLE retro_q_cochange AS
SELECT row_number() OVER () AS ord, * FROM query(getvariable('retro_q_cochange'));

CREATE OR REPLACE TEMP TABLE retro_signal_rows AS
SELECT 'unit_churn' AS signal, path, kind, qualified_name, ord AS score_ord, commits AS value FROM retro_q_churn
UNION ALL
SELECT 'fan_in_x_commits', path, kind, qualified_name,
       row_number() OVER (ORDER BY fan_in_files * commits DESC, ord), fan_in_files * commits FROM retro_q_fan_in
UNION ALL
SELECT 'fan_in', path, kind, qualified_name,
       row_number() OVER (ORDER BY fan_in_files DESC, commits DESC, ord), fan_in_files FROM retro_q_fan_in
UNION ALL
SELECT 'clone_one_sided', file_a, u.kind, u.qualified_name, ord, one_sided
FROM (SELECT file_a, ord, one_sided, unnest(side_units_a) AS u FROM retro_q_clones)
UNION ALL
SELECT 'clone_one_sided', file_b, u.kind, u.qualified_name, ord, one_sided
FROM (SELECT file_b, ord, one_sided, unnest(side_units_b) AS u FROM retro_q_clones)
UNION ALL
SELECT 'cochange_without_reference', path_a, kind_a, qualified_name_a, ord, coupling_pct FROM retro_q_cochange
UNION ALL
SELECT 'cochange_without_reference', path_b, kind_b, qualified_name_b, ord, coupling_pct FROM retro_q_cochange
UNION ALL
SELECT 'loc', path, kind, qualified_name, row_number() OVER (ORDER BY loc DESC, path, start_line, kind, qualified_name), loc
FROM retro_units;

-- Rank within the scope: the identity's first row, renumbered over the scope's identities.
CREATE OR REPLACE TEMP TABLE retro_ranks AS
WITH firsts AS (
    SELECT r.signal, r.path, r.kind, r.qualified_name, min(r.score_ord) AS score_ord,
           arg_min(r.value, r.score_ord) AS value
    FROM retro_signal_rows r
    JOIN retro_scope_units s USING (path, kind, qualified_name)
    GROUP BY ALL
)
SELECT signal, path, kind, qualified_name, value,
       row_number() OVER (PARTITION BY signal ORDER BY score_ord, path, kind, qualified_name) AS rank
FROM firsts;

-- module_cycles as a set: every identity whose unit belongs to a module of a cycle.
CREATE OR REPLACE TEMP TABLE retro_cycle_members AS
SELECT DISTINCT f.path, u.kind, u.qualified_name, any_value(mc.modules) OVER (PARTITION BY f.path, u.kind, u.qualified_name) AS cycle_size
FROM module_cycle_edges e
JOIN module_cycles mc USING (cycle)
JOIN unit_module um ON um.module IN (e.src_module, e.dst_module)
JOIN units u ON u.unit_id = um.unit_id
JOIN files f ON f.file_id = u.file_id
JOIN retro_scope_units s ON s.path = f.path AND s.kind = u.kind AND s.qualified_name = u.qualified_name;

CREATE OR REPLACE TEMP TABLE retro_scope_truth AS
SELECT t.* FROM retro_truth t
JOIN retro_scope_units s ON s.path = t.path AND s.kind = t.unit_kind AND s.qualified_name = t.qualified_name;

.print
.print ## Scope and ground truth
SELECT getvariable('retro_scope') AS scope,
       (SELECT count(*) FROM retro_scope_units) AS identities,
       (SELECT count(*) FROM retro_scope_truth WHERE decision = 'refactored') AS refactored,
       (SELECT count(*) FROM retro_scope_truth WHERE decision = 'preserve') AS preserved,
       (SELECT count(*) FROM retro_scope_truth a JOIN retro_scope_truth b USING (path, unit_kind, qualified_name)
        WHERE a.decision = 'refactored' AND b.decision = 'preserve') AS preserved_and_refactored,
       round(100.0 * (SELECT count(*) FROM retro_scope_truth WHERE decision = 'refactored')
             / (SELECT count(*) FROM retro_scope_units), 1) AS base_rate_pct;

.print
.print ## Cross-check: refactoring targets the records name, in the refactored set (all kinds, before scope)
SELECT r.record, r.path, r.name, u.kind, u.qualified_name, t.source AS refactored
FROM read_csv(getvariable('retro_record_targets'), header := true) r
LEFT JOIN retro_units u
  ON u.path = r.path AND (u.qualified_name = r.name OR u.qualified_name LIKE '%.' || r.name)
 AND u.kind IN ('function', 'method')
LEFT JOIN retro_truth t
  ON t.decision = 'refactored' AND t.path = u.path AND t.unit_kind = u.kind AND t.qualified_name = u.qualified_name
ORDER BY r.record, r.path, r.name;

.print
.print ## Preserve sources mapped to baseline identities (all kinds)
SELECT source, source_path, owner, name, variant, unit_path, kind, qualified_name, via_lineage
FROM retro_preserve_map ORDER BY source, source_path, name, variant, unit_path;

.print
.print ## Precision and recall against the refactored set
WITH refactored AS (SELECT path, unit_kind AS kind, qualified_name FROM retro_scope_truth WHERE decision = 'refactored'),
n AS (SELECT count(*) AS n FROM refactored),
scored AS (
    SELECT r.signal, r.rank, f.path IS NOT NULL AS hit
    FROM retro_ranks r LEFT JOIN refactored f USING (path, kind, qualified_name)
)
SELECT signal, count(*) AS ranked,
       round(avg(hit::INT) FILTER (WHERE rank <= 10), 2) AS p_at_10,
       round(avg(hit::INT) FILTER (WHERE rank <= 50), 2) AS p_at_50,
       round(avg(hit::INT) FILTER (WHERE rank <= 100), 2) AS p_at_100,
       count(*) FILTER (WHERE rank <= 100 AND hit) AS hits_in_100,
       round(count(*) FILTER (WHERE rank <= 100 AND hit) / any_value(n.n), 3) AS recall_at_100,
       count(*) FILTER (WHERE hit) AS hits_ranked
FROM scored CROSS JOIN n
GROUP BY signal
UNION ALL
SELECT 'module_cycles (set)', count(*), NULL, NULL, NULL, NULL, NULL, count(f.path)
FROM retro_cycle_members m LEFT JOIN refactored f USING (path, kind, qualified_name)
ORDER BY signal;

.print
.print ## Preserved identities: rank per signal (NULL: not ranked)
SELECT t.path, t.unit_kind, t.qualified_name,
       EXISTS (SELECT 1 FROM retro_scope_truth x WHERE x.decision = 'refactored' AND x.path = t.path
               AND x.unit_kind = t.unit_kind AND x.qualified_name = t.qualified_name) AS also_refactored,
       max(r.rank) FILTER (WHERE r.signal = 'unit_churn') AS churn,
       max(r.rank) FILTER (WHERE r.signal = 'fan_in_x_commits') AS fan_in_x_commits,
       max(r.rank) FILTER (WHERE r.signal = 'fan_in') AS fan_in,
       max(r.rank) FILTER (WHERE r.signal = 'clone_one_sided') AS clone_one_sided,
       max(r.rank) FILTER (WHERE r.signal = 'cochange_without_reference') AS cochange_wo_ref,
       max(r.rank) FILTER (WHERE r.signal = 'loc') AS loc,
       bool_or(m.path IS NOT NULL) AS in_cycle
FROM retro_scope_truth t
LEFT JOIN retro_ranks r ON r.path = t.path AND r.kind = t.unit_kind AND r.qualified_name = t.qualified_name
LEFT JOIN retro_cycle_members m ON m.path = t.path AND m.kind = t.unit_kind AND m.qualified_name = t.qualified_name
WHERE t.decision = 'preserve'
GROUP BY ALL
ORDER BY t.path, t.qualified_name;

.print
.print ## Preserved identities in each signal's top 100 (also refactored, preserved only)
WITH preserved AS (
    SELECT t.path, t.unit_kind AS kind, t.qualified_name,
           EXISTS (SELECT 1 FROM retro_scope_truth x WHERE x.decision = 'refactored' AND x.path = t.path
                   AND x.unit_kind = t.unit_kind AND x.qualified_name = t.qualified_name) AS also_refactored
    FROM retro_scope_truth t WHERE t.decision = 'preserve'
)
SELECT r.signal,
       count(*) FILTER (WHERE r.rank <= 100) AS preserved_in_100,
       count(*) FILTER (WHERE r.rank <= 100 AND p.also_refactored) AS also_refactored,
       count(*) FILTER (WHERE r.rank <= 100 AND NOT p.also_refactored) AS preserved_only,
       list(p.qualified_name ORDER BY r.rank) FILTER (WHERE r.rank <= 100 AND NOT p.also_refactored) AS preserved_only_units
FROM retro_ranks r JOIN preserved p USING (path, kind, qualified_name)
GROUP BY r.signal
UNION ALL
SELECT 'module_cycles (set)', count(*), count(*) FILTER (WHERE p.also_refactored),
       count(*) FILTER (WHERE NOT p.also_refactored),
       list(p.qualified_name ORDER BY p.path) FILTER (WHERE NOT p.also_refactored)
FROM retro_cycle_members m JOIN preserved p USING (path, kind, qualified_name)
ORDER BY 1;

.print
.print ## Top 20 per signal that were neither refactored nor preserved
SELECT r.signal, r.rank, r.path, r.kind, r.qualified_name, r.value
FROM retro_ranks r
LEFT JOIN retro_scope_truth t ON t.path = r.path AND t.unit_kind = r.kind AND t.qualified_name = r.qualified_name
WHERE t.path IS NULL
QUALIFY row_number() OVER (PARTITION BY r.signal ORDER BY r.rank) <= 20
ORDER BY r.signal, r.rank;

.print
.print ## Every ground-truth identity with its rank per signal -> retro_ranks_out (CSV)
COPY (
    SELECT t.decision, t.path, t.unit_kind, t.qualified_name, t.source,
           max(r.rank) FILTER (WHERE r.signal = 'unit_churn') AS churn,
           max(r.rank) FILTER (WHERE r.signal = 'fan_in_x_commits') AS fan_in_x_commits,
           max(r.rank) FILTER (WHERE r.signal = 'fan_in') AS fan_in,
           max(r.rank) FILTER (WHERE r.signal = 'clone_one_sided') AS clone_one_sided,
           max(r.rank) FILTER (WHERE r.signal = 'cochange_without_reference') AS cochange_wo_ref,
           max(r.rank) FILTER (WHERE r.signal = 'loc') AS loc,
           bool_or(m.path IS NOT NULL) AS in_cycle
    FROM retro_scope_truth t
    LEFT JOIN retro_ranks r ON r.path = t.path AND r.kind = t.unit_kind AND r.qualified_name = t.qualified_name
    LEFT JOIN retro_cycle_members m ON m.path = t.path AND m.kind = t.unit_kind AND m.qualified_name = t.qualified_name
    GROUP BY ALL
    ORDER BY t.decision, churn NULLS LAST, t.path, t.qualified_name
) TO (getvariable('retro_ranks_out')) (FORMAT csv, HEADER);
SELECT count(*) AS rows_written FROM read_csv(getvariable('retro_ranks_out'));

COPY (
    SELECT r.signal, r.rank, r.path, r.kind, r.qualified_name, r.value FROM retro_ranks r ORDER BY r.signal, r.rank
) TO (getvariable('retro_all_ranks_out')) (FORMAT csv, HEADER);
