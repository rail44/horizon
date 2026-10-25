-- The syntax of historical file contents: the blobs on either side of every change of a
-- source file in since..HEAD, parsed with the same read_ast call as the current files.
--
-- blob_files(sha, path, old_path, side, blob_id, mode, file): one row per side of a
--   change that has a regular-file blob, from git_changed_blobs (the git_history
--   extension): the (commit, file change) pairs of git_file_changes, `side` 'old'
--   (absent for an addition) or 'new' (absent for a deletion), `path`/`old_path` as in
--   file_changes, `mode` the side's git mode, and `file` = <blob_dir>/<blob_id>.<ext>,
--   which the function writes when it is missing. Only extensions in `languages`; no
--   rows for a change with the same content on both sides (a pure rename), nor for
--   binary, symlink or submodule sides. `blob_dir` must be an absolute path.
--   `since` and `include_merges` are read at each scan, like the history views.
-- blob_nodes(blob_id, <read_ast's columns>): one parse per distinct blob, keyed by
--   blob_id; the language is the one read_ast detects from the file's extension, which
--   is the extension of the path the blob was recorded under.
--
-- read_ast and read_text take a constant list of files, and the files must exist when
-- the list is bound, so this file takes a snapshot, as 00_load.sql does for the current
-- files: `blob_snapshot` (a list of {blob_id, file} sorted by blob id) and `blob_paths`
-- (their files) are computed once here by scanning blob_files, which exports the blobs.
-- Re-read this file (or refresh.sql) to see commits made after it. With no blobs in the
-- range, the list is one file that does not exist: read_ast refuses an empty list,
-- `ignore_errors` lets it return no rows, and read_text returns none for a missing file.
--
-- Units of blobs (rust_blob_units, blob_units) are derived from the private inputs
-- below, which mirror scope_nodes, sources, source_bytes and source_lines with
-- file_id holding the blob's position in the snapshot. A blob's rows depend on that blob alone; refresh.sql
-- narrows the snapshot to the blobs it parses.

CREATE OR REPLACE VIEW blob_files AS
SELECT * FROM git_changed_blobs(getvariable('repo'), getvariable('blob_dir'), since := getvariable('since'),
                                include_merges := getvariable('include_merges'),
                                extensions := map_keys(getvariable('languages')));

-- The snapshot; refresh.sql repeats these statements over its cached blob_files.
SET VARIABLE blob_snapshot = (
    SELECT coalesce(list({'blob_id': blob_id, 'file': file} ORDER BY blob_id), [])
    FROM (SELECT blob_id, min(file) AS file FROM blob_files GROUP BY blob_id)
);
SET VARIABLE blob_paths_empty = len(getvariable('blob_snapshot')) = 0;
SET VARIABLE blob_paths = CASE WHEN getvariable('blob_paths_empty')
    THEN [getvariable('blob_dir') || '/no-such-blob.rs']
    ELSE list_transform(getvariable('blob_snapshot'), lambda b: b.file) END;

-- blob_list(blob_id, file, blob_ord): the snapshot as rows; blob_ord is the position in
-- the snapshot (1-based), the integer key the derivation runs on.
CREATE OR REPLACE VIEW blob_list AS
SELECT b.blob_id, b.file, i AS blob_ord
FROM (SELECT unnest(getvariable('blob_snapshot')) AS b,
             generate_subscripts(getvariable('blob_snapshot'), 1) AS i);

-- The same read_ast parameters as views/nodes.sql.
CREATE OR REPLACE VIEW blob_nodes AS
SELECT l.blob_id, a.*
FROM read_ast(getvariable('blob_paths'), source := 'full', peek := 'none', context := 'none',
              ignore_errors := getvariable('blob_paths_empty')) a
JOIN blob_list l ON l.file = a.file_path;

-- Inputs of rust_units_over and source_*_of for blobs, keyed by file_id = blob_ord.
-- An integer key, not the 40-character blob id: with the blob id as the key, a live
-- blob_units peaked at about 13 GB on this repository (31 million nodes).
CREATE OR REPLACE VIEW blob_scope_nodes AS
SELECT l.blob_ord AS file_id, n.* EXCLUDE (blob_id)
FROM blob_nodes n JOIN blob_list l USING (blob_id);

CREATE OR REPLACE VIEW blob_sources AS
SELECT l.blob_ord AS file_id, t.content
FROM read_text(getvariable('blob_paths')) t
JOIN blob_list l ON l.file = t.filename;

CREATE OR REPLACE VIEW blob_source_bytes AS FROM source_bytes_of('blob_sources');

CREATE OR REPLACE VIEW blob_source_lines AS FROM source_lines_of('blob_sources');
