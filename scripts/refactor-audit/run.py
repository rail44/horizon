#!/usr/bin/env python3
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["tree-sitter==0.25.2", "tree-sitter-rust==0.24.2"]
# ///
"""Reproducible refactoring discovery, comparisons, and explicit review records."""

import argparse
import json
from pathlib import Path
import sys
import unittest

from audit import scan
from comparison import write_comparison
from reviews import DECISIONS, read_report, record_review
from tooling import AuditError, ROOT, Toolchain


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser("setup", help="Install checksum-pinned analysis binaries locally")
    setup.add_argument("--history", action="store_true", help="Also download Code Maat")
    analyze = commands.add_parser("scan", help="Analyze working-tree Rust sources")
    analyze.add_argument(
        "paths", nargs="*", help="Repository-relative files/directories; default: configured roots"
    )
    analyze.add_argument("--tests", choices=["production", "tests", "all"], default="production")
    analyze.add_argument(
        "--top",
        type=int,
        default=5,
        help="Clone groups, rule matches and history pairs to show (default: 5)",
    )
    analyze.add_argument("--config", type=Path, default=Path(__file__).with_name("config.json"))
    analyze.add_argument("--reviews", type=Path, help="Carry recorded reasons with evidence freshness")
    analyze.add_argument("--output", type=Path, help="New or empty output directory")
    analyze.add_argument(
        "--history", action="store_true", help="Add file co-change from HEAD ancestry"
    )
    analyze.add_argument(
        "--dependencies", metavar="PACKAGE", help="Add cargo-modules graph for one library package"
    )
    comparison = commands.add_parser("compare", help="Compare compatible reports with explicit split/move mappings")
    comparison.add_argument("before", type=Path)
    comparison.add_argument("after", type=Path)
    comparison.add_argument("--mapping", type=Path)
    comparison.add_argument("--output", type=Path, required=True)
    comparison.add_argument("--top", type=int, default=10)
    record = commands.add_parser("record", help="Record one reviewed responsibility in a JSON ledger")
    record.add_argument("report", type=Path)
    record.add_argument("--file", required=True)
    record.add_argument("--owner", default="<free>")
    record.add_argument("--name", required=True)
    record.add_argument("--partition", choices=["production", "tests"], default="production")
    record.add_argument("--decision", choices=DECISIONS, required=True)
    record.add_argument("--reason", required=True)
    record.add_argument("--related", action="append", default=[], help="Related source path from the same report; repeatable")
    record.add_argument("--output", type=Path, required=True)
    commands.add_parser(
        "verify", help="Run regression fixtures, including the real pinned analyzers"
    )
    args = parser.parse_args()
    try:
        if args.command == "setup":
            tools = Toolchain(ROOT)
            tools.install("rca")
            tools.install("jscpd")
            tools.resolve("ast-grep")
            if args.history:
                tools.install("code-maat")
            print(f"Tools ready: {tools.cache}")
        elif args.command == "compare":
            if args.top < 1:
                raise AuditError("--top must be positive")
            write_comparison(args.before, args.after, args.mapping, args.output, args.top)
            print(f"Comparison: {args.output / 'summary.md'}")
        elif args.command == "record":
            record_review(read_report(args.report), {
                "file": args.file, "owner": args.owner, "name": args.name, "partition": args.partition,
            }, args.decision, args.reason, args.related, args.output)
            print(f"Review recorded: {args.output}")
        elif args.command == "verify":
            suite = unittest.defaultTestLoader.discover(str(Path(__file__).with_name("tests")))
            return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
        else:
            if args.top < 1:
                raise AuditError("--top must be positive")
            config = json.loads(args.config.read_text())
            output, status = scan(
                ROOT,
                config,
                args.paths,
                args.tests,
                args.top,
                args.output,
                history=args.history,
                dependencies=args.dependencies,
                reviews=args.reviews,
            )
            print(f"{status}: {output / 'summary.md'}")
            return 0 if status == "complete" else 1
    except (AuditError, OSError, ValueError) as error:
        print(f"refactor-audit: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
