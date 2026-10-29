-- Copy one side of a comparison (compare/sides.sql) into the database attached as
-- `snapshot`, which `code-db compare` then attaches read-only as `before` for
-- compare/views.sql. side_meta records the repository and when the copy was made.
CREATE TABLE snapshot.side_units AS SELECT * FROM side_units;
CREATE TABLE snapshot.side_modules AS SELECT * FROM side_modules;
CREATE TABLE snapshot.side_clones AS SELECT * FROM side_clones;
CREATE TABLE snapshot.side_module_edges AS SELECT * FROM side_module_edges;
CREATE TABLE snapshot.side_crate_edges AS SELECT * FROM side_crate_edges;
CREATE TABLE snapshot.side_cycles AS SELECT * FROM side_cycles;
CREATE TABLE snapshot.side_layer_violations AS SELECT * FROM side_layer_violations;
CREATE TABLE snapshot.side_meta AS SELECT getvariable('repo') AS repo, now()::TIMESTAMP AS taken_at;
