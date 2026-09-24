"""Select and partition Rust sources while retaining original byte/line coordinates."""

import fnmatch
from pathlib import Path
import re

import tree_sitter
import tree_sitter_rust

from tooling import AuditError, git, sha256

LANGUAGE = tree_sitter.Language(tree_sitter_rust.language())
PARSER = tree_sitter.Parser(LANGUAGE)


def walk(node):
    yield node
    for child in node.named_children:
        yield from walk(child)


def text(node, data):
    return data[node.start_byte : node.end_byte].decode() if node else ""


def matches(path, patterns):
    return any(
        fnmatch.fnmatchcase(path, pattern)
        or (pattern.startswith("**/") and fnmatch.fnmatchcase(path, pattern[3:]))
        for pattern in patterns
    )


def attributes(node, data):
    result = []
    previous = node.prev_named_sibling
    start = node.start_byte
    while previous and previous.type in ("attribute_item", "line_comment", "block_comment"):
        start = previous.start_byte
        if previous.type == "attribute_item":
            result.append(text(previous, data))
        previous = previous.prev_named_sibling
    return result, start


def normalized_syntax(node, data):
    """Ignore formatting/comments without changing strings inside cfg predicates."""
    if node.type in ("line_comment", "block_comment"):
        return ""
    if not node.children:
        return text(node, data)
    return "".join(normalized_syntax(child, data) for child in node.children)


def condition(node, data):
    attr = next((child for child in node.named_children if child.type == "attribute"), None)
    value = normalized_syntax(attr, data) if attr else ""
    return value if value.startswith(("cfg(", "cfg_attr(")) else None


def function_variant(node, data):
    """Syntactic identity within this file, not evaluation of a build configuration."""
    ancestors = []
    parent = node
    while parent:
        ancestors.append(parent)
        parent = parent.parent
    parts = []
    for item in reversed(ancestors):
        attrs = []
        previous = item.prev_named_sibling
        while previous and previous.type in ("attribute_item", "line_comment", "block_comment"):
            if previous.type == "attribute_item":
                attrs.append(previous)
            previous = previous.prev_named_sibling
        attrs.reverse()
        # Inner attributes apply to all items in a source file or module body.
        attrs.extend(child for child in item.named_children if child.type == "inner_attribute_item")
        parts.extend(value for attr in attrs if (value := condition(attr, data)))
        if item.type == "mod_item":
            parts.append("mod:" + text(item.child_by_field_name("name"), data))
        elif item.type == "trait_item":
            parts.append("trait_def:" + text(item.child_by_field_name("name"), data))
        elif item.type == "impl_item" and (trait := item.child_by_field_name("trait")):
            parts.append("trait:" + normalized_syntax(trait, data))
        elif item.type == "function_item" and item != node:
            parts.append("fn:" + text(item.child_by_field_name("name"), data))
    return " / ".join(parts)


def cfg_value(expression, test):
    """Evaluate only `test`; other cfg predicates remain unknown (None)."""
    tokens = iter(re.findall(r'"(?:\\.|[^"\\])*"|[A-Za-z_]\w*|[(),=]', expression))
    remaining = list(tokens)
    position = 0

    def parse():
        nonlocal position
        name = remaining[position]
        position += 1
        if position < len(remaining) and remaining[position] == "=":
            position += 2
            return None
        if position < len(remaining) and remaining[position] == "(":
            position += 1
            children = []
            while remaining[position] != ")":
                children.append(parse())
                if remaining[position] == ",":
                    position += 1
            position += 1
            if name == "all":
                return (
                    False
                    if False in children
                    else (True if all(x is True for x in children) else None)
                )
            if name == "any":
                return (
                    True
                    if True in children
                    else (False if all(x is False for x in children) else None)
                )
            if name == "not" and len(children) == 1:
                return None if children[0] is None else not children[0]
            return None
        return test if name == "test" else None

    try:
        value = parse()
        return value if position == len(remaining) else None
    except IndexError:
        return None


def test_only(node, data, test_attributes):
    attrs, start = attributes(node, data)
    for attr in attrs:
        compact = re.sub(r"\s+", "", attr)
        name = re.match(r"#\[([\w:]+)", compact)
        if name and name[1] in test_attributes:
            return True, start
        cfg = re.fullmatch(r"#\[cfg\((.*)\)\]", compact)
        if cfg and cfg_value(cfg[1], False) is False and cfg_value(cfg[1], True) is not False:
            return True, start
    return False, start


def exclusion_query(query):
    try:
        compiled = tree_sitter.Query(LANGUAGE, query)
    except (tree_sitter.QueryError, ValueError) as error:
        raise AuditError(f"Invalid exclusion query: {error}") from error
    if "exclude" not in [compiled.capture_name(i) for i in range(compiled.capture_count)]:
        raise AuditError("Exclusion query must capture @exclude")
    return compiled


def exclude_nodes(data, rules):
    """Mask explicitly selected syntax, keeping coordinates and a visible audit trail."""
    tree = PARSER.parse(data)
    if tree.root_node.has_error:
        raise AuditError("Cannot apply exclusions to invalid Rust syntax")
    masked = bytearray(data)
    excluded = []
    for rule in rules:
        captures = tree_sitter.QueryCursor(exclusion_query(rule["query"])).captures(tree.root_node)
        for node in sorted(captures.get("exclude", []), key=lambda n: n.start_byte):
            start = node.start_byte
            if node.type.endswith("_item"):
                _, start = attributes(node, data)
            for i in range(start, node.end_byte):
                if data[i] not in (10, 13):
                    masked[i] = 32
            excluded.append({
                "start": node.start_point.row + 1,
                "end": node.end_point.row + 1,
                "kind": node.type,
                "reason": rule["reason"],
                "sha256": sha256(data[start:node.end_byte]),
            })
    if PARSER.parse(bytes(masked)).root_node.has_error:
        raise AuditError("Exclusion produced invalid syntax; capture a complete item or match arm")
    return bytes(masked), excluded


def partition(data, is_test_file, selection, test_attributes):
    original = PARSER.parse(data)
    if original.root_node.has_error:
        errors = [
            n.start_point.row + 1
            for n in walk(original.root_node)
            if n.type == "ERROR" or n.is_missing
        ]
        raise AuditError(f"Tree-sitter could not parse source near lines {sorted(set(errors))[:8]}")
    regions = []
    for node in walk(original.root_node):
        if node.type in ("attribute_item", "line_comment", "block_comment"):
            continue
        only, start = test_only(node, data, test_attributes)
        if only:
            regions.append((start, node.end_byte))
    regions.sort()
    merged = []
    for start, end in regions:
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
        else:
            merged.append((start, end))
    if is_test_file:
        merged = [(0, len(data))]
    test_bytes = bytearray(len(data))
    for start, end in merged:
        test_bytes[start:end] = b"\1" * (end - start)
    masked = bytearray(data)
    if selection != "all":
        for i, value in enumerate(data):
            keep = bool(test_bytes[i]) == (selection == "tests")
            if not keep and value not in (10, 13):
                masked[i] = 32
    parsed = PARSER.parse(bytes(masked))
    if parsed.root_node.has_error:
        raise AuditError("Partitioning produced invalid syntax; inspect test/cfg boundaries")
    # Test partitioning can erase an enclosing impl/module while retaining
    # a cfg(test) method. Identity must come from the unmasked syntax tree.
    original_functions = {
        n.start_byte: n for n in walk(original.root_node) if n.type == "function_item"
    }
    functions = []
    for node in walk(parsed.root_node):
        if node.type != "function_item":
            continue
        original_node = original_functions[node.start_byte]
        parent = original_node.parent
        owner = "<free>"
        while parent:
            if parent.type == "impl_item":
                owner = text(parent.child_by_field_name("type"), data)
                break
            parent = parent.parent
        variant = function_variant(original_node, data)
        fingerprint = data[node.start_byte:node.end_byte]
        if variant:
            fingerprint += b"\0" + variant.encode()
        functions.append(
            {
                "name": text(node.child_by_field_name("name"), masked),
                "owner": owner,
                "start": node.start_point.row + 1,
                "end": node.end_point.row + 1,
                "partition": "tests" if test_bytes[node.start_byte] else "production",
                "variant": variant,
                "sha256": sha256(fingerprint),
            }
        )
    return (
        bytes(masked),
        functions,
        [[data[:start].count(b"\n") + 1, data[:end].count(b"\n") + 1] for start, end in merged],
    )


def region(path):
    parts = Path(path).parts
    if len(parts) > 1 and parts[0] == "crates":
        return "/".join(parts[:2])
    if parts[0] == "src":
        return "/".join(parts[:2]) if len(parts) > 2 else "src"
    return parts[0]


def collect(root, config, paths, selection, input_dir, report):
    root = root.resolve()
    requested = paths or config["roots"]
    scopes = []
    for scope in requested:
        path = root / scope
        try:
            relative = path.resolve().relative_to(root).as_posix()
        except ValueError:
            raise AuditError(f"Scope must be within repository: {scope}") from None
        if not path.exists():
            if paths:
                raise AuditError(f"Scope does not exist: {scope}")
            report["excluded"].append({"file": relative, "reason": "configured root absent"})
            continue
        scopes.append(relative)
    names = (
        git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
        .decode()
        .split("\0")
    )
    for name in sorted(set(names)):
        if not name.endswith(".rs") or not any(
            scope == "." or name == scope or name.startswith(scope + "/") for scope in scopes
        ):
            continue
        path = root / name
        if matches(name, config["exclude"]):
            report["excluded"].append({"file": name, "reason": "exclude pattern"})
            continue
        if not path.exists():
            report["excluded"].append({"file": name, "reason": "deleted from working tree"})
            continue
        if path.is_symlink() or not path.resolve().is_relative_to(root):
            raise AuditError(f"Source symlinks are not supported: {name}")
        is_test = matches(name, config["test_paths"])
        if is_test and selection == "production":
            report["excluded"].append({"file": name, "reason": "test path"})
            continue
        data = path.read_bytes()
        data.decode("utf-8")
        try:
            selected, excluded = exclude_nodes(data, [
                rule for rule in config.get("exclude_nodes", []) if matches(name, rule["files"])
            ])
            report["excluded"].extend({"file": name, **entry} for entry in excluded)
            masked, functions, tests = partition(
                selected, is_test, selection, config["test_attributes"]
            )
        except AuditError as error:
            raise AuditError(f"{name}: {error}") from error
        if not masked.strip():
            report["excluded"].append({"file": name, "reason": "empty selected partition"})
            continue
        target = input_dir / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(masked)
        report["sources"].append(
            {
                "file": name,
                "sha256": sha256(data),
                "input_sha256": sha256(masked),
                "test_regions": tests,
                "region": region(name),
                "functions": len(functions),
            }
        )
        report["expected_functions"].extend(
            {**fn, "file": name, "region": region(name)} for fn in functions
        )
    if not report["sources"]:
        raise AuditError("No Rust source files remain in the selected scopes/partition")
