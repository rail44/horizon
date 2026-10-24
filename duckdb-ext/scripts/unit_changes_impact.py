"""Measure how gix's -U0 hunk placement changes the spike's unit_changes.

Usage: python unit_changes_impact.py REPO GIT_HUNKS_TSV GIX_HUNKS_TSV
Both TSVs hold rows (sha, path, old_start, old_lines, new_start, new_lines).
The spike's own loaders build unit_changes from each hunk set; the two
results are compared as sets of (sha, path, kind, qualified_name, change).
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "scripts" / "code-db"))
import history  # noqa: E402
import languages  # noqa: E402

root = Path(sys.argv[1]).resolve()
adapters = languages.select(["rust"])


def load_hunks(path):
    rows = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            parts = line.rstrip("\n").split("\t")
            if len(parts) != 6 or not parts[1].endswith(".rs"):
                continue
            sha, p, a, b, c, d = parts
            rows.append((sha, p, int(a), int(b), int(c), int(d)))
    return rows


git_hunks = load_hunks(sys.argv[2])
gix_hunks = load_hunks(sys.argv[3])
print(f"hunk rows (*.rs): git {len(git_hunks)}, gix {len(gix_hunks)}")
_, _, raw = history.commits_and_changes(root)
cache = history.UnitCache(root, adapters)
try:
    from_git = set(history.unit_changes(raw, git_hunks, cache))
    from_gix = set(history.unit_changes(raw, gix_hunks, cache))
finally:
    cache.reader.close()
only_git = sorted(from_git - from_gix)
only_gix = sorted(from_gix - from_git)
print(f"unit_changes rows: git {len(from_git)}, gix {len(from_gix)}")
print(f"only from git hunks: {len(only_git)}, only from gix hunks: {len(only_gix)}")
commits = {r[0] for r in only_git} | {r[0] for r in only_gix}
units = {(r[1], r[2], r[3]) for r in only_git} | {(r[1], r[2], r[3]) for r in only_gix}
print(f"commits affected: {len(commits)}, distinct units affected: {len(units)}")
for label, rows in (("only from git hunks", only_git), ("only from gix hunks", only_gix)):
    print(label + ":")
    for row in rows[:12]:
        print("   ", row[0][:10], row[1], row[2], row[3], row[4])
