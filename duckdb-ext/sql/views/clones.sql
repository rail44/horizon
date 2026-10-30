-- Duplication: jscpd's clone pairs and the units their fragments lie in.
--
-- clone_report_pairs(report_pair_id, source, file_a, start_line_a, end_line_a, file_b,
--             start_line_b, end_line_b, tokens, lines): one row per jscpd duplicate in the
-- reports listed in `clone_reports` (00_load.sql: the reports `code-db index` wrote that
-- exist; duckdb-ext/sql/jscpd.sh produces them over the files of `files`), as reported.
--
-- Read with the core read_json and a fixed schema; only
-- duplicates[].firstFile/secondFile.{name, start, end}, tokens and lines are read.
-- `source` is the name of the directory holding the report (exact, normalized). The list
-- always holds duckdb-ext/sql/empty-jscpd-report.json too (no duplicates), so that the
-- view exists, empty, when no report does; its path is relative to the Horizon checkout,
-- the current directory, as 00_load.sql's LOAD paths are. Lines are jscpd's 1-based
-- inclusive start/end. Names are absolute (jscpd --absolute) and made relative to `repo`;
-- a name outside `repo` stays absolute and matches no file. report_pair_id numbers the
-- duplicates by source and report order, from 0. read_json reads the reports at every
-- scan.
CREATE OR REPLACE VIEW clone_report_pairs AS
WITH reports AS (
    SELECT regexp_extract(filename, '([^/]+)/[^/]+$', 1) AS source,
           unnest(duplicates) AS d,
           generate_subscripts(duplicates, 1) AS ordinal
    FROM read_json(getvariable('clone_reports') || ['duckdb-ext/sql/empty-jscpd-report.json'],
                   columns := {'duplicates': 'STRUCT("firstFile" STRUCT(name VARCHAR, "start" INTEGER, "end" INTEGER),
                                                     "secondFile" STRUCT(name VARCHAR, "start" INTEGER, "end" INTEGER),
                                                     tokens INTEGER, lines INTEGER)[]'},
                   filename := true)
),
relative AS (
    SELECT source, ordinal, d,
           CASE WHEN starts_with(d.firstFile.name, getvariable('repo') || '/')
                THEN substr(d.firstFile.name, length(getvariable('repo')) + 2)
                ELSE d.firstFile.name END AS file_a,
           CASE WHEN starts_with(d.secondFile.name, getvariable('repo') || '/')
                THEN substr(d.secondFile.name, length(getvariable('repo')) + 2)
                ELSE d.secondFile.name END AS file_b
    FROM reports
)
SELECT (row_number() OVER (ORDER BY source, ordinal) - 1)::INTEGER AS report_pair_id,
       source,
       file_a, d.firstFile.start AS start_line_a, d.firstFile."end" AS end_line_a,
       file_b, d.secondFile.start AS start_line_b, d.secondFile."end" AS end_line_b,
       d.tokens AS tokens, d.lines AS lines
FROM relative;

-- clone_pairs(pair_id, sources, report_pairs, file_a, start_line_a, end_line_a, file_b,
--             start_line_b, end_line_b, tokens, lines): the clone pairs the queries read,
-- from clone_report_pairs by two rules:
--   - a duplicate whose two fragments lie in one file with overlapping line ranges (they
--     share a line) is dropped: it is a run of similar lines matching itself shifted by a
--     few lines, not two copies. Two fragments of one file that do not overlap stay (a
--     copy inside one unit);
--   - the same two places found by both reports are one pair: an `exact` duplicate whose
--     fragments overlap those of a `normalized` duplicate on both sides (the same two
--     files, overlapping ranges on each side, in either order) joins the one of them
--     sharing the most lines (the lowest report_pair_id on a tie); a `normalized`
--     duplicate can take several `exact` ones. Duplicates of one report are not merged
--     with each other: overlapping duplicates within a report are different pairs of a
--     family of similar blocks (block 1 with block 3, block 2 with block 4).
-- Each duplicate's sides are first put in order (side a the one with the smaller path,
-- or in one file the earlier start line). A merged pair's ranges are the union of its
-- duplicates' ranges on each side; tokens and lines are the largest of its duplicates'
-- (jscpd's counts, not recounted over the union). sources lists the reports that found
-- it (sorted), report_pairs its duplicates' report_pair_ids. pair_id numbers the pairs
-- by their first report_pair_id, from 0.
CREATE OR REPLACE VIEW clone_pairs AS
WITH swapped AS (
    SELECT *, file_b < file_a OR (file_b = file_a AND start_line_b < start_line_a) AS swap
    FROM clone_report_pairs
),
kept AS MATERIALIZED (
    SELECT * FROM (
        SELECT s.report_pair_id, s.source, s.tokens, s.lines,
               CASE WHEN s.swap THEN s.file_b ELSE s.file_a END AS file_a,
               CASE WHEN s.swap THEN s.start_line_b ELSE s.start_line_a END AS start_line_a,
               CASE WHEN s.swap THEN s.end_line_b ELSE s.end_line_a END AS end_line_a,
               CASE WHEN s.swap THEN s.file_a ELSE s.file_b END AS file_b,
               CASE WHEN s.swap THEN s.start_line_a ELSE s.start_line_b END AS start_line_b,
               CASE WHEN s.swap THEN s.end_line_a ELSE s.end_line_b END AS end_line_b
        FROM swapped s
    )
    WHERE NOT (file_a = file_b AND start_line_a <= end_line_b AND start_line_b <= end_line_a)
),
-- For each exact duplicate, the normalized duplicate it joins.
joins AS (
    SELECT x.report_pair_id, y.report_pair_id AS group_id
    FROM kept x JOIN kept y
      ON x.source = 'exact' AND y.source = 'normalized' AND x.file_a = y.file_a AND x.file_b = y.file_b
     AND x.start_line_a <= y.end_line_a AND y.start_line_a <= x.end_line_a
     AND x.start_line_b <= y.end_line_b AND y.start_line_b <= x.end_line_b
    QUALIFY row_number() OVER (
        PARTITION BY x.report_pair_id
        ORDER BY least(x.end_line_a, y.end_line_a) - greatest(x.start_line_a, y.start_line_a)
                 + least(x.end_line_b, y.end_line_b) - greatest(x.start_line_b, y.start_line_b) DESC,
                 y.report_pair_id) = 1
),
grouped AS (
    SELECT k.*, coalesce(j.group_id, k.report_pair_id) AS group_id
    FROM kept k LEFT JOIN joins j USING (report_pair_id)
)
SELECT (row_number() OVER (ORDER BY min(report_pair_id)) - 1)::INTEGER AS pair_id,
       list(DISTINCT source ORDER BY source) AS sources,
       list(report_pair_id ORDER BY report_pair_id) AS report_pairs,
       any_value(file_a) AS file_a, min(start_line_a) AS start_line_a, max(end_line_a) AS end_line_a,
       any_value(file_b) AS file_b, min(start_line_b) AS start_line_b, max(end_line_b) AS end_line_b,
       max(tokens) AS tokens, max(lines) AS lines
FROM grouped
GROUP BY group_id;

-- clone_sides(pair_id, side, file_id, start_line, end_line, unit_id): each pair's two
-- fragments (side 'a' and 'b') with the innermost unit whose lines contain the whole
-- fragment (a file module at worst; NULL in a file without units or outside `files`).
CREATE OR REPLACE VIEW clone_sides AS
WITH sides AS MATERIALIZED (
    SELECT p.pair_id, 'a' AS side, f.file_id, p.start_line_a AS start_line, p.end_line_a AS end_line
    FROM clone_pairs p LEFT JOIN files f ON f.path = p.file_a
    UNION ALL
    SELECT p.pair_id, 'b', f.file_id, p.start_line_b, p.end_line_b
    FROM clone_pairs p LEFT JOIN files f ON f.path = p.file_b
),
unit_lines AS MATERIALIZED (
    SELECT unit_id, file_id, start_line, end_line FROM units
),
innermost AS (
    SELECT s.pair_id, s.side, max(u.unit_id) AS unit_id
    FROM sides s JOIN unit_lines u ON u.file_id = s.file_id AND u.start_line <= s.start_line AND s.end_line <= u.end_line
    GROUP BY ALL
)
SELECT s.pair_id, s.side, s.file_id, s.start_line, s.end_line, i.unit_id
FROM sides s LEFT JOIN innermost i USING (pair_id, side);
