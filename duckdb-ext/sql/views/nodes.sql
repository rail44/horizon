-- nodes: every syntax node of every source file, anonymous tokens included, as
-- sitting_duck's read_ast returns them, plus file_id.
--
-- Columns are read_ast's (docs/research/sitting-duck-evaluation-2026-10-24.md):
-- node_id is the preorder index within the file and parent_id the parent's node_id
-- (NULL at the root); start_line/end_line and start_column/end_column are 1-based
-- (columns count bytes); start_byte/end_byte are 0-based, end_byte exclusive. depth,
-- sibling_index (0-based), children_count and descendant_count describe the tree:
-- the subtree of n is node_id in [n.node_id, n.node_id + n.descendant_count].
-- end_line/end_column are the node's end point, so a node that ends with its newline
-- (a line comment) ends at column 1 of the following line.
--
-- context := 'none' skips sitting_duck's semantic context, whose default ('native')
-- is quadratic in the statements of a block; peek := 'none' drops the source excerpt.
-- The language is detected from the file extension.

CREATE OR REPLACE VIEW nodes AS
SELECT f.file_id, a.*
FROM read_ast(getvariable('source_paths'), source := 'full', peek := 'none', context := 'none') a
JOIN files f ON f.path = substr(a.file_path, length(getvariable('repo')) + 2);
