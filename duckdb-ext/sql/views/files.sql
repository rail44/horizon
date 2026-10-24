-- files(file_id, path, language): the current source files of the repository.
--
-- The set is `source_files` from 00_load.sql: files matching `include_globs` under
-- `repo`, minus `exclude_patterns`, with an extension listed in `languages`. It is
-- read from the working tree, so untracked files that match are included; there is
-- no tracked-files table function yet. `path` is repository-relative with `/`.
-- `file_id` is the position in the sorted list (0-based) and is stable only while
-- `source_files` is unchanged.

CREATE OR REPLACE VIEW files AS
SELECT (i - 1)::INTEGER AS file_id,
       path,
       getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] AS language
FROM (
    SELECT unnest(getvariable('source_files')) AS path,
           generate_subscripts(getvariable('source_files'), 1) AS i
);

-- sources(file_id, content): the text of every source file, read live.
CREATE OR REPLACE VIEW sources AS
SELECT f.file_id, t.content
FROM read_text(getvariable('source_paths')) t
JOIN files f ON f.path = substr(t.filename, length(getvariable('repo')) + 2);

-- Bytes [s, e) of a file whose content is given hex-encoded (source_hex). DuckDB has
-- no substring on BLOB and VARCHAR substring counts code points, so slicing goes
-- through hex().
CREATE OR REPLACE MACRO byte_slice(hex_content, s, e) AS
    decode(unhex(substring(hex_content, 2 * s + 1, 2 * (e - s))));

-- Collapse whitespace runs to one space and strip (RE2's \s is ASCII whitespace).
CREATE OR REPLACE MACRO normalize_ws(t) AS trim(regexp_replace(t, '\s+', ' ', 'g'));

CREATE OR REPLACE VIEW source_hex AS
SELECT file_id, hex(content) AS hex FROM sources;

-- source_lines(file_id, line, nonblank_through): for each 1-based line, the number of
-- non-blank lines up to and including it (a line is blank when it holds only ASCII
-- space, tab, CR, VT or FF). The non-blank lines of [s, e] are
-- nonblank_through(e) - nonblank_through(s - 1).
CREATE OR REPLACE VIEW source_lines AS
WITH lines AS (
    SELECT file_id, unnest(string_split(content, chr(10))) AS text,
           generate_subscripts(string_split(content, chr(10)), 1) AS line
    FROM sources
)
SELECT file_id, line,
       sum(CASE WHEN regexp_full_match(text, '[ \t\r\x0b\x0c]*') THEN 0 ELSE 1 END)
           OVER (PARTITION BY file_id ORDER BY line) AS nonblank_through
FROM lines;
