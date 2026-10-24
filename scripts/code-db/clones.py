"""Load clone pairs from jscpd JSON reports.

Only this subset of the report is read: ``duplicates[]`` with
``firstFile``/``secondFile`` objects holding ``name`` (path), ``start`` and
``end`` (1-based inclusive lines), plus optional ``tokens`` and ``lines``.
"""

import json
from pathlib import Path


def label_for(path, taken):
    path = Path(path)
    label = path.parent.name if path.stem == "jscpd-report" else path.stem
    if label in taken:
        raise ValueError(f"Clone report label {label!r} is ambiguous; pass --clones LABEL=PATH")
    return label


def parse_specs(specs):
    """Turn `PATH` or `LABEL=PATH` arguments into (label, Path) pairs."""
    result = []
    for spec in specs:
        label, sep, path = spec.partition("=")
        if not sep:
            label, path = label_for(spec, {l for l, _ in result}), spec
        elif label in {l for l, _ in result}:
            raise ValueError(f"Duplicate clone report label {label!r}")
        result.append((label, Path(path)))
    return result


def repo_path(name, root, known):
    path = Path(name)
    if path.is_absolute():
        try:
            relative = path.resolve().relative_to(root.resolve()).as_posix()
        except ValueError:
            raise ValueError(f"Clone report path is outside the repository: {name}") from None
    else:
        relative = path.as_posix().removeprefix("./")
    if relative not in known:
        raise ValueError(
            f"Clone report path {name!r} is not a selected repository file; "
            "run jscpd from the repository root or with --absolute"
        )
    return relative


class Groups:
    def __init__(self):
        self.parent = {}

    def find(self, key):
        self.parent.setdefault(key, key)
        while self.parent[key] != key:
            self.parent[key] = self.parent[self.parent[key]]
            key = self.parent[key]
        return key

    def union(self, a, b):
        self.parent[self.find(a)] = self.find(b)


def load(specs, root, known_paths):
    """Return clone_pairs rows. Groups join fragments that overlap within one report."""
    pairs = []
    for label, path in parse_specs(specs):
        for duplicate in json.loads(Path(path).read_text())["duplicates"]:
            sides = []
            for key in ("firstFile", "secondFile"):
                fragment = duplicate[key]
                start, end = int(fragment["start"]), int(fragment["end"])
                if not 1 <= start <= end:
                    raise ValueError(f"{path}: invalid fragment lines {fragment}")
                sides.append((repo_path(fragment["name"], root, known_paths), start - 1, end - 1))
            pairs.append((label, sides, duplicate.get("tokens"), duplicate.get("lines")))
    groups = Groups()
    by_file = {}
    for i, (label, sides, _, _) in enumerate(pairs):
        groups.union((i, 0), (i, 1))
        for s, (file, start, end) in enumerate(sides):
            by_file.setdefault((label, file), []).append((start, end, (i, s)))
    for fragments in by_file.values():
        fragments.sort()
        current_end, current = -1, None
        for start, end, key in fragments:
            if current is not None and start <= current_end:
                groups.union(key, current)
            if end > current_end:
                current_end, current = end, key
    roots = {}
    rows = []
    for i, (label, (a, b), tokens, lines) in enumerate(pairs):
        group = roots.setdefault(groups.find((i, 0)), len(roots))
        rows.append((i, group, label, a[0], a[1], a[2], b[0], b[1], b[2], tokens, lines))
    return rows
