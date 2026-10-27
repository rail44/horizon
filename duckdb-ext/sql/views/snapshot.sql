-- The before side of the before/after comparison (README.md, "Before/after comparison"):
-- the source files and units of an earlier commit, `compare_before`, taken from the blob
-- layer through git_tree (no checkout), and the correspondence of the current units to
-- them through the lineage. The after side is the current snapshot (files, units).
-- Read after views/lineage.sql.
--
-- compare_before is any revision git_tree accepts, read at each scan like the other
-- variables. Unset, the snapshot views show HEAD's tree and compare_commit stops with a
-- message. The before commit must be a commit of `commits` (an ancestor of HEAD within
-- since..HEAD; a merge only with include_merges), and the snapshot is complete only when
-- the blob layer holds every blob of its tree: a file unchanged since before `since` is
-- missing (snapshot_missing), so compare with the whole history cached (no `since`).

-- compare_commit(sha, ord): compare_before resolved to a commit of `commits`, with its
-- commit_order position. The commit is the one git log lists in compare_before^..HEAD
-- but not in compare_before..HEAD: exactly compare_before when it is an ancestor of
-- HEAD that is not a merge (a merge whose second parent brings other commits lists
-- those too, and stops here). A root commit has no `^` and fails when the view is
-- bound.
CREATE OR REPLACE VIEW compare_commit AS
WITH listed AS (
    SELECT sha FROM git_commits(getvariable('repo'), since := getvariable('compare_before') || '^',
                                include_merges := true)
    EXCEPT
    SELECT sha FROM git_commits(getvariable('repo'), since := getvariable('compare_before'),
                                include_merges := true)
),
resolved AS (
    SELECT count(*) AS n, any_value(l.sha) AS sha, any_value(o.ord) AS ord
    FROM listed l LEFT JOIN commit_order o USING (sha)
)
SELECT CASE
           WHEN getvariable('compare_before') IS NULL
               THEN error('set the variable compare_before to the revision to compare HEAD with')
           WHEN n = 1 AND ord IS NOT NULL THEN sha
           ELSE error('compare_before ' || getvariable('compare_before') || ' is not a commit of `commits`: '
                      || 'it must be an ancestor of HEAD within since..HEAD, and not a merge unless include_merges')
       END AS sha,
       ord
FROM resolved;

-- compare_range(sha): the commits of compare_before..HEAD (merges included; the readers
-- join them to rows that follow include_merges).
CREATE OR REPLACE VIEW compare_range AS
SELECT sha FROM git_commits(getvariable('repo'), since := coalesce(getvariable('compare_before'), 'HEAD'),
                            include_merges := true);

-- snapshot_files(file_id, path, language, mode, blob_id): the source files of
-- compare_before's tree, by the rule of the current files (is_source_file: regular
-- files, `languages`, `exclude_patterns`); file_id is the position in path order.
CREATE OR REPLACE VIEW snapshot_files AS
SELECT (row_number() OVER (ORDER BY path) - 1)::INTEGER AS file_id, path,
       getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] AS language, mode, blob_id
FROM git_tree(getvariable('repo'), rev := coalesce(getvariable('compare_before'), 'HEAD'))
WHERE is_source_file(path, mode);

-- snapshot_missing(path, mode, blob_id): the files of snapshot_files whose blob is not in
-- the blob layer (blob_list, views/blobs.sql), so that their units are unknown. A blob in
-- the layer with no units (a file of `use` declarations) is not missing.
CREATE OR REPLACE VIEW snapshot_missing AS
SELECT path, mode, blob_id FROM snapshot_files
WHERE blob_id NOT IN (SELECT blob_id FROM blob_list);

-- snapshot_units(unit_id, file_id, node_id, parent_unit_id, kind, name, qualified_name,
--     start_line, end_line, loc, lead_start_line, blob_id): the units of snapshot_files,
-- from blob_units, numbered as units are (file order, then position in the file, so an
-- enclosing unit comes before what it contains and a file's units are consecutive).
CREATE OR REPLACE VIEW snapshot_units AS
SELECT unit_id, file_id, node_id, unit_id - unit_ord + parent_unit_ord AS parent_unit_id, kind, name,
       qualified_name, start_line, end_line, loc, lead_start_line, blob_id
FROM (
    SELECT row_number() OVER (ORDER BY f.file_id, u.unit_ord) - 1 AS unit_id, f.file_id, f.blob_id,
           u.* EXCLUDE (blob_id)
    FROM snapshot_files f JOIN blob_units u USING (blob_id)
);

-- snapshot_nodes, snapshot_impl_parts, snapshot_tags: the graph inputs of the before side
-- (views/graph.sql). snapshot_nodes is blob_nodes keyed by snapshot file_id (from the
-- persistent cache's copy when refresh.sql was read; live it parses every blob);
-- snapshot_impl_parts applies the language rule of impl_parts to it. Blobs have no tags
-- (README.md, "Deliberately not here yet"), so snapshot_tags is empty: no unit of the
-- before side is a test and no occurrence lies in a macro.
CREATE OR REPLACE VIEW snapshot_nodes AS
SELECT f.file_id, n.* EXCLUDE (blob_id)
FROM snapshot_files f JOIN blob_nodes n USING (blob_id);

CREATE OR REPLACE VIEW snapshot_impl_parts AS FROM impl_parts_over('snapshot_nodes');

CREATE OR REPLACE VIEW snapshot_tags AS
SELECT NULL::INTEGER AS file_id, NULL::BIGINT AS node_id, NULL::VARCHAR AS tag, NULL::BIGINT AS start_line,
       NULL::BIGINT AS end_line
WHERE false;

-- unit_at_before(unit_id, path, kind, qualified_name, depth, link, sha): for each current
-- unit, its identities at compare_before: the identities of its lineage
-- (unit_lineage_segments) whose commit interval [from_ord, until_ord] contains
-- compare_before's position and that are units of snapshot_units. Depth 0 is the unit's
-- own identity, when it reaches back that far; depth n an identity n links back, reached
-- through `link` at commit `sha`. A unit with none was added after compare_before (or
-- its lineage restarted after it: the stub of a body move); a unit with several
-- continues several earlier units (a merge), and an identity reached by several units
-- was split. An identity reached along several paths is listed once, at its least depth.
CREATE OR REPLACE VIEW unit_at_before AS
WITH before AS MATERIALIZED (
    SELECT ord FROM compare_commit
),
identities AS MATERIALIZED (
    SELECT DISTINCT f.path, u.kind, u.qualified_name
    FROM snapshot_units u JOIN snapshot_files f USING (file_id)
)
SELECT s.unit_id, s.path, s.kind, s.qualified_name, min(s.depth) AS depth,
       arg_min(s.link, s.depth) AS link, arg_min(s.sha, s.depth) AS sha
FROM unit_lineage_segments s
JOIN before b ON s.from_ord <= b.ord AND b.ord <= s.until_ord
JOIN identities i USING (path, kind, qualified_name)
GROUP BY s.unit_id, s.path, s.kind, s.qualified_name;
