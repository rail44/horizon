"""Compare git_history_core's rows (through the gix-probe binary) with the git CLI's.

python3 duckdb-ext/scripts/gix_parity.py REPO PROBE OUT_DIR [SECTION...]

PROBE is the built `gix-probe` binary; OUT_DIR receives every TSV it compares.
SECTION is any of hunks, context, file_changes, tail, commits, merges, timing
(default all; tail reads the TSVs of hunks and file_changes).
Rows are compared as multisets; the git side is parsed by cli_hunks.py and
cli_file_changes.py, and commit order is `git log --format=%H`.
"""

import collections
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
GIT = ["git", "-c", "core.quotePath=false", "-c", "log.showSignature=false", "-c", "diff.noprefix=false",
       "-c", "diff.mnemonicPrefix=false", "-c", "log.diffMerges=off"]


def rows(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return [tuple(line.rstrip("\n").split("\t")) for line in f if line.strip()]


def run_to(path, args, cwd=None):
    with open(path, "wb") as f:
        subprocess.run(args, cwd=cwd, check=True, stdout=f)
    return rows(path)


def probe(ctx, name, table, *args):
    return run_to(os.path.join(ctx["out"], f"gix_{name}.tsv"), [ctx["probe"], ctx["repo"], table, *args])


def git_script(ctx, name, script, *args):
    return run_to(os.path.join(ctx["out"], f"git_{name}.tsv"),
                  [sys.executable, os.path.join(HERE, script), ctx["repo"], *args])


def multiset_diff(ours, theirs):
    a, b = collections.Counter(ours), collections.Counter(theirs)
    return a - b, b - a


def by_file(counter):
    groups = collections.defaultdict(list)
    for row, n in counter.items():
        groups[row[:2]].extend([tuple(int(v) for v in row[2:])] * n)
    return groups


def classify(git_only, gix_only):
    """Group differing hunk rows by (sha, path) and name the shape of each group."""
    g, x = by_file(git_only), by_file(gix_only)
    shapes = collections.Counter()
    rows_by_shape = collections.Counter()
    examples = collections.defaultdict(list)
    for key in sorted(set(g) | set(x)):
        a, b = sorted(g.get(key, [])), sorted(x.get(key, []))
        sums = lambda hs: (sum(h[1] for h in hs), sum(h[3] for h in hs))
        if a and b and len(a) == len(b) and all(
                (p[1], p[3]) == (q[1], q[3]) and p[0] - q[0] == p[2] - q[2] for p, q in zip(a, b)):
            shape = "shifted"  # same sizes, both sides moved by the same offset
        elif a and b and sums(a) == sums(b):
            shape = "split-or-merged"  # same line totals, different hunk boundaries
        elif not b:
            shape = "git-only-file"
        elif not a:
            shape = "gix-only-file"
        else:
            shape = "different-lines"
        shapes[shape] += 1
        rows_by_shape[shape] += len(a) + len(b)
        examples[shape].append((key, a, b))
    return shapes, rows_by_shape, examples


def header(h):
    return f"@@ -{h[0]},{h[1]} +{h[2]},{h[3]} @@"


def report_hunks(label, git, gix, n_examples=3):
    git_only, gix_only = multiset_diff(git, gix)
    print(f"{label}: git {len(git)} rows, gix {len(gix)} rows, "
          f"only in git {sum(git_only.values())}, only in gix {sum(gix_only.values())}")
    shapes, rows_by_shape, examples = classify(git_only, gix_only)
    for shape in sorted(shapes):
        print(f"  {shape}: {shapes[shape]} files, {rows_by_shape[shape]} differing rows (both sides)")
        for (sha, path), a, b in examples[shape][:n_examples]:
            print(f"    {sha[:10]} {path}")
            print(f"      git: {' '.join(header(h) for h in a) or '-'}")
            print(f"      gix: {' '.join(header(h) for h in b) or '-'}")
    return git_only, gix_only, examples


def cli_hunks_with_context(ctx, name, context, *extra):
    """`git log -p -U<context>` hunk headers, counting body lines per side."""
    out_path = os.path.join(ctx["out"], f"git_{name}.tsv")
    proc = subprocess.Popen([*GIT, "log", "HEAD", "-p", f"-U{context}", "-M", "--no-color", "--no-ext-diff",
                             "--no-textconv", "--diff-merges=off", "--format=%x01%H", *extra],
                            cwd=ctx["repo"], stdout=subprocess.PIPE)
    sha = old_path = new_path = None
    old_left = new_left = 0
    with open(out_path, "wb") as out:
        for line in proc.stdout:
            line = line.rstrip(b"\n")
            if old_left or new_left:
                if line.startswith(b"\\"):
                    continue
                kind = line[:1]
                if kind in (b" ", b""):
                    old_left -= 1
                    new_left -= 1
                elif kind == b"-":
                    old_left -= 1
                elif kind == b"+":
                    new_left -= 1
                continue
            if line.startswith(b"\x01"):
                sha = line[1:]
            elif line.startswith(b"diff --git "):
                old_path = new_path = None
            elif line.startswith(b"--- "):
                old_path = None if line[4:] == b"/dev/null" else line[6:]
            elif line.startswith(b"+++ "):
                new_path = None if line[4:] == b"/dev/null" else line[6:]
            elif line.startswith(b"@@ -"):
                spec = line[4:].split(b" @@")[0]
                old, new = spec.split(b" +")
                a, _, b = old.partition(b",")
                c, _, d = new.partition(b",")
                a, c = int(a), int(c)
                b, d = (int(b) if b else 1), (int(d) if d else 1)
                old_left, new_left = b, d
                out.write(b"\t".join([sha, new_path or old_path, *(str(v).encode() for v in (a, b, c, d))]) + b"\n")
    assert proc.wait() == 0
    return rows(out_path)


def section_hunks(ctx):
    git = git_script(ctx, "hunks", "cli_hunks.py")
    gix = probe(ctx, "hunks", "hunks")
    report_hunks("hunks", git, gix)


def section_context(ctx):
    for context in (1, 3):
        git = cli_hunks_with_context(ctx, f"hunks_u{context}", context)
        gix = probe(ctx, f"hunks_u{context}", "hunks", "--context", str(context))
        report_hunks(f"hunks -U{context}", git, gix)
        print()


def section_file_changes(ctx, suffix="", *extra):
    git = git_script(ctx, f"file_changes{suffix}", "cli_file_changes.py", *extra)
    merges = ["--include-merges"] if extra else []
    gix = probe(ctx, f"file_changes{suffix}", "file_changes", *merges)
    report_file_changes(f"file_changes{suffix}", git, gix)


def report_file_changes(label, git, gix):
    git_only, gix_only = multiset_diff(git, gix)
    print(f"{label}: git {len(git)} rows, gix {len(gix)} rows, "
          f"only in git {sum(git_only.values())}, only in gix {sum(gix_only.values())}")
    g = {(r[0], r[1]): r for r in git_only.elements()}
    x = {(r[0], r[1]): r for r in gix_only.elements()}
    pairing, status, binary, counts, other = [], [], [], [], []
    for key in sorted(set(g) | set(x)):
        a, b = g.get(key), x.get(key)
        if a and b:
            if a[2] != b[2]:
                pairing.append((a, b))
            elif a[3] != b[3]:
                status.append((a, b))
            elif ("\\N" in a[4:]) != ("\\N" in b[4:]):
                binary.append((a, b))
            elif a[4:] != b[4:]:
                counts.append((a, b))
            else:
                other.append((a, b))
        else:
            other.append((a, b))
    rename_commits = sorted({r[0] for pair in other for r in pair if r and (r[3] == "R" or r[2] != "\\N")}
                            | {a[0] for a, _ in pairing})
    for name, items in [("old_path differs", pairing), ("status differs", status),
                        ("binary (NULL) differs", binary), ("numstat differs", counts),
                        ("row only on one side", other)]:
        print(f"  {name}: {len(items)}")
        for a, b in items[:12]:
            print(f"    git: {a}")
            print(f"    gix: {b}")
    print(f"  commits with differing rename rows: {len(rename_commits)} {[c[:10] for c in rename_commits]}")
    nulls = lambda rs: sum(1 for r in rs if r[4] == "\\N")
    print(f"  NULL-count rows: git {nulls(git)}, gix {nulls(gix)}")


def section_commits(ctx):
    order = subprocess.run(["git", "log", "--format=%H", "HEAD"], cwd=ctx["repo"], check=True,
                           capture_output=True, text=True).stdout.split()
    n = len(order)
    sinces = [order[round(k * n / 10)] for k in range(1, 10)]
    print(f"commits: {n} in git log; since values: {[s[:10] for s in sinces]}")
    for since in [None, *sinces]:
        rev = f"{since}..HEAD" if since else "HEAD"
        want = subprocess.run(["git", "log", "--format=%H", rev], cwd=ctx["repo"], check=True,
                              capture_output=True, text=True).stdout.split()
        args = ["--since", since] if since else []
        got = [r[0] for r in probe(ctx, f"commits_{(since or 'HEAD')[:10]}", "commits", *args)]
        if got == want:
            verdict = "equal"
        else:
            moved = sum(1 for a, b in zip(got, want) if a != b) + abs(len(got) - len(want))
            same_set = collections.Counter(got) == collections.Counter(want)
            verdict = f"{moved} positions differ{'' if same_set else ' (different set)'}"
        print(f"  {(since or 'HEAD')[:10]} ({len(want)}): {verdict}")
    git_rows = run_to(os.path.join(ctx["out"], "git_commits.tsv"),
                      ["git", "log", "--format=%H%x09%at%x09%an%x09%s%x09%P", "HEAD"], cwd=ctx["repo"])
    git_rows = [(h, t, a, s, str(len(p.split())), str(len(p.split()) > 1).lower()) for h, t, a, s, p in
                [r + ("",) * (5 - len(r)) for r in git_rows]]
    gix_rows = probe(ctx, "commits_fields", "commits")
    only_git, only_gix = multiset_diff(git_rows, gix_rows)
    print(f"  commit fields (multiset): only in git {sum(only_git.values())}, only in gix {sum(only_gix.values())}")
    for a in list(only_git)[:5]:
        print(f"    git: {a}")
    for b in list(only_gix)[:5]:
        print(f"    gix: {b}")


def blob(ctx, rev):
    out = subprocess.run(["git", "cat-file", "blob", rev], cwd=ctx["repo"], capture_output=True)
    return out.stdout if out.returncode == 0 else b""


def common_tail(a, b):
    n = 0
    while n < min(len(a), len(b)) and a[len(a) - 1 - n] == b[len(b) - 1 - n]:
        n += 1
    return n


def section_tail(ctx, git_name="hunks", gix_name="hunks", changes="file_changes"):
    """For every file whose -U0 hunks differ, measure the byte length of the common tail of its two sides."""
    git = rows(os.path.join(ctx["out"], f"git_{git_name}.tsv"))
    gix = rows(os.path.join(ctx["out"], f"gix_{gix_name}.tsv"))
    renames = {(r[0], r[1]): r[2] for r in rows(os.path.join(ctx["out"], f"git_{changes}.tsv")) if r[2] != "\\N"}
    git_only, gix_only = multiset_diff(git, gix)
    files = sorted({r[:2] for r in git_only} | {r[:2] for r in gix_only})
    buckets = collections.Counter()
    for sha, path in files:
        old = blob(ctx, f"{sha}^:{renames.get((sha, path), path)}")
        new = blob(ctx, f"{sha}:{path}")
        tail = common_tail(old, new)
        buckets["common tail >= 1024 bytes" if tail >= 1024 else "common tail < 1024 bytes"] += 1
        if tail < 1024:
            print(f"  short tail: {sha[:10]} {path} tail={tail} old={len(old)} new={len(new)}")
    print(f"tail [{gix_name}]: {len(files)} files with differing -U0 hunks: {dict(buckets)}")


def section_merges(ctx):
    git = git_script(ctx, "hunks_merges", "cli_hunks.py", "--diff-merges=first-parent")
    gix = probe(ctx, "hunks_merges", "hunks", "--include-merges")
    report_hunks("hunks include_merges", git, gix, n_examples=1)
    print()
    section_file_changes(ctx, "_merges", "--diff-merges=first-parent")
    section_tail(ctx, "hunks_merges", "hunks_merges", "file_changes_merges")
    git = cli_hunks_with_context(ctx, "hunks_merges_u1", 1, "--diff-merges=first-parent")
    gix = probe(ctx, "hunks_merges_u1", "hunks", "--include-merges", "--context", "1")
    report_hunks("hunks include_merges -U1", git, gix)


def timed(args, cwd=None):
    """Wall time and peak RSS (MiB) of one child process, its output discarded."""
    start = time.perf_counter()
    pid = subprocess.Popen(args, cwd=cwd, stdout=subprocess.DEVNULL).pid
    _, status, usage = os.wait4(pid, 0)
    assert status == 0, args
    return time.perf_counter() - start, usage.ru_maxrss / 1024


def section_timing(ctx, runs=3):
    """Fresh processes: the probe, the git CLI alone, the git CLI with the TSV parse."""
    git_hunks = [*GIT, "log", "HEAD", "-p", "-U0", "-M", "--no-color", "--no-ext-diff", "--no-textconv",
                 "--diff-merges=off", "--format=%x01%H"]
    git_changes = [*GIT, "log", "HEAD", "--raw", "--numstat", "-M", "--no-abbrev", "-z", "--no-color",
                   "--no-ext-diff", "--no-textconv", "--diff-merges=off", "--format=%x01%H"]
    cases = [
        ("gix-probe hunks", [ctx["probe"], ctx["repo"], "hunks"], None),
        ("gix-probe file_changes", [ctx["probe"], ctx["repo"], "file_changes"], None),
        ("gix-probe commits", [ctx["probe"], ctx["repo"], "commits"], None),
        ("git log -p -U0 -M", git_hunks, ctx["repo"]),
        ("git log --raw --numstat -M", git_changes, ctx["repo"]),
        ("git log --format=%H", ["git", "log", "--format=%H", "HEAD"], ctx["repo"]),
        ("cli_hunks.py", [sys.executable, os.path.join(HERE, "cli_hunks.py"), ctx["repo"]], None),
        ("cli_file_changes.py", [sys.executable, os.path.join(HERE, "cli_file_changes.py"), ctx["repo"]], None),
    ]
    for name, args, cwd in cases:
        results = [timed(args, cwd) for _ in range(runs)]
        print(f"{name}: " + ", ".join(f"{t:.2f} s" for t, _ in results)
              + f"; peak RSS {max(m for _, m in results):.0f} MiB")


def main():
    repo, probe_bin, out = sys.argv[1:4]
    sections = sys.argv[4:] or ["hunks", "context", "file_changes", "tail", "commits", "merges", "timing"]
    os.makedirs(out, exist_ok=True)
    ctx = {"repo": repo, "probe": os.path.abspath(probe_bin), "out": out}
    for section in sections:
        print(f"## {section}")
        globals()[f"section_{section}"](ctx)
        print()


if __name__ == "__main__":
    main()
