# /// script
# requires-python = ">=3.12"
# dependencies = ["duckdb==2.0.0"]
# ///
"""Compare sitting_duck's read_ast and the prototype views with a spike database.

Usage (from the repository root, after building the spike database):

    uv run --script scripts/code-db/views/evaluate.py target/code-db/horizon.duckdb [section ...]

Sections: nodes, units, tags, builtin (default: all). Parses exactly the spike's `files`
with read_ast in an in-memory database, runs rust_units.sql and rust_tags.sql over it and
prints counts and set differences against the spike's nodes, units and tags.
"""

import pathlib
import sys
import time

import duckdb

HERE = pathlib.Path(__file__).parent
EXAMPLES = 12


def run_script(c, path):
    for statement in path.read_text().split(";\n"):
        if statement.strip():
            c.execute(statement)


def show(c, title, sql):
    print(f"\n## {title}")
    print(c.sql(sql).show(max_rows=60, max_width=250) or "", end="")


def main():
    spike = sys.argv[1]
    sections = set(sys.argv[2:]) or {"nodes", "units", "tags", "builtin"}
    c = duckdb.connect()
    c.execute("INSTALL sitting_duck FROM community")
    c.execute("LOAD sitting_duck")
    c.execute(f"ATTACH '{spike}' AS spike (READ_ONLY)")
    c.execute("CREATE TABLE sd_files AS SELECT file_id, path FROM spike.files")
    paths = [r[0] for r in c.sql("SELECT path FROM sd_files ORDER BY file_id").fetchall()]
    listing = "[" + ",".join("'" + p.replace("'", "''") + "'" for p in paths) + "]"
    t0 = time.monotonic()
    c.execute(f"""
        CREATE TABLE sd_nodes AS
        SELECT file_path, node_id, parent_id, type, semantic_type, flags, name, qualified_name,
               start_line, end_line, start_column, end_column, start_byte, end_byte,
               depth, sibling_index, children_count, descendant_count
        FROM read_ast({listing}, 'rust', source := 'full', peek := 'none')""")
    t1 = time.monotonic()
    c.execute(f"CREATE TABLE sd_source AS SELECT filename AS file_path, content FROM read_text({listing})")
    print(f"read_ast: {t1 - t0:.2f}s, files {len(paths)}, "
          f"rows {c.sql('SELECT count(*) FROM sd_nodes').fetchall()[0][0]}")

    c.execute("""
        CREATE TABLE s_units AS
        SELECT f.path AS file_path, u.node_id, p.node_id AS parent_node_id, u.kind, u.name,
               u.qualified_name, u.start_row, u.end_row, u.loc
        FROM spike.units u JOIN spike.files f USING (file_id)
        LEFT JOIN spike.units p ON p.unit_id = u.parent_unit_id""")
    c.execute("""
        CREATE TABLE s_tags AS
        SELECT f.path AS file_path, t.node_id, t.tag FROM spike.tags t JOIN spike.files f USING (file_id)""")

    if "nodes" in sections:
        show(c, "node counts per file", """
            WITH a AS (SELECT file_id, count(*) n FROM spike.nodes GROUP BY 1),
                 b AS (SELECT f.file_id, count(*) n FROM sd_nodes s JOIN sd_files f ON f.path = s.file_path GROUP BY 1)
            SELECT count(*) files, count(*) FILTER (WHERE a.n = b.n) equal_count FROM a FULL JOIN b USING (file_id)""")
        show(c, "rows differing on (file, node_id, parent_id, kind, bytes, rows, cols, depth)", """
            WITH x AS (SELECT file_id, node_id, parent_id, kind, start_byte, end_byte,
                              start_row, start_col, end_row, end_col, depth FROM spike.nodes),
                 y AS (SELECT f.file_id, node_id, parent_id, type, start_byte, end_byte,
                              start_line - 1, start_column - 1, end_line - 1, end_column - 1, depth
                       FROM sd_nodes s JOIN sd_files f ON f.path = s.file_path)
            SELECT (SELECT count(*) FROM (FROM x EXCEPT ALL FROM y)) only_spike,
                   (SELECT count(*) FROM (FROM y EXCEPT ALL FROM x)) only_sitting_duck""")

    if "units" in sections or "tags" in sections:
        t2 = time.monotonic()
        run_script(c, HERE / "rust_units.sql")
        c.execute("CREATE TABLE v_units AS SELECT * FROM rust_units")
        t3 = time.monotonic()
        run_script(c, HERE / "rust_tags.sql")
        c.execute("CREATE TABLE v_tags AS SELECT * FROM rust_tags")
        t4 = time.monotonic()
        print(f"\nviews: units {t3 - t2:.2f}s, tags {t4 - t3:.2f}s")

    if "units" in sections:
        show(c, "units by kind", """
            SELECT kind, s.n spike, v.n view FROM
              (SELECT kind, count(*) n FROM s_units GROUP BY 1) s
              FULL JOIN (SELECT kind, count(*) n FROM v_units GROUP BY 1) v USING (kind) ORDER BY 1""")
        cols = "file_path, node_id, parent_node_id, kind, name, qualified_name, start_row, end_row, loc"
        show(c, "unit rows differing (all columns)", f"""
            SELECT (SELECT count(*) FROM (SELECT {cols} FROM s_units EXCEPT ALL SELECT {cols} FROM v_units)) only_spike,
                   (SELECT count(*) FROM (SELECT {cols} FROM v_units EXCEPT ALL SELECT {cols} FROM s_units)) only_view""")
        show(c, "unit differences by column (matched on file_path, node_id)", """
            SELECT count(*) FILTER (WHERE s.node_id IS NULL) missing_in_spike,
                   count(*) FILTER (WHERE v.node_id IS NULL) missing_in_view,
                   count(*) FILTER (WHERE s.kind <> v.kind) kind,
                   count(*) FILTER (WHERE s.name <> v.name) name,
                   count(*) FILTER (WHERE s.qualified_name <> v.qualified_name) qualified_name,
                   count(*) FILTER (WHERE s.parent_node_id IS DISTINCT FROM v.parent_node_id) parent,
                   count(*) FILTER (WHERE s.start_row <> v.start_row OR s.end_row <> v.end_row) rows,
                   count(*) FILTER (WHERE s.loc <> v.loc) loc
            FROM s_units s FULL JOIN v_units v USING (file_path, node_id)""")
        show(c, "unit difference examples", f"""
            SELECT 'spike' side, * FROM (SELECT {cols} FROM s_units EXCEPT ALL SELECT {cols} FROM v_units)
            UNION ALL
            SELECT 'view' side, * FROM (SELECT {cols} FROM v_units EXCEPT ALL SELECT {cols} FROM s_units)
            ORDER BY file_path, node_id, side LIMIT {EXAMPLES}""")

    if "tags" in sections:
        show(c, "tags by tag", """
            SELECT tag, s.n spike, v.n view FROM
              (SELECT tag, count(*) n FROM s_tags GROUP BY 1) s
              FULL JOIN (SELECT tag, count(*) n FROM v_tags GROUP BY 1) v USING (tag) ORDER BY 1""")
        show(c, "tag differences by tag and node type", """
            WITH d AS (SELECT 'only_spike' side, * FROM (FROM s_tags EXCEPT FROM v_tags)
                       UNION ALL SELECT 'only_view', * FROM (FROM v_tags EXCEPT FROM s_tags))
            SELECT side, tag, n.type, count(*) FROM d JOIN sd_nodes n USING (file_path, node_id)
            GROUP BY ALL ORDER BY ALL""")
        show(c, "tag difference examples", f"""
            WITH d AS (SELECT 'only_spike' side, * FROM (FROM s_tags EXCEPT FROM v_tags)
                       UNION ALL SELECT 'only_view', * FROM (FROM v_tags EXCEPT FROM s_tags))
            SELECT side, tag, d.file_path, d.node_id, n.type, n.start_line
            FROM d JOIN sd_nodes n USING (file_path, node_id) ORDER BY ALL LIMIT {EXAMPLES}""")

    if "builtin" in sections:
        # sitting_duck's own classification against the spike's function/method units.
        c.execute("""
            CREATE TABLE sd_defs AS
            SELECT file_path, node_id, type, semantic_type::VARCHAR AS semantic_type, name,
                   ast_qualified_name_as_string(qualified_name) AS qualified_name
            FROM sd_nodes WHERE is_function_definition(semantic_type)""")
        show(c, "sitting_duck function definitions by node type", """
            SELECT type, semantic_type, count(*) FROM sd_defs GROUP BY ALL ORDER BY 3 DESC""")
        show(c, "spike function/method units found by is_function_definition", """
            SELECT s.kind, count(*) units, count(d.node_id) identified,
                   count(*) FILTER (WHERE d.name = s.name) same_name
            FROM s_units s LEFT JOIN sd_defs d USING (file_path, node_id)
            WHERE s.kind IN ('function', 'method') GROUP BY ALL ORDER BY 1""")
        show(c, "is_function_definition nodes that are not spike function/method units", """
            SELECT d.type, coalesce(s.kind, '(none)') spike_kind, count(*)
            FROM sd_defs d LEFT JOIN s_units s USING (file_path, node_id)
            WHERE s.kind IS NULL OR s.kind NOT IN ('function', 'method') GROUP BY ALL ORDER BY 3 DESC""")
        show(c, "name differences (examples)", f"""
            SELECT s.kind, s.name spike_name, d.name sd_name, s.qualified_name spike_qn, d.qualified_name sd_qn
            FROM s_units s JOIN sd_defs d USING (file_path, node_id)
            WHERE d.name IS DISTINCT FROM s.name LIMIT {EXAMPLES}""")
        show(c, "sitting_duck names for all spike units (by kind)", """
            SELECT s.kind, count(*) units, count(*) FILTER (WHERE n.name = s.name) same_name,
                   count(*) FILTER (WHERE n.name IS NULL OR n.name = '') no_name
            FROM s_units s JOIN sd_nodes n USING (file_path, node_id) GROUP BY ALL ORDER BY 1""")


if __name__ == "__main__":
    main()
