-- files(file_id, path, language, mode, blob_id): the current source files of the
-- repository.
--
-- The set is the snapshot `source_files` taken by 00_load.sql (or refresh.sql) from
-- git_tracked_files: tracked regular files, plus untracked ones that are not ignored
-- when `include_untracked` is set, with an extension listed in `languages`, minus
-- `exclude_patterns`. `mode` and `blob_id` are git's mode string and the id of the
-- content as of the snapshot (contents themselves are read live by the views below).
-- `path` is repository-relative with `/`. `file_id` is the position in the sorted list
-- (0-based) and is stable only while `source_files` is unchanged.

CREATE OR REPLACE VIEW files AS
SELECT (i - 1)::INTEGER AS file_id,
       f.path,
       getvariable('languages')[regexp_extract(f.path, '\.([^./]+)$', 1)] AS language,
       f.mode,
       f.blob_id
FROM (
    SELECT unnest(getvariable('source_files')) AS f,
           generate_subscripts(getvariable('source_files'), 1) AS i
);

-- scope_files: the files the derived views (lang/*.sql) compute rows for. All files,
-- unless refresh.sql sets `scope_paths` (repository-relative paths) to recompute
-- units and tags for the files it re-parsed; it resets the variable to NULL after.
CREATE OR REPLACE VIEW scope_files AS
SELECT * FROM files
WHERE getvariable('scope_paths') IS NULL OR list_contains(getvariable('scope_paths'), path);

-- sources(file_id, content): the text of every source file in scope, read live.
CREATE OR REPLACE VIEW sources AS
SELECT f.file_id, t.content
FROM read_text(getvariable('source_paths')) t
JOIN scope_files f ON f.path = substr(t.filename, length(getvariable('repo')) + 2);

-- Bytes [s, e) of a file whose content is given as a BLOB (source_bytes). VARCHAR
-- substring counts code points; BLOB slicing counts bytes (1-based, inclusive).
CREATE OR REPLACE MACRO byte_slice(bytes, s, e) AS decode(bytes[s + 1:e]);

-- Collapse whitespace runs to one space and strip (RE2's \s is ASCII whitespace).
CREATE OR REPLACE MACRO normalize_ws(t) AS trim(regexp_replace(t, '\s+', ' ', 'g'));

-- The two text views below are table macros over a `(file_id, content)` relation named
-- by `sources_t`, so that views/blobs.sql derives the same columns from historical
-- blobs (file_id is then the blob id). query_table resolves the name when a query runs.

-- source_bytes(file_id, bytes): sources as BLOBs, the argument byte_slice takes.
CREATE OR REPLACE MACRO source_bytes_of(sources_t) AS TABLE
SELECT file_id, encode(content) AS bytes FROM query_table(sources_t);

CREATE OR REPLACE VIEW source_bytes AS FROM source_bytes_of('sources');

-- source_lines(file_id, line, nonblank_through): for each 1-based line, the number of
-- non-blank lines up to and including it (a line is blank when it holds only ASCII
-- space, tab, CR, VT or FF). The non-blank lines of [s, e] are
-- nonblank_through(e) - nonblank_through(s - 1).
CREATE OR REPLACE MACRO source_lines_of(sources_t) AS TABLE
WITH lines AS (
    SELECT file_id, unnest(string_split(content, chr(10))) AS text,
           generate_subscripts(string_split(content, chr(10)), 1) AS line
    FROM query_table(sources_t)
)
SELECT file_id, line,
       sum(CASE WHEN regexp_full_match(text, '[ \t\r\x0b\x0c]*') THEN 0 ELSE 1 END)
           OVER (PARTITION BY file_id ORDER BY line) AS nonblank_through
FROM lines;

CREATE OR REPLACE VIEW source_lines AS FROM source_lines_of('sources');
