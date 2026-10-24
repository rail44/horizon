"""Print `git log -p -U0 -M` hunk headers for all files as TSV.

python3 duckdb-ext/scripts/cli_hunks.py REPO [extra git log args...]

Only the `@@` headers are read (content lines are skipped), with no
pathspec, so the output is what git_hunks is meant to reproduce.
"""

import re
import subprocess
import sys

HUNK = re.compile(rb"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
GIT = ["git", "-c", "core.quotePath=false", "-c", "log.showSignature=false", "-c", "diff.noprefix=false",
       "-c", "diff.mnemonicPrefix=false", "-c", "log.diffMerges=off"]


def main():
    root, extra = sys.argv[1], sys.argv[2:]
    proc = subprocess.Popen([*GIT, "log", "HEAD", "-p", "-U0", "-M", "--no-color", "--no-ext-diff", "--no-textconv",
                             "--diff-merges=off", "--format=%x01%H", *extra], cwd=root, stdout=subprocess.PIPE)
    out = sys.stdout.buffer
    sha = old_path = new_path = None
    remaining = 0
    for line in proc.stdout:
        line = line.rstrip(b"\n")
        if remaining:
            if not line.startswith(b"\\"):
                remaining -= 1
            continue
        if line.startswith(b"\x01"):
            sha = line[1:]
        elif line.startswith(b"diff --git "):
            old_path = new_path = None
        elif line.startswith(b"--- "):
            old_path = None if line[4:] == b"/dev/null" else line[6:]
        elif line.startswith(b"+++ "):
            new_path = None if line[4:] == b"/dev/null" else line[6:]
        elif match := HUNK.match(line):
            a, b, c, d = (int(match[1]), 1 if match[2] is None else int(match[2]),
                          int(match[3]), 1 if match[4] is None else int(match[4]))
            remaining = b + d
            out.write(b"\t".join([sha, new_path or old_path, *(str(v).encode() for v in (a, b, c, d))]) + b"\n")
    sys.exit(proc.wait())


if __name__ == "__main__":
    main()
