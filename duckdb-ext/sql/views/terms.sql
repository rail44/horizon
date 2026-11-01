-- Terms: the views name the two levels above a file by internal names, `package` (what a
-- manifest declares, versions and depends by: a Cargo package, a Go module; SCIP's
-- package) and `namespace` (a unit of namespacing and visibility: a Rust module, a Go
-- package; SCIP's Namespace descriptor), and every view, column, query, parameter and
-- vocabulary value built from them (`package_edges`, `fan_in_packages`, `namespace_cycles`,
-- `level := 'package'`, kind 'namespace'). A reader sees them by the names of the
-- repository's language instead: each language's file under lang/ gives its two official
-- names (<language>_terms), and this file turns one into the other. The same table names
-- the language's control constructs where its words differ from the internal ones (Go's
-- switch, case, func literal for match, arm, closure: views/quality.sql, unit_complexity),
-- and, with a NULL official name, the constructs the language lacks.
--
-- The display language is one language: `lang` when set (00_load.sql then keeps only that
-- language's files), else the one language of `files`; with no language or several, the
-- internal names stay. `code-db` sets `lang` from the command line, stops when a
-- repository has several languages and none is named, translates what it is given
-- (query names, parameter names and values) into internal names, and shows everything in
-- the display language: the views as the schema named after the language (`go.units`,
-- `go.module_edges`... created from display_ddl) and each query's result through
-- display_select.

-- lang_terms(language, internal, official): every language's two names.
CREATE OR REPLACE VIEW lang_terms AS
SELECT 'rust' AS language, internal, official FROM rust_terms
UNION ALL
SELECT 'go', internal, official FROM go_terms;

-- display_language(): the display language (above), or NULL.
CREATE OR REPLACE MACRO display_language() AS
    coalesce(CAST(getvariable('lang') AS VARCHAR),
             (SELECT CASE WHEN count(DISTINCT language) = 1 THEN any_value(language) END FROM files));

-- lang_visibility_boundaries(language, level) and visibility_boundary(): the level whose
-- boundary each language's public names open (`pub` the package's outside in Rust, an
-- exported name the namespace's in Go), and the display language's, else package. The
-- queries about public use count across it (public_surface, intent, interface, coupling's
-- contract_candidate).
CREATE OR REPLACE VIEW lang_visibility_boundaries AS
SELECT 'rust' AS language, level FROM rust_visibility_boundary
UNION ALL
SELECT 'go', level FROM go_visibility_boundary;

CREATE OR REPLACE MACRO visibility_boundary() AS
    coalesce((SELECT level FROM lang_visibility_boundaries WHERE language = display_language()), 'package');

-- term_swap(s, pairs): s with each word of `pairs` ({f, t, i}: from, to, a distinct index)
-- replaced by its counterpart, all at once (Go's names swap package and namespace), a word
-- being a whole `_`-separated part of s or its plural (`namespaces`, `fan_in_packages`), so
-- that `try` never touches `entry_point`.
CREATE OR REPLACE MACRO term_swap(s, pairs) AS
    CASE WHEN s IS NULL OR coalesce(len(pairs), 0) = 0 THEN s ELSE (
        SELECT substr(swapped, 2, length(swapped) - 2)
        FROM (SELECT list_reduce(pairs, lambda acc, p: replace(acc, chr((57344 + p.i)::INTEGER), p.t),
                                 list_reduce(pairs, lambda acc, p:
                                     replace(replace(replace(replace(acc,
                                         '_' || p.f || '_', '_' || chr((57344 + p.i)::INTEGER) || '_'),
                                         '_' || p.f || '_', '_' || chr((57344 + p.i)::INTEGER) || '_'),
                                         '_' || p.f || 's_', '_' || chr((57344 + p.i)::INTEGER) || 's_'),
                                         '_' || p.f || 's_', '_' || chr((57344 + p.i)::INTEGER) || 's_'),
                                     '_' || s || '_')) AS swapped)
    ) END;

-- display_term(s, lang) and internal_term(s, lang): s with lang's internal words replaced
-- by its official names (lang_terms), or the reverse; s itself for a NULL or unknown lang.
-- A row with a NULL official name (a construct the language lacks) renames nothing.
CREATE OR REPLACE MACRO display_term(s, lang) AS term_swap(s, (
    SELECT list({'f': internal, 't': official, 'i': i})
    FROM (SELECT internal, official, row_number() OVER (ORDER BY internal) AS i
          FROM lang_terms WHERE language = lang AND official IS NOT NULL AND official <> internal)));

CREATE OR REPLACE MACRO internal_term(s, lang) AS term_swap(s, (
    SELECT list({'f': official, 't': internal, 'i': i})
    FROM (SELECT internal, official, row_number() OVER (ORDER BY internal) AS i
          FROM lang_terms WHERE language = lang AND official IS NOT NULL AND official <> internal)));

-- display_column(c, t, lang): the select-list item showing column c (of type t) in lang:
-- its name translated, and for the columns holding the vocabulary (kind, level, status,
-- reference_status, resolved_by, unit_kind) its text values too.
CREATE OR REPLACE MACRO display_column(c, t, lang) AS
    CASE WHEN c IN ('kind', 'level', 'status', 'reference_status', 'resolved_by', 'unit_kind')
              AND t = 'VARCHAR'
         THEN 'display_term("' || replace(c, '"', '""') || '", ' || coalesce('''' || lang || '''', 'NULL') || ')'
         ELSE '"' || replace(c, '"', '""') || '"' END
    || ' AS "' || replace(display_term(c, lang), '"', '""') || '"';

-- display_select(v): a SELECT showing the temporary view v in the display language.
CREATE OR REPLACE MACRO display_select(v) AS (
    SELECT 'SELECT ' || string_agg(display_column(column_name, data_type, display_language()), ', '
                                   ORDER BY column_index) || ' FROM temp.' || v
    FROM duckdb_columns() WHERE database_name = 'temp' AND table_name = v);

-- display_ddl(ord, statement): the statements creating the display language's schema, one
-- view per view of the main schema but the languages' own and this file's, named and
-- columned in the language. Empty without a display language.
CREATE OR REPLACE VIEW display_ddl AS
WITH lang AS (
    SELECT display_language() AS lang
),
shown AS (
    SELECT view_name FROM duckdb_views()
    WHERE NOT internal AND database_name = current_database() AND schema_name = 'main' AND NOT temporary
      AND NOT regexp_matches(view_name, '^(rust|go)_')
      AND view_name NOT IN ('derived_units', 'lang_terms', 'lang_visibility_boundaries', 'display_ddl')
),
columns AS (
    SELECT c.table_name, c.column_name, c.column_index, c.data_type
    FROM duckdb_columns() c JOIN shown s ON s.view_name = c.table_name
    WHERE c.database_name = current_database() AND c.schema_name = 'main'
)
SELECT 0 AS ord, 'CREATE SCHEMA IF NOT EXISTS "' || lang || '"' AS statement FROM lang WHERE lang IS NOT NULL
UNION ALL
SELECT 1, 'CREATE OR REPLACE VIEW "' || lang || '"."' || display_term(table_name, lang) || '" AS SELECT '
          || string_agg(display_column(column_name, data_type, lang), ', ' ORDER BY column_index)
          || ' FROM main."' || table_name || '"'
FROM columns, lang WHERE lang IS NOT NULL
GROUP BY table_name, lang
ORDER BY 1, 2;
