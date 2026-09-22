#!/usr/bin/env python3
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = ["tree-sitter==0.25.2", "tree-sitter-rust==0.24.2"]
# ///
"""Reproducible, read-only refactoring discovery for this checkout."""

import argparse
import json
from pathlib import Path
import sys
import unittest

from audit import scan
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
    analyze.add_argument("--output", type=Path, help="New or empty output directory")
    analyze.add_argument(
        "--history", action="store_true", help="Add file co-change from HEAD ancestry"
    )
    analyze.add_argument(
        "--dependencies", metavar="PACKAGE", help="Add cargo-modules graph for one library package"
    )
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
            )
            print(f"{status}: {output / 'summary.md'}")
            return 0 if status == "complete" else 1
    except (AuditError, OSError, ValueError) as error:
        print(f"refactor-audit: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
