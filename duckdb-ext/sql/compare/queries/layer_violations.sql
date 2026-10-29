-- Question: which module dependencies against the declared layers appeared or
-- disappeared?
-- Rows: every row of compare_layer_violations (both snapshots read with AFTER's
-- settings file, so the layers are the same on both sides): appeared, disappeared, or
-- kept with the unit edges on both sides. Production before test (no unit edge without a
-- test end on either side); appeared, then disappeared, then kept; the heaviest first. No
-- layers, no rows.
-- Run by `code-db compare` (README.md, Comparison). Thresholds: none.
SELECT v.status, CASE WHEN v.is_test THEN 'test' ELSE 'production' END AS scope, v.src_layer, v.dst_layer,
       v.src_module, v.dst_module, v.unit_edges_before, v.unit_edges_after, v.unit_edges_delta
FROM compare_layer_violations v
ORDER BY v.is_test, CASE v.status WHEN 'appeared' THEN 0 WHEN 'disappeared' THEN 1 ELSE 2 END,
         greatest(v.unit_edges_before, v.unit_edges_after) DESC, v.src_module, v.dst_module;
