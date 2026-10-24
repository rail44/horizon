"""Language-neutral extraction of syntax nodes, units and tags from adapters."""

import fnmatch
from functools import cache
import re

import tree_sitter

import languages


class Grammar:
    """One compiled grammar plus its adapter's queries."""

    def __init__(self, adapter, extension):
        self.adapter = adapter
        self.language = tree_sitter.Language(adapter.grammars[extension]())
        self.parser = tree_sitter.Parser(self.language)
        self.unit_query = tree_sitter.Query(self.language, adapter.unit_query)
        self.unit_kinds = {}
        for i in range(self.unit_query.capture_count):
            capture = self.unit_query.capture_name(i)
            if capture.startswith("unit."):
                self.unit_kinds[capture] = capture.removeprefix("unit.")
            elif capture != "name":
                raise ValueError(f"{adapter.name}: unit query capture @{capture} is neither @unit.<kind> nor @name")
        self.tag_queries = {}
        for tag, source in adapter.tag_queries.items():
            query = tree_sitter.Query(self.language, source)
            names = [query.capture_name(i) for i in range(query.capture_count)]
            if "tagged" not in names:
                raise ValueError(f"{adapter.name}: tag query {tag!r} must capture @tagged")
            self.tag_queries[tag] = query

    def parse(self, data):
        return self.parser.parse(data)


@cache
def grammar(adapter_name, extension):
    return Grammar(languages.ADAPTERS[adapter_name], extension)


def grammar_for(path, adapters):
    for adapter in adapters:
        for extension in adapter.grammars:
            if path.endswith(extension):
                return grammar(adapter.name, extension)
    return None


def path_matches(path, patterns):
    return any(
        fnmatch.fnmatchcase(path, pattern)
        or (pattern.startswith("**/") and fnmatch.fnmatchcase(path, pattern[3:]))
        for pattern in patterns
    )


WHITESPACE = re.compile(r"\s+")


def node_text(node, data):
    return WHITESPACE.sub(" ", data[node.start_byte : node.end_byte].decode("utf-8", "replace")).strip()


def nonblank_lines(lines, start_row, end_row):
    return sum(1 for line in lines[start_row : end_row + 1] if line.strip())


def units(g, tree, data):
    """Return units sorted by position, each with its parent's index and qualified name."""
    chosen = {}
    for pattern, captures in tree_sitter.QueryCursor(g.unit_query).matches(tree.root_node):
        for capture, kind in g.unit_kinds.items():
            for node in captures.get(capture, []):
                names = sorted(captures.get("name", []), key=lambda n: n.start_byte)
                name = g.adapter.name_separator.join(node_text(n, data) for n in names)
                previous = chosen.get(node.id)
                if previous is None or pattern < previous[0]:
                    chosen[node.id] = (pattern, node, kind, name)
    ordered = sorted(chosen.values(), key=lambda c: (c[1].start_byte, -c[1].end_byte, c[0]))
    lines = data.split(b"\n")
    result = []
    stack = []  # indexes into result of currently open enclosing units
    for _, node, kind, name in ordered:
        while stack and result[stack[-1]]["node"].end_byte < node.end_byte:
            stack.pop()
        parent = stack[-1] if stack else None
        qualified = name if parent is None else result[parent]["qualified_name"] + "." + name
        result.append({
            "node": node,
            "kind": kind,
            "name": name,
            "qualified_name": qualified,
            "parent": parent,
            "start_row": node.start_point.row,
            "end_row": node.end_point.row,
            "loc": nonblank_lines(lines, node.start_point.row, node.end_point.row),
        })
        stack.append(len(result) - 1)
    return result


def nodes(tree, file_id):
    """Preorder rows of the full concrete syntax tree and a map from tree-sitter ids."""
    rows = []
    ids = {}
    cursor = tree.walk()
    parents = []
    while True:
        node = cursor.node
        node_id = len(rows)
        ids[node.id] = node_id
        start, end = node.start_point, node.end_point
        rows.append((
            file_id, node_id, parents[-1] if parents else None, node.type, node.is_named,
            node.start_byte, node.end_byte, start.row, start.column, end.row, end.column, len(parents),
        ))
        if cursor.goto_first_child():
            parents.append(node_id)
            continue
        while not cursor.goto_next_sibling():
            if not cursor.goto_parent():
                return rows, ids
            parents.pop()


def tags(g, tree, path):
    """Yield (tree-sitter node id, tag) pairs, including file-level tags on the root."""
    found = set()
    for tag, patterns in g.adapter.file_tags.items():
        if path_matches(path, patterns):
            found.add((tree.root_node.id, tag))
    for tag, query in g.tag_queries.items():
        for node in tree_sitter.QueryCursor(query).captures(tree.root_node).get("tagged", []):
            found.add((node.id, tag))
    return found
