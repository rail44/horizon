-- Question: how many file pairs pass each coupling threshold?
-- Same counting as file_cochange (min_shared and max_commit_files apply), for
-- both bases, at 10/20/30/40/50 %.
-- Evidence: links drop sharply as the co-change threshold rises (MDPI Software
-- 2024, five repositories, low-tier venue); no threshold is validated.
-- @param min_shared 3
-- @param max_commit_files 50
-- @param include_merges 0
WITH eligible AS (
    SELECT fc.sha
    FROM file_changes fc
    JOIN commits c USING (sha)
    WHERE NOT c.is_merge OR $include_merges = 1
    GROUP BY fc.sha
    HAVING count(DISTINCT fc.path) <= $max_commit_files
),
touches AS (
    SELECT DISTINCT fc.sha, fc.path
    FROM file_changes fc
    JOIN eligible USING (sha)
    JOIN files f ON f.path = fc.path
),
revs AS (SELECT path, count(*) AS n FROM touches GROUP BY path),
pairs AS (
    SELECT a.path AS path_a, b.path AS path_b, count(*) AS shared
    FROM touches a JOIN touches b ON a.sha = b.sha AND a.path < b.path
    GROUP BY ALL
    HAVING count(*) >= $min_shared
),
scored AS (
    SELECT 100.0 * p.shared / greatest(ra.n, rb.n) AS pct_max,
           100.0 * p.shared / ((ra.n + rb.n) / 2.0) AS pct_average
    FROM pairs p
    JOIN revs ra ON ra.path = p.path_a
    JOIN revs rb ON rb.path = p.path_b
)
SELECT t.threshold_pct,
       count(s.pct_max) FILTER (WHERE s.pct_max >= t.threshold_pct) AS pairs_basis_max,
       count(s.pct_average) FILTER (WHERE s.pct_average >= t.threshold_pct) AS pairs_basis_average
FROM (SELECT unnest([10, 20, 30, 40, 50]) AS threshold_pct) t
LEFT JOIN scored s ON true
GROUP BY t.threshold_pct
ORDER BY t.threshold_pct
