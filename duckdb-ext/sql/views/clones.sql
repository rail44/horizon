-- clone_pairs(pair_id, source, file_a, start_line_a, end_line_a, file_b, start_line_b,
--             end_line_b, tokens, lines): one row per jscpd duplicate in the reports
-- listed in `clone_reports` (duckdb-ext/sql/jscpd.sh produces them).
--
-- Read with the core read_json, which types each report as one row whose
-- `duplicates` column is a list of structs; the view unnests it. Only
-- duplicates[].firstFile/secondFile.{name, start, end}, tokens and lines are read.
-- `source` is the name of the directory holding the report (exact, normalized).
-- Lines are jscpd's 1-based inclusive start/end. Names are absolute (jscpd
-- --absolute) and made relative to `repo`; a name outside `repo` stays absolute and
-- matches no file. pair_id numbers the pairs by source and report order, from 0.
-- read_json reads the reports when this file is read (to type the view) and again at
-- every scan.

CREATE OR REPLACE VIEW clone_pairs AS
WITH reports AS (
    SELECT regexp_extract(filename, '([^/]+)/[^/]+$', 1) AS source,
           unnest(duplicates) AS d,
           generate_subscripts(duplicates, 1) AS ordinal
    FROM read_json(getvariable('clone_reports'), filename := true)
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
       file_a, d.firstFile.start::INTEGER AS start_line_a, d.firstFile."end"::INTEGER AS end_line_a,
       file_b, d.secondFile.start::INTEGER AS start_line_b, d.secondFile."end"::INTEGER AS end_line_b,
       d.tokens::INTEGER AS tokens, d.lines::INTEGER AS lines
FROM relative;
