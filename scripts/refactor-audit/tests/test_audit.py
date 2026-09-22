"""Fixtures exercise real analyzers and failures at their input/output boundaries."""

import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

from audit import scan, validate_config
from sources import cfg_value, partition
from tooling import HERE, ROOT, AuditError, Runner, Toolchain


def config():
    data = json.loads((HERE / "config.json").read_text())
    data["roots"] = ["src", "crates"]
    return data


class PartitionTests(unittest.TestCase):
    def test_invalid_config_is_rejected_before_starting_analysis(self):
        for invalid in (None, {}, {**config(), "clones": None}, {**config(), "history": []}):
            with self.subTest(config=invalid), self.assertRaises(AuditError):
                validate_config(invalid)

    def test_partition_preserves_coordinates_and_handles_cfg_boolean_logic(self):
        data = (HERE / "fixtures/boundaries.rs").read_bytes()
        prod, functions, _ = partition(data, False, "production", config()["test_attributes"])
        test, test_functions, _ = partition(data, False, "tests", config()["test_attributes"])
        self.assertEqual(len(prod), len(data))
        self.assertEqual(len(test), len(data))
        self.assertEqual(
            [i for i, x in enumerate(prod) if x == 10], [i for i, x in enumerate(data) if x == 10]
        )
        self.assertEqual(
            {f["name"] for f in functions},
            {
                "byte_literal",
                "after_byte",
                "borrowed_identifier",
                "with_closure",
                "bounded",
                "through_macro",
                "mixed_cfg",
                "only_production",
            },
        )
        self.assertEqual(
            {f["name"] for f in test_functions}, {"helper", "unit_check", "standalone_test"}
        )
        self.assertTrue(all(f["partition"] == "tests" for f in test_functions))
        self.assertIsNone(cfg_value('any(test, target_os="linux")', False))
        self.assertFalse(cfg_value("all(test, unix)", False))
        self.assertTrue(cfg_value("not(test)", False))

    def test_async_test_attribute_unicode_and_production_named_tests_module(self):
        data = (
            "mod tests { fn ordinary() {} }\n"
            '#[tokio::test]\nasync fn example() { let text = "日本語"; }\n'
            "#[gpui::test]\nfn gui_example() {}\n"
        ).encode()
        prod, functions, _ = partition(data, False, "production", config()["test_attributes"])
        self.assertEqual(len(prod), len(data))
        self.assertEqual([f["name"] for f in functions], ["ordinary"])
        _, tests, _ = partition(data, False, "tests", config()["test_attributes"])
        self.assertEqual({f["name"] for f in tests}, {"example", "gui_example"})

    def test_unparseable_input_is_an_error(self):
        with self.assertRaises(AuditError):
            partition(b"fn broken( {", False, "production", config()["test_attributes"])


class AnalyzerTests(unittest.TestCase):
    def test_e2e_module_includes_its_helpers_in_the_test_partition(self):
        (self.root / "src/e2e.rs").write_text(
            "fn setup_view() {}\n#[gpui::test]\nfn paints() { setup_view(); }\n"
        )
        _, production = self.analyze()
        self.assertNotIn("src/e2e.rs", {f["file"] for f in production["functions"]})
        _, tests = self.analyze("tests")
        self.assertEqual(
            {f["name"] for f in tests["functions"] if f["file"] == "src/e2e.rs"},
            {"setup_view", "paints"},
        )

    @classmethod
    def setUpClass(cls):
        cls.tools = Toolchain(ROOT)
        for name in ("rca", "jscpd", "ast-grep"):
            cls.tools.resolve(name)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="horizon-audit-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(
            [
                "git",
                "-C",
                str(self.root),
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
            check=True,
        )
        (self.root / ".gitignore").write_text("/target/\n")
        (self.root / "src/runtime").mkdir(parents=True)
        (self.root / "crates/sample/src").mkdir(parents=True)
        shutil.copyfile(HERE / "fixtures/boundaries.rs", self.root / "src/boundaries.rs")
        clone = (HERE / "fixtures/clone.rs").read_text()
        (self.root / "src/a.rs").write_text(clone)
        (self.root / "crates/sample/src/b.rs").write_text(clone)
        replacements = dict(
            summarize="describe",
            values="items",
            value="entry",
            total="sum",
            count="number",
            largest="maximum",
            mean="average",
            deviation="spread",
            delta="distance",
            above="higher",
            below="lower",
        )
        renamed = re.sub(
            r"\b(?:" + "|".join(replacements) + r")\b", lambda m: replacements[m[0]], clone
        )
        (self.root / "src/c.rs").write_text(renamed)
        (self.root / "src/runtime/visibility.rs").write_text(
            "pub fn exposed() {}\npub(crate) fn internal() {}\n"
        )
        (self.root / "src/tests.rs").write_text(
            "fn external_test_helper() {}\n#[test] fn outside() {}\n"
        )

    def analyze(self, selection="production", settings=None):
        output, status = scan(self.root, settings or config(), [], selection, tools=self.tools)
        data = json.loads((output / "report.json").read_text())
        self.assertEqual(status, "complete", data["errors"])
        return output, data

    def test_real_tools_cover_functions_clones_and_scoped_convention(self):
        before = {p: p.read_bytes() for p in self.root.rglob("*.rs")}
        output, data = self.analyze()
        self.assertEqual(len(data["functions"]), 13)
        self.assertEqual(data["closures"], 1)
        self.assertNotIn("src/tests.rs", {s["file"] for s in data["sources"]})
        self.assertEqual(
            [(r["file"], r["start"]) for r in data["rules"]], [("src/runtime/visibility.rs", 1)]
        )
        exact = [
            {f["file"] for f in pair["fragments"]}
            for pair in data["clones"]
            if pair["mode"] == "exact"
        ]
        normalized = [
            {f["file"] for f in pair["fragments"]}
            for pair in data["clones"]
            if pair["mode"] == "normalized"
        ]
        self.assertIn({"src/a.rs", "crates/sample/src/b.rs"}, exact)
        self.assertFalse(any("src/c.rs" in files for files in exact))
        self.assertTrue(any("src/c.rs" in files and len(files) == 2 for files in normalized))
        self.assertTrue(data["clone_groups"])
        self.assertIn("summary.md", {p.name for p in output.iterdir()})
        self.assertEqual(before, {p: p.read_bytes() for p in before})

    def test_tests_are_separate_and_all_keeps_partition_labels(self):
        _, tests = self.analyze("tests")
        self.assertEqual(len(tests["functions"]), 5)
        self.assertTrue(all(f["partition"] == "tests" for f in tests["functions"]))
        _, both = self.analyze("all")
        self.assertEqual(len(both["functions"]), 18)
        self.assertEqual({f["partition"] for f in both["functions"]}, {"production", "tests"})

    def test_parse_failure_has_report_and_non_success_status(self):
        (self.root / "src/broken.rs").write_text("fn broken( {")
        output, status = scan(self.root, config(), [], tools=self.tools)
        data = json.loads((output / "report.json").read_text())
        self.assertEqual(status, "failed")
        self.assertIn("src/broken.rs", data["errors"][0])
        self.assertIn("Partial results", (output / "summary.md").read_text())

    def test_bad_scope_and_empty_selection_do_not_look_clean(self):
        for paths in (["../escape"], ["missing"], [".gitignore"]):
            with self.subTest(paths=paths):
                output, status = scan(self.root, config(), paths, tools=self.tools)
                self.assertEqual(status, "failed")
                self.assertTrue(json.loads((output / "report.json").read_text())["errors"])

    def test_tool_failure_preserves_stderr(self):
        bad = self.root / "bad-tool"
        bad.write_text("#!/bin/sh\necho deliberate-fixture-failure >&2\nexit 7\n")
        bad.chmod(0o755)
        tools = Toolchain(ROOT)
        tools.used["rca"] = {"path": str(bad), "version": "fixture"}
        output, status = scan(self.root, config(), [], tools=tools)
        data = json.loads((output / "report.json").read_text())
        self.assertEqual(status, "failed")
        self.assertEqual(data["commands"][0]["exit_code"], 7)
        self.assertIn("deliberate-fixture-failure", (output / "raw/rca.stderr").read_text())

    def test_changed_input_is_rejected(self):
        import unittest.mock

        original = Runner.run

        def change_after_analysis(runner, name, args, cwd, timeout=300):
            result = original(runner, name, args, cwd, timeout)
            if name == "rule-0":
                (self.root / "src/a.rs").write_text("fn changed_during_scan() {}\n")
            return result

        with unittest.mock.patch.object(Runner, "run", change_after_analysis):
            output, status = scan(self.root, config(), [], tools=self.tools)
        self.assertEqual(status, "failed")
        self.assertIn(
            "Source changed", json.loads((output / "report.json").read_text())["errors"][0]
        )

    def test_exploratory_query_positive_and_negative_examples(self):
        input_file = self.root / "query.rs"
        input_file.write_text("fn sample() { reply.recv_timeout(limit); reply.try_recv(); }\n")
        result = subprocess.run(
            [
                str(self.tools.resolve("ast-grep")),
                "scan",
                "--rule",
                str(HERE / "queries/sync-reply.yml"),
                "--json=compact",
                str(input_file),
            ],
            capture_output=True,
            check=True,
        )
        matches = json.loads(result.stdout)
        self.assertEqual(len(matches), 1)
        self.assertEqual(matches[0]["text"], "reply.recv_timeout(limit)")

    def test_boundary_fixture_compiles(self):
        subprocess.run(
            [
                "rustc",
                "--edition=2024",
                "--crate-type=lib",
                "--emit=metadata",
                str(HERE / "fixtures/boundaries.rs"),
                "-o",
                str(self.root / "fixture.rmeta"),
            ],
            check=True,
            capture_output=True,
        )


if __name__ == "__main__":
    unittest.main()
