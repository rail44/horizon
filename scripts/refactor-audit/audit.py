"""Compose pinned analyzers over one immutable source snapshot."""

import csv
from datetime import datetime, timezone
import importlib.metadata
import json
import sys
import uuid

from results import clone_groups, clone_pairs, metrics, source_path, summary
from sources import collect
from tooling import AuditError, HERE, Runner, Toolchain, git, sha256, write_json


def validate_config(config):
    required = {
        "schema_version",
        "roots",
        "exclude",
        "test_paths",
        "test_attributes",
        "clones",
        "rules",
        "history",
    }
    if not isinstance(config, dict) or set(config) != required or config["schema_version"] != 1:
        raise AuditError("Unsupported config keys/schema_version")
    for key in ("roots", "exclude", "test_paths", "test_attributes", "rules"):
        if not isinstance(config[key], list) or not all(isinstance(x, str) for x in config[key]):
            raise AuditError(f"config.{key} must be an array of strings")
    for name in config["rules"]:
        if (
            not (HERE / name).resolve().is_relative_to(HERE / "rules")
            or not (HERE / name).is_file()
        ):
            raise AuditError(f"Rule must be an existing file under rules/: {name}")
    clones = config["clones"]
    if (
        not isinstance(clones, dict)
        or set(clones) != {"min_tokens", "min_lines", "mode"}
        or clones["mode"]
        not in (
            "weak",
            "mild",
            "strict",
        )
    ):
        raise AuditError("Unsupported clone configuration")
    history = config["history"]
    if not isinstance(history, dict) or set(history) != {
        "min_shared",
        "max_commit_files",
        "min_coupling",
    }:
        raise AuditError("Unsupported history configuration")
    for value in [clones["min_tokens"], clones["min_lines"], *history.values()]:
        if type(value) is not int or value < 1:
            raise AuditError("Thresholds must be positive integers")


def scan(
    root,
    config,
    paths,
    selection="production",
    top=5,
    output=None,
    *,
    history=False,
    dependencies=None,
    tools=None,
):
    validate_config(config)
    root = root.resolve()
    commit = git(root, "rev-parse", "HEAD").decode().strip()
    tag = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8]
    output = (output or root / "target/refactor-audit" / (commit[:12] + "-" + tag)).resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise AuditError(f"Output directory must be empty: {output}")
    report = {
        "schema_version": 1,
        "status": "failed",
        "commit": commit,
        "root": str(root),
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "selection": selection,
        "requested_paths": paths,
        "config": config,
        "sources": [],
        "excluded": [],
        "expected_functions": [],
        "functions": [],
        "closures": 0,
        "clones": [],
        "clone_groups": [],
        "clone_statistics": {},
        "rules": [],
        "errors": [],
        "history": None,
        "dependencies": None,
        "limitations": [
            "Non-test cfg/features are not evaluated; macros are not expanded.",
            "Conditional test attributes such as cfg_attr are not evaluated.",
            "External test modules use configured test_paths; module cfg is not propagated across files.",
            "Metrics include nested closures; clone matches may cross function/test-mask boundaries.",
            "File history includes test edits and uses paths without rename tracking.",
        ],
    }
    tools = tools or Toolchain(root)
    runner = Runner(output)
    try:
        report["python"] = sys.version
        report["python_packages"] = {
            name: importlib.metadata.version(name) for name in ("tree-sitter", "tree-sitter-rust")
        }
        report["implementation"] = {
            p.name: sha256(p.read_bytes()) for p in sorted(HERE.glob("*.py"))
        }
        report["tool_versions_sha256"] = sha256((HERE / "tool-versions.json").read_bytes())
        report["rules_sha256"] = {
            name: sha256((HERE / name).read_bytes()) for name in config["rules"]
        }
        report["git_status"] = git(
            root, "status", "--porcelain=v1", "--untracked-files=normal"
        ).decode()
        input_dir = output / "input"
        input_dir.mkdir()
        collect(root, config, paths, selection, input_dir, report)
        files = {s["file"] for s in report["sources"]}
        rca = tools.resolve("rca")
        raw = runner.run("rca", [rca, "-p", input_dir, "-m", "-O", "json", "-j", "1"], root)
        report["functions"], report["closures"] = metrics(
            raw, input_dir, report["expected_functions"], files
        )
        jscpd = tools.resolve("jscpd")
        clone_config = output / "raw/jscpd-config.json"
        write_json(clone_config, {})
        max_size = max((input_dir / file).stat().st_size for file in files) + 1
        max_lines = max(len((input_dir / file).read_bytes().splitlines()) for file in files) + 1
        for mode, extra in [
            ("exact", []),
            ("normalized", ["--ignore-identifiers", "--ignore-literals"]),
        ]:
            destination = output / "raw" / ("jscpd-" + mode)
            destination.mkdir()
            settings = config["clones"]
            runner.run(
                "jscpd-" + mode,
                [
                    jscpd,
                    ".",
                    "--config",
                    clone_config,
                    "--format",
                    "rust",
                    "--min-tokens",
                    str(settings["min_tokens"]),
                    "--min-lines",
                    str(settings["min_lines"]),
                    "--mode",
                    settings["mode"],
                    "--reporters",
                    "json",
                    "--output",
                    destination,
                    "--max-size",
                    str(max_size),
                    "--max-lines",
                    str(max_lines),
                    "--no-gitignore",
                    "--no-colors",
                    "--workers",
                    "1",
                ]
                + extra,
                input_dir,
            )
            data = json.loads((destination / "jscpd-report.json").read_text())
            report["clone_statistics"][mode] = data["statistics"]["total"]
            report["clones"].extend(clone_pairs(data, mode, input_dir, files))
        report["clone_groups"] = clone_groups(report["clones"], input_dir)
        if config["rules"]:
            ast_grep = tools.resolve("ast-grep")
            for i, name in enumerate(config["rules"]):
                raw = runner.run(
                    f"rule-{i}",
                    [
                        ast_grep,
                        "scan",
                        "--rule",
                        HERE / name,
                        "--json=compact",
                        "--no-ignore",
                        "hidden",
                        "--no-ignore",
                        "vcs",
                        ".",
                    ],
                    input_dir,
                )
                for match in json.loads(raw):
                    file = source_path(match["file"], input_dir)
                    if file not in files:
                        raise AuditError(f"ast-grep returned unknown source: {file}")
                    report["rules"].append(
                        {
                            "file": file,
                            "start": match["range"]["start"]["line"] + 1,
                            "end": match["range"]["end"]["line"] + 1,
                            "rule": match["ruleId"],
                            "message": match["message"],
                        }
                    )
            report["rules"].sort(key=lambda r: (r["file"], r["start"], r["rule"]))
        if history:
            jar = tools.resolve("code-maat")
            runner.run(
                "git-history",
                [
                    "git",
                    "log",
                    commit,
                    "--no-merges",
                    "--numstat",
                    "--date=short",
                    "--pretty=format:--%H--%ad--analyst",
                    "--no-renames",
                ],
                root,
            )
            path = output / "raw/git-history.stdout"
            settings = config["history"]
            raw = runner.run(
                "history",
                [
                    "java",
                    "-jar",
                    jar,
                    "-l",
                    path,
                    "-c",
                    "git2",
                    "-a",
                    "coupling",
                    "-n",
                    str(settings["min_shared"]),
                    "-m",
                    str(settings["min_shared"]),
                    "-s",
                    str(settings["max_commit_files"]),
                    "-i",
                    str(settings["min_coupling"]),
                ],
                root,
            )
            current = set(git(root, "ls-files", "-z", "--", "*.rs").decode().split("\0"))
            report["history"] = [
                row
                for row in csv.DictReader(raw.decode().splitlines())
                if row["entity"] in current
                and row["coupled"] in current
                and (row["entity"] in files or row["coupled"] in files)
            ]
        if dependencies:
            cargo_modules = tools.resolve("cargo-modules")
            runner.run(
                "dependencies",
                [
                    cargo_modules,
                    "dependencies",
                    "--lib",
                    "-p",
                    dependencies,
                    "--no-externs",
                    "--no-sysroot",
                ],
                root,
                timeout=600,
            )
            report["dependencies"] = {
                "package": dependencies,
                "target": "host",
                "features": "default",
                "tests": False,
            }
        for source in report["sources"]:
            if sha256((root / source["file"]).read_bytes()) != source["sha256"]:
                raise AuditError(f"Source changed during analysis: {source['file']}")
        if git(root, "rev-parse", "HEAD").decode().strip() != commit:
            raise AuditError("HEAD changed during analysis")
        report["status"] = "complete"
    except (AuditError, OSError, ValueError, KeyError) as error:
        report["errors"].append(f"{type(error).__name__}: {error}")
    finally:
        report["tools"] = tools.used
        report["commands"] = runner.records
        report["finished_utc"] = datetime.now(timezone.utc).isoformat()
        write_json(output / "report.json", report)
        summary(report, output, root, top)
    return output, report["status"]
