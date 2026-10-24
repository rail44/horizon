"""Compare gix-probe's tracked_files rows with the git CLI's.

python3 duckdb-ext/scripts/tracked_parity.py REPO PROBE

PROBE is the built `gix-probe` binary. The git side is `git ls-files -s -z`
(tracked), `git ls-files -o --exclude-standard -z` (untracked),
`git status --porcelain=v2 -z` (worktree modifications) and
`git hash-object` for every content id (a symlink's target path through
`--stdin`, since `git hash-object PATH` follows the link). Prints row counts,
every differing row, and the wall time of both sides.
"""

import os
import subprocess
import sys
import time

GIT = ["git", "-c", "core.quotePath=false"]


def git(repo, *args, stdin=None):
    return subprocess.run([*GIT, *args], cwd=repo, check=True, capture_output=True, input=stdin).stdout


def split_z(data):
    return [part.decode("utf-8", "replace") for part in data.split(b"\0") if part]


def probe(repo, binary, *args):
    start = time.monotonic()
    out = subprocess.run([binary, repo, "tracked_files", *args], check=True, capture_output=True).stdout
    elapsed = time.monotonic() - start
    rows = [tuple(line.split("\t")) for line in out.decode("utf-8", "replace").splitlines() if line]
    return rows, elapsed


def hash_objects(repo, paths):
    """`git hash-object` for each path, files in one batch, symlinks one by one."""
    files = [p for p in paths if not os.path.islink(os.path.join(repo, p))]
    ids = {}
    if files:
        out = git(repo, "hash-object", "--stdin-paths", stdin="\n".join(files).encode())
        ids.update(zip(files, out.decode().split()))
    for p in paths:
        if p not in ids:
            target = os.readlink(os.path.join(repo, p)).encode()
            ids[p] = git(repo, "hash-object", "--stdin", stdin=target).decode().strip()
    return ids


def file_mode(repo, path):
    st = os.lstat(os.path.join(repo, path))
    if os.path.islink(os.path.join(repo, path)):
        return "120000"
    return "100755" if st.st_mode & 0o100 else "100644"


def reference(repo, include_untracked):
    changes = {}
    for record in split_z(git(repo, "status", "--porcelain=v2", "-z", "--untracked-files=no")):
        fields = record.split(" ", 8)
        if fields[0] == "1" and fields[1][1] != ".":
            changes[fields[8]] = (fields[5], fields[1][1] == "D")
        elif fields[0] == "u":
            changes[record.split(" ", 10)[10]] = (fields[6], False)
    entries = {}
    for record in split_z(git(repo, "ls-files", "-s", "-z")):
        meta, path = record.split("\t", 1)
        mode, oid, _stage = meta.split(" ")
        entries.setdefault(path, (mode, oid))
    untracked = []
    if include_untracked:
        untracked = [p for p in split_z(git(repo, "ls-files", "-o", "--exclude-standard", "-z"))
                     if not p.endswith("/")]
    hashed = [p for p, (mode, _) in entries.items()
              if mode != "160000" and not changes.get(p, (None, False))[1]] + untracked
    ids = hash_objects(repo, hashed)
    rows, mismatched = [], []
    for path, (mode, oid) in entries.items():
        change = changes.get(path)
        if change and change[1]:
            continue
        if mode == "160000":
            rows.append((path, mode, oid, "true", "false"))
        elif change:
            rows.append((path, change[0], ids[path], "true", "true"))
        else:
            if ids[path] != oid:
                mismatched.append((path, oid, ids[path]))
            rows.append((path, mode, oid, "true", "false"))
    for path in untracked:
        rows.append((path, file_mode(repo, path), ids[path], "false", "false"))
    rows.sort(key=lambda r: r[0].encode())
    return rows, mismatched


def main():
    repo, binary = sys.argv[1], sys.argv[2]
    for include_untracked in (False, True):
        args = ["--include-untracked"] if include_untracked else []
        ours, ours_time = probe(repo, binary, *args)
        start = time.monotonic()
        git(repo, "ls-files", "-s", "-z")
        if include_untracked:
            git(repo, "ls-files", "-o", "--exclude-standard", "-z")
        listing_time = time.monotonic() - start
        theirs, mismatched = reference(repo, include_untracked)
        only_ours = sorted(set(ours) - set(theirs))
        only_theirs = sorted(set(theirs) - set(ours))
        print(f"include_untracked={include_untracked}: probe {len(ours)} rows in {ours_time:.3f} s, "
              f"git {len(theirs)} rows (ls-files alone {listing_time:.3f} s)")
        print(f"  only in probe: {len(only_ours)}, only in git: {len(only_theirs)}")
        for row in only_ours[:20]:
            print("  probe:", "\t".join(row))
        for row in only_theirs[:20]:
            print("  git:  ", "\t".join(row))
        for path, oid, hashed in mismatched:
            print(f"  unchanged per git status but hash-object differs: {path} {oid} {hashed}")


if __name__ == "__main__":
    main()
