"""Print `git log --raw --numstat -M` file changes as TSV, parsed like the spike.

python3 duckdb-ext/scripts/cli_file_changes.py REPO [extra git log args...]

The parse is the one in scripts/code-db/history.py (`commits_and_changes`);
NULL is printed as \\N, matching the `gix-probe` binary.
"""

import re
import subprocess
import sys

NUMSTAT = re.compile(r"^(\d+|-)\t(\d+|-)\t(.*)$", re.S)
GIT = ["git", "-c", "core.quotePath=false", "-c", "log.showSignature=false", "-c", "diff.noprefix=false",
       "-c", "diff.mnemonicPrefix=false", "-c", "log.diffMerges=off"]


def main():
    root, extra = sys.argv[1], sys.argv[2:]
    out = subprocess.run([*GIT, "log", "HEAD", "--raw", "--numstat", "-M", "--no-abbrev", "-z", "--no-color",
                          "--no-ext-diff", "--no-textconv", "--diff-merges=off", "--format=%x01%H", *extra],
                         cwd=root, check=True, capture_output=True).stdout.decode("utf-8", "replace")
    for record in out.split("\x01")[1:]:
        sha, _, rest = record.partition("\x00")
        sha = sha.strip()
        tokens = iter(rest.split("\x00"))
        stats, entries = {}, []
        for token in tokens:
            token = token.lstrip("\n")
            if not token:
                continue
            if token.startswith(":"):
                status = token[1:].split(" ")[4]
                path = next(tokens)
                old_path = None
                if status[0] in "RC":
                    old_path, path = path, next(tokens)
                entries.append((status[0], old_path, path))
            elif match := NUMSTAT.match(token):
                path = match[3]
                if not path:
                    next(tokens)
                    path = next(tokens)
                stats[path] = tuple(None if v == "-" else int(v) for v in (match[1], match[2]))
            else:
                raise ValueError(f"unexpected token in {sha}: {token[:80]!r}")
        for status, old_path, path in entries:
            added, deleted = stats.get(path, (None, None))
            fields = [sha, path, old_path, status, added, deleted]
            print("\t".join("\\N" if f is None else str(f) for f in fields))


if __name__ == "__main__":
    main()
