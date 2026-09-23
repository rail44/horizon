"""Carry review reasons with explicit function identity and checked source evidence."""

from collections import defaultdict
import json
from pathlib import Path

from tooling import AuditError, write_json

IDENTITY = ("file", "owner", "name", "partition")
DECISIONS = ("refactor", "preserve", "investigate")


def identity(function):
    return tuple(function[key] for key in IDENTITY)


def selector(function):
    return {key: function[key] for key in IDENTITY}


def index_functions(report):
    index = defaultdict(list)
    for function in report["functions"]:
        index[identity(function)].append(function)
    return index


def select_function(report, target):
    if (
        not isinstance(target, dict)
        or set(target) != set(IDENTITY)
        or not all(isinstance(value, str) for value in target.values())
    ):
        raise AuditError(f"Function selector requires {', '.join(IDENTITY)}")
    found = index_functions(report).get(identity(target), [])
    if len(found) != 1:
        raise AuditError(f"Expected one function, found {len(found)}: {target}")
    return found[0]


def read_report(path):
    report = json.loads(Path(path).read_text())
    if (
        not isinstance(report, dict)
        or report.get("schema_version") != 1
        or report.get("status") != "complete"
        or report.get("errors")
    ):
        raise AuditError(f"A complete, error-free v1 report is required: {path}")
    return report


def read_reviews(path):
    data = json.loads(Path(path).read_text())
    if (
        not isinstance(data, dict)
        or set(data) != {"schema_version", "reviews"}
        or data["schema_version"] != 1
    ):
        raise AuditError("Unsupported review ledger")
    if not isinstance(data["reviews"], list):
        raise AuditError("reviews must be an array")
    seen = set()
    for review in data["reviews"]:
        if (
            not isinstance(review, dict)
            or set(review) != {"target", "sha256", "related", "decision", "reason", "commit"}
            or not isinstance(review["target"], dict)
            or set(review["target"]) != set(IDENTITY)
            or not all(isinstance(value, str) for value in review["target"].values())
            or review["decision"] not in DECISIONS
            or not isinstance(review["reason"], str)
            or not review["reason"].strip()
            or not isinstance(review["sha256"], str)
            or not isinstance(review["related"], dict)
            or not all(
                isinstance(k, str) and isinstance(v, str) for k, v in review["related"].items()
            )
        ):
            raise AuditError("Invalid review entry")
        key = identity(review["target"])
        if key in seen:
            raise AuditError(f"Duplicate review identity: {key}")
        seen.add(key)
    return data


def record_review(report, target, decision, reason, related, path):
    if decision not in DECISIONS or not reason.strip():
        raise AuditError("A decision and nonempty reason are required")
    function = select_function(report, target)
    if "sha256" not in function:
        raise AuditError("Rescan with function fingerprints before recording a review")
    sources = {row["file"]: row["sha256"] for row in report["sources"]}
    if any(file not in sources for file in related):
        raise AuditError("Related files must be present in the same report")
    data = read_reviews(path) if path.exists() else {"schema_version": 1, "reviews": []}
    data["reviews"] = [r for r in data["reviews"] if identity(r["target"]) != identity(target)]
    data["reviews"].append(
        {
            "target": target,
            "sha256": function["sha256"],
            "related": {file: sources[file] for file in sorted(set(related))},
            "decision": decision,
            "reason": reason,
            "commit": report["commit"],
        }
    )
    data["reviews"].sort(key=lambda r: identity(r["target"]))
    write_json(path, data)


def assess_reviews(report, ledger):
    """Evidence equality carries a reason, never a new semantic approval or suppression."""
    index = index_functions(report)
    sources = {row["file"]: row["sha256"] for row in report["sources"]}
    result = []
    for review in ledger["reviews"]:
        found = index.get(identity(review["target"]), [])
        if not found:
            state = "absent_from_scope"
        elif len(found) > 1:
            state = "ambiguous"
        elif found[0].get("sha256") != review["sha256"]:
            state = "source_changed"
        elif any(sources.get(file) != digest for file, digest in review["related"].items()):
            state = "related_evidence_changed_or_absent"
        else:
            state = "evidence_unchanged"
        result.append({**review, "evidence": state})
    return result
