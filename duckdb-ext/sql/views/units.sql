-- units and tags across languages, read after every lang/*.sql file.
--
-- units(unit_id, file_id, node_id, parent_unit_id, kind, name, qualified_name,
--       start_line, end_line, loc) and tags(file_id, node_id, tag, start_line, end_line)
-- (the tagged node's lines) use the neutral vocabulary every language view maps to:
-- kinds function, method, impl, type, module, macro; tags test and generated_or_macro
-- (plus language-specific ones such as Rust's trait_impl). Rust is the only language so far; a second one turns each view into a
-- UNION ALL (with unit_id renumbered).

CREATE OR REPLACE VIEW units AS SELECT * FROM rust_units;

CREATE OR REPLACE VIEW tags AS SELECT * FROM rust_tags;
