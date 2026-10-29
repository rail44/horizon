-- Question: which definitions does nothing in the indexed code reference?
-- Axis: unreferenced definitions (docs/research/code-quality-axes-2026-10-29.md §3.16).
-- Rows: every unit whose own symbol has no reference occurrence in any index
-- (unit_reference_status, views/quality.sql), with status: 'unreferenced', or the reason
-- the unit is structurally unreferenced rather than dead: 'test', 'trait_impl' (calls
-- resolve to the trait's item, not the impl's), 'entry_point' (main), 'crate_root',
-- 'trait_item' (a trait's own item: its impls implement it). Other
-- entry points (callbacks registered by name, FFI exports, uses a derive generates, macro
-- bodies, a name used only inside a format string such as "{USAGE}" or an attribute's
-- string such as serde's deserialize_with) also show as 'unreferenced': rust-analyzer
-- records no reference there; so does a `pub` item kept for a caller outside the
-- indexed workspaces. The compiler's dead_code lint already reports private items; what
-- this adds is mostly `pub` items no crate of the repository uses. Largest first.
-- Evidence: weak. Romano et al. (TSE 2020) conclude that dead code is widespread and
-- worth avoiding (effect sizes not read).
-- Judgment from the settings file: scope and tests (scope.exclude, tests.*). Needs the
-- SCIP indexes.
-- Parameters (DuckDB variables; default when unset): include_structural false (true: the
-- structurally unreferenced rows too).
WITH params AS (
    SELECT coalesce(CAST(getvariable('include_structural') AS BOOLEAN), false) AS include_structural
)
SELECT u.path, u.crate, u.kind, u.qualified_name, u.start_line, u.loc, u.visibility, r.status
FROM units u
JOIN unit_reference_status r USING (unit_id)
CROSS JOIN params p
WHERE r.status <> 'referenced' AND (p.include_structural OR r.status = 'unreferenced')
ORDER BY r.status = 'unreferenced' DESC, u.loc DESC, u.path, u.start_line;
