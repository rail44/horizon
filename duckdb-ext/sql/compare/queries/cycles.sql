-- Question: which module cycles appeared, disappeared, grew or shrank, and which modules
-- joined or left them?
-- Rows: every row of compare_cycles: a BEFORE and an AFTER cycle sharing a module (kept,
-- flagged split or merged when a cycle shares modules with several on the other side),
-- or a cycle sharing none (appeared, disappeared); sizes, members added and removed, and
-- the unit edges inside on both sides. Cycles follow cycle_edges (views/graph.sql) on
-- both sides. Production before test (a cycle is test when every member is a test
-- module); the largest change of size first.
-- Run by `code-db compare` (README.md, Comparison). Thresholds: none.
SELECT c.status, CASE WHEN c.is_test THEN 'test' ELSE 'production' END AS scope, c.crate, c.cycle_before,
       c.cycle_after, c.modules_before, c.modules_after, c.modules_delta, c.members_added, c.members_removed,
       c.unit_edges_before, c.unit_edges_after, c.split, c.merged
FROM compare_cycles c
ORDER BY c.is_test, abs(c.modules_delta) DESC, len(c.members_added) + len(c.members_removed) DESC, c.status,
         c.cycle_before, c.cycle_after;
