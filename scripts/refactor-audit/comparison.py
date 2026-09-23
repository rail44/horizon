"""Compare compatible scans; require explicit correspondence for moved or split functions."""

import json

from reviews import identity, index_functions, read_report, select_function
from tooling import AuditError, write_json

METRICS = ("physical_code_lines", "cyclomatic", "cognitive")


def compatible(before, after):
    for key in (
        "selection", "requested_paths", "python_packages", "tool_versions_sha256",
        "rules_sha256", "implementation",
    ):
        if before.get(key) != after.get(key):
            raise AuditError(f"Cannot compare scans with different {key}; rescan with the same settings")
    configs = [
        {**r["config"], "exclude_nodes": r["config"].get("exclude_nodes", [])}
        for r in (before, after)
    ]
    if configs[0] != configs[1]:
        raise AuditError("Cannot compare different configurations; rescan the baseline")
    # Ignore executable paths, which legitimately differ between worktrees.
    for name in ("rca", "jscpd", "ast-grep"):
        versions = [
            {k: v for k, v in r["tools"].get(name, {}).items() if k != "path"}
            for r in (before, after)
        ]
        if versions[0] != versions[1]:
            raise AuditError(f"Cannot compare different {name} executables")


def measure(functions):
    return {
        "functions": len(functions),
        **{key: max((row[key] for row in functions), default=0) for key in METRICS},
    }


def compare(before, after, mappings=()):
    compatible(before, after)
    matched_before, matched_after = set(), set()
    groups = []
    for mapping in mappings:
        if (
            not isinstance(mapping, dict)
            or set(mapping) != {"label", "before", "after"}
            or not isinstance(mapping["label"], str)
            or not mapping["label"].strip()
            or not isinstance(mapping["before"], list)
            or not mapping["before"]
            or not isinstance(mapping["after"], list)
            or not mapping["after"]
        ):
            raise AuditError("Each correspondence needs a label and nonempty before/after selectors")
        sides = []
        for report, side, used in ((before, "before", matched_before), (after, "after", matched_after)):
            functions = [select_function(report, target) for target in mapping[side]]
            for row in functions:
                key = identity(row)
                if key in used:
                    raise AuditError(f"Function reused in correspondence: {key}")
                used.add(key)
            sides.append(functions)
        groups.append({
            "label": mapping["label"], "before": sides[0], "after": sides[1],
            "max_before": measure(sides[0]), "max_after": measure(sides[1]),
        })
    old, new = index_functions(before), index_functions(after)
    changed, added, removed, ambiguous = [], [], [], []
    for key in sorted(old.keys() | new.keys()):
        left = [] if key in matched_before else old.get(key, [])
        right = [] if key in matched_after else new.get(key, [])
        if len(left) > 1 or len(right) > 1:
            ambiguous.append({"identity": key, "before": left, "after": right})
        elif left and right:
            delta = {metric: right[0][metric] - left[0][metric] for metric in METRICS}
            if any(delta.values()) or left[0].get("sha256") != right[0].get("sha256"):
                changed.append({"before": left[0], "after": right[0], "delta": delta})
        elif left:
            removed.extend(left)
        elif right:
            added.extend(right)
    return {
        "schema_version": 1,
        "before_commit": before["commit"], "after_commit": after["commit"],
        "correspondences": groups, "changed": changed, "added": added, "removed": removed,
        "ambiguous": ambiguous,
        "counts": {
            side: {
                "files": len(report["sources"]), "functions": len(report["functions"]),
                "clone_pairs": len(report["clones"]), "clone_groups": len(report["clone_groups"]),
            }
            for side, report in (("before", before), ("after", after))
        },
    }


def write_comparison(before_path, after_path, mapping_path, output, top):
    mappings = json.loads(mapping_path.read_text()) if mapping_path else []
    if not isinstance(mappings, list):
        raise AuditError("Correspondences must be an array")
    before, after = read_report(before_path), read_report(after_path)
    result = compare(before, after, mappings)
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise AuditError(f"Output directory must be empty: {output}")
    write_json(output / "comparison.json", result)
    lines = ["# Refactoring comparison", "", f"`{before['commit']}` → `{after['commit']}`", "",
             "Metrics include closures. Added/removed functions are not automatic improvements.",
             "Explicit correspondences report maxima across all selected functions, not only the new entry point.", "",
             "| Scope | Before | After |", "| --- | ---: | ---: |"]
    for key in result["counts"]["before"]:
        lines.append(f"| {key} | {result['counts']['before'][key]} | {result['counts']['after'][key]} |")
    lines += ["", "## Explicit correspondences", "", "| Responsibility | Functions before/after | Cognitive max before/after |", "| --- | --- | --- |"]
    for group in result["correspondences"]:
        left, right = group["max_before"], group["max_after"]
        label = group["label"].replace("|", "\\|").replace("\n", " ")
        lines.append(f"| {label} | {left['functions']} / {right['functions']} | {left['cognitive']:g} / {right['cognitive']:g} |")
    lines += ["", "## Changed functions", ""]
    for row in sorted(result["changed"], key=lambda r: -abs(r["delta"]["cognitive"]))[:top]:
        fn = row["after"]
        lines.append(f"- `{fn['file']}:{fn['start']}` `{fn['owner']}::{fn['name']}`: cognitive {row['before']['cognitive']:g} → {fn['cognitive']:g}")
    lines += ["", f"Added: {len(result['added'])}; removed: {len(result['removed'])}; ambiguous identities: {len(result['ambiguous'])}.",
              "Full coordinates and unmatched functions: [comparison.json](comparison.json).", ""]
    (output / "summary.md").write_text("\n".join(lines))
    return result
