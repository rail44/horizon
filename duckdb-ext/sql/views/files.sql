-- files(file_id, path, language, mode, blob_id): the current source files of the
-- repository.
--
-- The set is the snapshot `source_files` taken by 00_load.sql (or refresh.sql) from
-- git_tracked_files: tracked regular files, plus untracked ones that are not ignored
-- when `include_untracked` is set, with an extension listed in `languages`, minus
-- `exclude_patterns`. `mode` and `blob_id` are git's mode string and the id of the
-- content as of the snapshot (contents themselves are read live by the views below).
-- `path` is repository-relative with `/`. `file_id` is the position in the sorted list
-- (0-based) and is stable only while `source_files` is unchanged. A file that no SCIP
-- index covers (outside every Cargo workspace, or code rust-analyzer leaves out) is listed
-- here and has no units.

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

-- sources(file_id, content): the text of every source file, read live.
CREATE OR REPLACE VIEW sources AS
SELECT f.file_id, t.content
FROM read_text(getvariable('source_paths')) t
JOIN files f ON f.path = substr(t.filename, length(getvariable('repo')) + 2);

-- source_lines(file_id, line, text, is_blank, is_comment, nonblank_through): every
-- 1-based line of every source file with its text (without the newline). is_blank: the
-- line holds only ASCII space, tab, CR, VT or FF. is_comment: its first non-blank
-- characters are `//` (line and doc comments; a block comment's inner lines are not
-- recognised). nonblank_through counts the non-blank lines up to and including it, so
-- the non-blank lines of [s, e] are nonblank_through(e) - nonblank_through(s - 1).
CREATE OR REPLACE VIEW source_lines AS
WITH lines AS (
    SELECT file_id, unnest(string_split(content, chr(10))) AS text,
           generate_subscripts(string_split(content, chr(10)), 1) AS line
    FROM sources
)
SELECT file_id, line::INTEGER AS line, text,
       regexp_full_match(text, '[ \t\r\x0b\x0c]*') AS is_blank,
       regexp_matches(text, '^\s*//') AS is_comment,
       sum(CASE WHEN regexp_full_match(text, '[ \t\r\x0b\x0c]*') THEN 0 ELSE 1 END)
           OVER (PARTITION BY file_id ORDER BY line) AS nonblank_through
FROM lines;
