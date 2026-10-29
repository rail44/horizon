-- Question: which module dependencies go against the layers the repository declares?
-- Rows: one per module edge (module_edges, views/graph.sql) whose source module is in a
-- layer that may not depend on the target module's layer (module_layer_violations: the
-- layers and their may_depend_on come from the settings file's [[layers]], README.md,
-- Settings file; a module in no layer is unconstrained). The edge's counts are
-- module_edges'; nested_modules marks an edge between a module and its ancestor or
-- descendant; cycle is the module cycle (module_cycle_edges) the edge is inside, NULL
-- when it is in none. No layers, no rows.
-- Axis: independence (docs/research/code-quality-axes-2026-10-29.md §3.8).
-- Evidence: weak, none on Rust; layering violations are among the architecture smells of
-- the survey. Which dependencies are intended is the repository's own decision, which is
-- why the layers are a setting: [[layers]] and may_depend_on of the settings file. Needs
-- the SCIP indexes.
SELECT v.src_layer, v.dst_layer, v.src_module, v.dst_module, e.unit_edges, e.occurrences,
       e.test_unit_edges, e.ambiguous_unit_edges, e.nested_modules, c.cycle
FROM module_layer_violations v
JOIN module_edges e USING (src_module, dst_module)
LEFT JOIN module_cycle_edges c USING (src_module, dst_module)
ORDER BY v.src_layer, v.dst_layer, e.unit_edges DESC, v.src_module, v.dst_module;
