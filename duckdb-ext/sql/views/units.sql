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
