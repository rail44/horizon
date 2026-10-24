"""Read commits, file changes and -U0 hunks from git, and map hunks onto units."""

import re
import subprocess

import syntax

NULL_SHA = "0" * 40
HUNK = re.compile(rb"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
NUMSTAT = re.compile(r"^(\d+|-)\t(\d+|-)\t(.*)$", re.S)
# Pin output-affecting configuration so user settings cannot change the parse.
GIT = ["git", "-c", "core.quotePath=false", "-c", "log.showSignature=false", "-c", "diff.noprefix=false",
       "-c", "diff.mnemonicPrefix=false", "-c", "log.diffMerges=off"]


def git(root, *args):
    return subprocess.run([*GIT, *args], cwd=root, check=True, capture_output=True).stdout


def revision_range(since):
    return [f"{since}..HEAD"] if since else ["HEAD"]


def merge_flag(include_merges):
    return "--diff-merges=first-parent" if include_merges else "--diff-merges=off"


def commits_and_changes(root, since=None, include_merges=False):
    """Return (commits, file changes, raw entries) from one `git log --raw --numstat -z` pass.

    Raw entries carry the pre/post blob ids that unit_changes needs.
    """
    out = git(root, "log", *revision_range(since), "--raw", "--numstat", "-M", "--no-abbrev", "-z",
              "--no-color", "--no-ext-diff", "--no-textconv", merge_flag(include_merges),
              "--format=%x01%H%x00%at%x00%an%x00%P%x00%s").decode("utf-8", "replace")
    commits, changes, raw = [], [], []
    for record in out.split("\x01")[1:]:
        sha, author_time, author, parents, rest = record.split("\x00", 4)
        subject, _, rest = rest.partition("\x00")
        parent_count = len(parents.split())
        commits.append((sha, int(author_time), author, subject, parent_count, parent_count > 1))
        tokens = iter(rest.split("\x00"))
        stats = {}
        entries = []
        for token in tokens:
            token = token.lstrip("\n")
            if not token:
                continue
            if token.startswith(":"):
                _, _, old_blob, new_blob, status = token[1:].split(" ")
                path = next(tokens)
                old_path = None
                if status[0] in "RC":
                    old_path, path = path, next(tokens)
                entries.append({"sha": sha, "status": status[0], "old_path": old_path, "path": path,
                                "old_blob": old_blob, "new_blob": new_blob})
            elif match := NUMSTAT.match(token):
                path = match[3]
                if not path:
                    next(tokens)
                    path = next(tokens)
                added, deleted = (None if v == "-" else int(v) for v in (match[1], match[2]))
                stats[path] = (added, deleted)
            else:
                raise ValueError(f"Unexpected git log token in {sha}: {token[:80]!r}")
        for entry in entries:
            added, deleted = stats.get(entry["path"], (None, None))
            changes.append((sha, entry["path"], entry["old_path"], entry["status"], added, deleted))
        raw.extend(entries)
    return commits, changes, raw


def unquote(path):
    """Undo git's C-style quoting of unusual paths."""
    if not path.startswith(b'"'):
        return path.decode("utf-8", "replace")
    body = path[1:-1]
    out = bytearray()
    i = 0
    escapes = {ord("n"): 10, ord("t"): 9, ord('"'): 34, ord("\\"): 92, ord("a"): 7, ord("b"): 8,
               ord("f"): 12, ord("r"): 13, ord("v"): 11}
    while i < len(body):
        c = body[i]
        if c == 92:
            nxt = body[i + 1]
            if 48 <= nxt <= 55:
                out.append(int(body[i + 1 : i + 4], 8))
                i += 4
                continue
            out.append(escapes.get(nxt, nxt))
            i += 2
            continue
        out.append(c)
        i += 1
    return out.decode("utf-8", "replace")


def hunks(root, pathspecs, since=None, include_merges=False):
    """Parse `git log -p -U0` for the given pathspecs into hunk tuples."""
    proc = subprocess.Popen(
        [*GIT, "log", *revision_range(since), "-p", "-U0", "-M", "--no-color", "--no-ext-diff",
         "--no-textconv", merge_flag(include_merges), "--format=%x01%H", "--", *pathspecs],
        cwd=root, stdout=subprocess.PIPE)
    rows = []
    sha = None
    old_path = new_path = None
    remaining = 0  # content lines left in the current hunk
    for line in proc.stdout:
        line = line.rstrip(b"\n")
        if remaining:
            if not line.startswith(b"\\"):
                remaining -= 1
            continue
        if line.startswith(b"\x01"):
            sha = line[1:].decode()
        elif line.startswith(b"diff --git "):
            old_path = new_path = None
        elif line.startswith(b"rename from "):
            old_path = unquote(line[12:])
        elif line.startswith(b"rename to "):
            new_path = unquote(line[10:])
        elif line.startswith(b"--- "):
            value = line[4:]
            old_path = None if value == b"/dev/null" else unquote(value)[2:]
        elif line.startswith(b"+++ "):
            value = line[4:]
            new_path = None if value == b"/dev/null" else unquote(value)[2:]
        elif match := HUNK.match(line):
            old_start, old_lines, new_start, new_lines = (
                int(match[1]), 1 if match[2] is None else int(match[2]),
                int(match[3]), 1 if match[4] is None else int(match[4]))
            # \ "No newline at end of file" lines are not counted.
            remaining = old_lines + new_lines
            rows.append((sha, new_path or old_path, old_start, old_lines, new_start, new_lines))
    if proc.wait():
        raise RuntimeError("git log -p failed")
    return rows


class BlobReader:
    """Stream blob contents through one `git cat-file --batch` process."""

    def __init__(self, root):
        self.proc = subprocess.Popen([*GIT, "cat-file", "--batch"], cwd=root,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE)

    def read(self, blob):
        self.proc.stdin.write(blob.encode() + b"\n")
        self.proc.stdin.flush()
        header = self.proc.stdout.readline().split()
        if len(header) < 3 or header[1] != b"blob":
            raise RuntimeError(f"git cat-file: unexpected {header!r} for {blob}")
        data = self.proc.stdout.read(int(header[2]))
        self.proc.stdout.read(1)
        return data

    def close(self):
        self.proc.stdin.close()
        self.proc.wait()


class UnitCache:
    """Parse each blob once; history repeats the same blobs across commits."""

    def __init__(self, root, adapters):
        self.reader = BlobReader(root)
        self.adapters = adapters
        self.cache = {}
        self.lookups = 0
        self.hits = 0

    def units(self, blob, path):
        self.lookups += 1
        g = syntax.grammar_for(path, self.adapters)
        key = (blob, id(g))
        if key in self.cache:
            self.hits += 1
            return self.cache[key]
        data = self.reader.read(blob)
        tree = g.parse(data)
        result = [(u["kind"], u["qualified_name"], u["start_row"], u["end_row"]) for u in syntax.units(g, tree, data)]
        self.cache[key] = result
        return result


def touched(units, ranges):
    """Units whose 0-based inclusive row range intersects any 1-based (start, count) range."""
    hit = set()
    for kind, name, start_row, end_row in units:
        for start, count in ranges:
            if count == 0:
                continue
            first, last = start - 1, start + count - 2
            if first <= end_row and last >= start_row:
                hit.add((kind, name))
                break
    return hit


def unit_changes(raw, hunk_rows, cache):
    """Map each commit's hunks onto the units of its pre- and post-image."""
    by_file = {}
    for sha, path, old_start, old_lines, new_start, new_lines in hunk_rows:
        by_file.setdefault((sha, path), []).append((old_start, old_lines, new_start, new_lines))
    rows = []
    for entry in raw:
        path = entry["path"]
        if syntax.grammar_for(path, cache.adapters) is None:
            continue
        ranges = by_file.get((entry["sha"], path))
        if not ranges:
            continue  # pure rename, mode change, or binary
        old_path = entry["old_path"] or path
        has_old = entry["old_blob"] != NULL_SHA and syntax.grammar_for(old_path, cache.adapters)
        has_new = entry["new_blob"] != NULL_SHA
        old_units = cache.units(entry["old_blob"], old_path) if has_old else []
        new_units = cache.units(entry["new_blob"], path) if has_new else []
        old_ids = {(k, n) for k, n, _, _ in old_units}
        new_ids = {(k, n) for k, n, _, _ in new_units}
        after = touched(new_units, [(c, d) for _, _, c, d in ranges])
        before = touched(old_units, [(a, b) for a, b, _, _ in ranges])
        changed = {}
        for unit in after:
            changed[unit] = "modified" if unit in old_ids else "added"
        for unit in before:
            if unit not in changed:
                changed[unit] = "modified" if unit in new_ids else "removed"
        for (kind, name), change in sorted(changed.items()):
            rows.append((entry["sha"], old_path if change == "removed" else path, kind, name, change))
    return rows
