"""Normalize tool output and render a bounded review entry point."""

from collections import defaultdict
import json
from pathlib import Path

from tooling import AuditError


def source_path(name, input_dir):
    path = Path(name)
    try:
        return (
            path.resolve().relative_to(input_dir.resolve()).as_posix()
            if path.is_absolute()
            else path.as_posix().removeprefix("./")
        )
    except ValueError:
        raise AuditError(f"Tool returned a path outside the analyzed input: {name}") from None


def spaces(node):
    yield node
    for child in node.get("spaces", []):
        yield from spaces(child)


def metrics(raw, input_dir, expected, files):
    indexed = {(f["file"], f["start"], f["end"], f["name"]): f for f in expected}
    found = set()
    file_names = set()
    rows = []
    closures = 0
    for unit in [json.loads(line) for line in raw.splitlines() if line.strip()]:
        file = source_path(unit["name"], input_dir)
        file_names.add(file)
        for node in spaces(unit):
            if node["kind"] != "function":
                continue
            if node["name"] == "<anonymous>":
                closures += 1
                continue
            key = (file, node["start_line"], node["end_line"], node["name"])
            if key not in indexed:
                raise AuditError(f"RCA and Tree-sitter disagree on function boundary: {key}")
            if key in found:
                raise AuditError(f"RCA returned duplicate function: {key}")
            found.add(key)
            values = node["metrics"]
            rows.append(
                {
                    **indexed[key],
                    "physical_code_lines": values["loc"]["ploc"],
                    "cyclomatic": values["cyclomatic"]["sum"],
                    "cognitive": values["cognitive"]["sum"],
                    "nested_spaces": len(node.get("spaces", [])),
                }
            )
    if file_names != set(files):
        raise AuditError(
            f"RCA file coverage mismatch: missing={sorted(set(files) - file_names)}, extra={sorted(file_names - set(files))}"
        )
    if found != set(indexed):
        raise AuditError(f"RCA omitted functions: {sorted(set(indexed) - found)[:10]}")
    return sorted(rows, key=lambda row: (row["file"], row["start"], row["end"])), closures


def clone_pairs(raw, mode, input_dir, files):
    rows = []
    for pair in raw["duplicates"]:
        fragments = []
        for key in ("firstFile", "secondFile"):
            fragment = pair[key]
            file = source_path(fragment["name"], input_dir)
            if file not in files:
                raise AuditError(f"jscpd returned an unknown source: {file}")
            count = len((input_dir / file).read_bytes().splitlines())
            if not 1 <= fragment["start"] <= fragment["end"] <= count:
                raise AuditError(f"jscpd returned invalid coordinates: {fragment}")
            fragments.append({"file": file, "start": fragment["start"], "end": fragment["end"]})
        rows.append(
            {
                "mode": mode,
                "fragments": sorted(fragments, key=lambda f: (f["file"], f["start"], f["end"])),
            }
        )
    return sorted(
        rows, key=lambda r: tuple((f["file"], f["start"], f["end"]) for f in r["fragments"])
    )


def clone_groups(pairs, input_dir):
    parents = list(range(len(pairs)))
    lines = {}
    cache = {}

    def find(index):
        while parents[index] != index:
            parents[index] = parents[parents[index]]
            index = parents[index]
        return index

    for i, pair in enumerate(pairs):
        for fragment in pair["fragments"]:
            file = fragment["file"]
            if file not in cache:
                cache[file] = (input_dir / file).read_text().splitlines()
            for line in range(fragment["start"], fragment["end"] + 1):
                if not cache[file][line - 1].strip():
                    continue
                key = (file, line)
                if key in lines:
                    parents[find(i)] = find(lines[key])
                else:
                    lines[key] = i
    grouped = defaultdict(list)
    counts = defaultdict(int)
    for i in range(len(pairs)):
        grouped[find(i)].append(i)
    for i in lines.values():
        counts[find(i)] += 1
    result = []
    for root, ids in grouped.items():
        fragments = {tuple(sorted(f.items())) for i in ids for f in pairs[i]["fragments"]}
        result.append(
            {
                "pairs": ids,
                "nonblank_lines": counts[root],
                "modes": sorted({pairs[i]["mode"] for i in ids}),
                "fragments": sorted(
                    [dict(f) for f in fragments], key=lambda f: (f["file"], f["start"], f["end"])
                ),
            }
        )
    return sorted(result, key=lambda r: (-r["nonblank_lines"], r["pairs"]))


def summary(report, output, root, top):
    lines = [
        "# Refactoring audit",
        "",
        f"Status: **{report['status']}**. Commit: `{report['commit']}`.",
        f"Partition: `{report['selection']}`. {len(report['sources'])} files; {len(report['functions'])} named functions.",
        "",
        "These are review entry points, not confirmed defects. Full coordinates, configuration, hashes, exclusions and commands: [report.json](report.json).",
        "",
    ]
    if report["errors"]:
        lines.extend(
            ["## Analysis errors", ""]
            + [f"- {error}" for error in report["errors"]]
            + ["", "Partial results must not be interpreted as a clean scan.", ""]
        )

    def location(row, label=None):
        from urllib.parse import quote

        file = row["file"]
        return f"[{label or file + ':' + str(row['start'])}]({quote(str(root / file), safe='/')}:{row['start']})"

    grouped = defaultdict(list)
    for fn in report["functions"]:
        grouped[(fn["region"], fn["partition"])].append(fn)
    lines += [
        "## Function overview",
        "",
        "Highest value per region/partition; all functions are in report.json. Scores include nested closures. These columns are independent signals, not a combined score.",
        "",
        "| Region / partition | Functions | Physical code lines | Cyclomatic | Cognitive |",
        "| --- | ---: | --- | --- | --- |",
    ]
    for (region, partition), rows in sorted(grouped.items()):
        cells = []
        for metric in ("physical_code_lines", "cyclomatic", "cognitive"):
            row = min(rows, key=lambda r: (-r[metric], r["file"], r["start"]))
            cells.append(f"{row[metric]:g} {location(row, row['name'])}")
        lines.append(f"| {region} / {partition} | {len(rows)} | {' | '.join(cells)} |")
    lines.append("")
    lines += [
        "## Clone groups",
        "",
        f"{len(report['clones'])} pairs across both modes; {len(report['clone_groups'])} overlapping groups. Showing {top} groups.",
        "Grouping joins reports sharing a nonblank input line; comments count. A group is not a responsibility or a recommended extraction.",
        "",
    ]
    if report["clone_statistics"]:
        counts = ", ".join(
            f"{mode}: {stats['sources']}" for mode, stats in report["clone_statistics"].items()
        )
        lines += [
            f"Files meeting jscpd's token minimum ({report['config']['clones']['min_tokens']}): {counts}. Smaller files cannot produce a match at this threshold.",
            "",
        ]
    for group in report["clone_groups"][:top]:
        locations = ", ".join(location(f) for f in group["fragments"][:6])
        extra = len(group["fragments"]) - 6
        if extra > 0:
            locations += f", and {extra} more ranges"
        lines.append(
            f"- {group['nonblank_lines']} nonblank lines; {', '.join(group['modes'])}: {locations}"
        )
    lines += [
        "",
        "## Convention checks",
        "",
        f"{len(report['rules'])} matches; showing up to {top}.",
        "",
    ]
    for row in report["rules"][:top]:
        lines.append(f"- `{row['rule']}`: {location(row)} — {row['message']}")
    if not report["rules"]:
        checked = sum(
            c["name"].startswith("rule-") and c["exit_code"] == 0 for c in report["commands"]
        )
        lines.append(
            f"{checked}/{len(report['config']['rules'])} configured rules completed. No matches were reported; this does not establish responsibility or convention correctness."
        )
    if report.get("history") is not None:
        lines += [
            "",
            "## Optional history",
            "",
            f"{len(report['history'])} pairs; showing up to {top}. File history includes test edits and legitimate feature-wide changes.",
            "",
        ]
        for row in report["history"][:top]:
            lines.append(
                f"- `{row['entity']}` ↔ `{row['coupled']}`: degree {row['degree']}, average revisions {row['average-revs']}"
            )
    if report.get("dependencies"):
        lines += [
            "",
            "Dependency graph: [DOT](raw/dependencies.stdout). This describes the selected package/library and feature configuration.",
        ]
    lines += [
        "",
        "Next: compare contracts and state ownership; inspect a sample outside these rankings too. Record location, evidence, required differences, and judgment using docs/refactoring-review.md.",
        "",
    ]
    (output / "summary.md").write_text("\n".join(lines))
