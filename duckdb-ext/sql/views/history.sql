-- commits, file_changes and hunks: git history of `repo` over the git_history
-- extension (duckdb-ext/README.md, "Functions"), computed when a query scans them.
--
-- `since` (unset: all of HEAD) and `include_merges` (default false) are read at each
-- scan. Paths are repository-relative; numbers in hunks are git's 1-based
-- `@@ -old_start,old_lines +new_start,new_lines @@` fields.

-- commits(sha, author_time, author_name, subject, parent_count, is_merge)
CREATE OR REPLACE VIEW commits AS
SELECT * FROM git_commits(getvariable('repo'), since := getvariable('since'),
                          include_merges := getvariable('include_merges'));

-- file_changes(sha, path, old_path, status, added, deleted): all files, every language.
CREATE OR REPLACE VIEW file_changes AS
SELECT * FROM git_file_changes(getvariable('repo'), since := getvariable('since'),
                               include_merges := getvariable('include_merges'));

-- hunks(sha, path, old_start, old_lines, new_start, new_lines): only files whose
-- extension is in `languages`, whether or not they still exist.
CREATE OR REPLACE VIEW hunks AS
SELECT * FROM git_hunks(getvariable('repo'), since := getvariable('since'),
                        include_merges := getvariable('include_merges'))
WHERE getvariable('languages')[regexp_extract(path, '\.([^./]+)$', 1)] IS NOT NULL;
