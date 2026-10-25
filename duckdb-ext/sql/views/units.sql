-- units and tags across languages, read after every lang/*.sql file.
--
-- units(unit_id, file_id, node_id, parent_unit_id, kind, name, qualified_name,
--       start_line, end_line, loc) and tags(file_id, node_id, tag, start_line, end_line)
-- (the tagged node's lines) use the neutral vocabulary every language view maps to:
-- kinds function, method, impl, type, module, macro; tags test and generated_or_macro
-- (plus language-specific ones such as Rust's trait_impl). Rust is the only language so far; a second one turns each view into a
-- UNION ALL (with unit_id renumbered in file_id order, so that a file's units stay
-- consecutive: refresh.sql relies on it).
--
-- derived_units and derived_tags are the union itself, computed from `nodes` for the
-- files of `scope_files`. units and tags read them; cache.sql and refresh.sql replace
-- units and tags with views over their copies, while refresh.sql keeps computing new
-- rows from derived_units and derived_tags.

CREATE OR REPLACE VIEW derived_units AS SELECT * FROM rust_units;

CREATE OR REPLACE VIEW derived_tags AS SELECT * FROM rust_tags;

CREATE OR REPLACE VIEW units AS SELECT * FROM derived_units;

CREATE OR REPLACE VIEW tags AS SELECT * FROM derived_tags;

-- impl_parts(file_id, node_id, part, start_line, start_column, head_end_line,
-- head_end_column): the trait and self type parts of each impl unit's node
-- (lang/rust.sql, rust_impl_parts), which views/graph.sql resolves to units. Read live
-- from `nodes` (whichever nodes are in place); neither cache stores it.
CREATE OR REPLACE VIEW impl_parts AS SELECT * FROM rust_impl_parts;

-- blob_units(blob_id, unit_ord, parent_unit_ord, node_id, kind, name, qualified_name,
--            start_line, end_line, loc): the units of every blob of views/blobs.sql,
-- derived by the same rules as units (the language views' *_blob_units). unit_ord is
-- the unit's position within its blob (the order of unit_id within a file) and
-- parent_unit_ord its innermost enclosing unit's, so the rows of a blob do not depend
-- on which other blobs are in the snapshot. refresh.sql replaces blob_units with a view
-- over its copy and keeps computing new rows from derived_blob_units.
CREATE OR REPLACE VIEW derived_blob_units AS
SELECT l.blob_id,
       u.unit_id - min(u.unit_id) OVER (PARTITION BY u.file_id) AS unit_ord,
       u.parent_unit_id - min(u.unit_id) OVER (PARTITION BY u.file_id) AS parent_unit_ord,
       u.node_id, u.kind, u.name, u.qualified_name, u.start_line, u.end_line, u.loc
FROM rust_blob_units u JOIN blob_list l ON l.blob_ord = u.file_id;

CREATE OR REPLACE VIEW blob_units AS SELECT * FROM derived_blob_units;
