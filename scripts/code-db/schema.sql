-- Language-neutral code database. Row/column numbers are 0-based (tree-sitter
-- and SCIP convention) unless a column is named *_start/*_lines after git's
-- 1-based hunk header fields. End rows are inclusive.

CREATE TABLE build_info (key VARCHAR PRIMARY KEY, value VARCHAR);

CREATE TABLE files (
    file_id INTEGER PRIMARY KEY,
    path VARCHAR NOT NULL UNIQUE,
    language VARCHAR NOT NULL,
    size_bytes BIGINT NOT NULL,
    blob_sha VARCHAR NOT NULL,
    parse_error BOOLEAN NOT NULL
);

CREATE TABLE nodes (
    file_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    parent_id INTEGER,
    kind VARCHAR NOT NULL,
    is_named BOOLEAN NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    start_row INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_row INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    depth INTEGER NOT NULL
);

CREATE TABLE units (
    unit_id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL,
    node_id INTEGER NOT NULL,
    parent_unit_id INTEGER,
    kind VARCHAR NOT NULL,
    name VARCHAR NOT NULL,
    qualified_name VARCHAR NOT NULL,
    start_row INTEGER NOT NULL,
    end_row INTEGER NOT NULL,
    loc INTEGER NOT NULL
);

CREATE TABLE tags (file_id INTEGER NOT NULL, node_id INTEGER NOT NULL, tag VARCHAR NOT NULL);

CREATE TABLE commits (
    sha VARCHAR PRIMARY KEY,
    author_time TIMESTAMP NOT NULL,  -- UTC
    author_name VARCHAR NOT NULL,
    subject VARCHAR NOT NULL,
    parent_count INTEGER NOT NULL,
    is_merge BOOLEAN NOT NULL
);

CREATE TABLE file_changes (
    sha VARCHAR NOT NULL,
    path VARCHAR NOT NULL,
    old_path VARCHAR,
    status VARCHAR NOT NULL,
    added INTEGER,    -- NULL for binary files
    deleted INTEGER
);

CREATE TABLE hunks (
    sha VARCHAR NOT NULL,
    path VARCHAR NOT NULL,
    old_start INTEGER NOT NULL,  -- 1-based, as in "@@ -old_start,old_lines +new_start,new_lines @@"
    old_lines INTEGER NOT NULL,
    new_start INTEGER NOT NULL,
    new_lines INTEGER NOT NULL
);

CREATE TABLE unit_changes (
    sha VARCHAR NOT NULL,
    path VARCHAR NOT NULL,
    unit_kind VARCHAR NOT NULL,
    unit_qualified_name VARCHAR NOT NULL,
    change_kind VARCHAR NOT NULL  -- modified | added | removed
);

CREATE TABLE clone_pairs (
    pair_id INTEGER PRIMARY KEY,
    group_id INTEGER NOT NULL,
    source VARCHAR NOT NULL,
    file_a VARCHAR NOT NULL,
    start_row_a INTEGER NOT NULL,
    end_row_a INTEGER NOT NULL,
    file_b VARCHAR NOT NULL,
    start_row_b INTEGER NOT NULL,
    end_row_b INTEGER NOT NULL,
    tokens INTEGER,
    lines INTEGER
);

CREATE TABLE scip_symbols (symbol VARCHAR PRIMARY KEY, kind VARCHAR, display_name VARCHAR);

CREATE TABLE scip_occurrences (
    file_id INTEGER NOT NULL,
    symbol VARCHAR NOT NULL,
    start_row INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_row INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    is_definition BOOLEAN NOT NULL,
    is_reference BOOLEAN NOT NULL,
    is_import BOOLEAN NOT NULL
);
