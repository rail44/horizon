-- Retrospective, step 2 with the history signals counted by identity, as before lineage:
-- the same variables as signals.sql, which this file runs after replacing
-- unit_lineage_segments with each current unit's own identity over the whole history.
-- Every history signal reads its commits through that view (unit_churn directly,
-- unit_fan_in_churn, clone_change_consistency and unit_commits through unit_history), so
-- each then counts the commits of the unit's (path, kind, qualified_name) in
-- unit_changes: unit_churn's commits equal its commits_by_name. The ground truth is not
-- affected. The replacement lives in this session only; the cache is not touched.
-- Needs views/graph.sql (the SCIP index of the baseline).
CREATE OR REPLACE VIEW unit_lineage_segments AS
SELECT u.unit_id, f.path, u.kind, u.qualified_name, 0::BIGINT AS from_ord,
       9223372036854775807::BIGINT AS until_ord, 0 AS depth, NULL::VARCHAR AS link, NULL::VARCHAR AS sha,
       1::BIGINT AS successors, true AS is_self
FROM units u JOIN files f USING (file_id);

.read duckdb-ext/sql/retro/signals.sql
