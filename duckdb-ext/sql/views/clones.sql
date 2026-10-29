-- Duplication: jscpd's clone pairs and the units their fragments lie in.
--
-- clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b,
--             end_line_b, tokens, lines): one row per jscpd duplicate in the reports
-- listed in `clone_reports` (00_load.sql: the reports `code-db index` wrote that exist;
-- duckdb-ext/sql/jscpd.sh produces them over the files of `files`).
--
-- Read with the core read_json and a fixed schema; only
-- duplicates[].firstFile/secondFile.{name, start, end}, tokens and lines are read.
-- `source` is the name of the directory holding the report (exact, normalized). The list
-- always holds duckdb-ext/sql/empty-jscpd-report.json too (no duplicates), so that the
-- view exists, empty, when no report does; its path is relative to the Horizon checkout,
-- the current directory, as 00_load.sql's LOAD paths are. Lines are jscpd's 1-based
-- inclusive start/end. Names are absolute (jscpd --absolute) and made relative to `repo`;
-- a name outside `repo` stays absolute and matches no file. pair_id numbers the pairs by
-- source and report order, from 0. read_json reads the reports at every scan.
CREATE OR REPLACE VIEW clone_pairs AS
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
SELECT (row_number() OVER (ORDER BY source, ordinal) - 1)::INTEGER AS pair_id,
       source,
       file_a, d.firstFile.start AS start_line_a, d.firstFile."end" AS end_line_a,
       file_b, d.secondFile.start AS start_line_b, d.secondFile."end" AS end_line_b,
       d.tokens AS tokens, d.lines AS lines
FROM relative;

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
