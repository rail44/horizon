"""Evidence freshness and comparisons must not turn changed scope into apparent progress."""

from copy import deepcopy
from pathlib import Path
import tempfile
import unittest

from comparison import compare
from reviews import assess_reviews, read_reviews, record_review, select_function, selector
from sources import exclude_nodes, partition
from tooling import AuditError


def report():
    return {
        "commit": "baseline", "selection": "production", "requested_paths": [], "config": {},
        "tools": {}, "functions": [{
            "file": "src/a.rs", "owner": "<free>", "name": "handle", "partition": "production",
            "start": 1, "end": 8, "sha256": "function-original", "physical_code_lines": 8,
            "cyclomatic": 4, "cognitive": 6,
        }],
        "sources": [{"file": "src/a.rs", "sha256": "file-original"},
                    {"file": "src/caller.rs", "sha256": "caller-original"}],
        "clones": [], "clone_groups": [],
    }


class ExclusionTests(unittest.TestCase):
    def test_excluding_function_and_match_arm_preserves_unicode_coordinates_and_other_code(self):
        source = '''#[inline]
fn board_action() { let text = "日本語"; }
fn other() { let board_label = "board"; }
fn dispatch(kind: Kind) {
    match kind { Kind::Board => board_action(), Kind::Terminal => other() }
}
'''.encode()
        rules = [{"query": '((function_item name: (identifier) @name) @exclude (#eq? @name "board_action"))', "reason": "separate work"},
                 {"query": '((match_arm pattern: (_) @pattern) @exclude (#eq? @pattern "Kind::Board"))', "reason": "separate work"}]
        masked, exclusions = exclude_nodes(source, rules)
        self.assertEqual(len(masked), len(source))
        self.assertEqual([i for i, c in enumerate(source) if c == 10], [i for i, c in enumerate(masked) if c == 10])
        _, functions, _ = partition(masked, False, "production", ["test"])
        self.assertEqual([f["name"] for f in functions], ["other", "dispatch"])
        self.assertEqual(functions[0]["start"], 3)
        self.assertEqual(len(exclusions), 2)
        self.assertIn(b'let board_label = "board"', masked)
        self.assertNotIn(b"Kind::Board", masked)
        self.assertIn(b"Kind::Terminal", masked)

    def test_partial_syntax_exclusion_fails_instead_of_hiding_parse_errors(self):
        with self.assertRaises(AuditError):
            exclude_nodes(b"fn example() {}", [{"query": "(identifier) @exclude", "reason": "invalid boundary"}])
        with self.assertRaises(AuditError):
            exclude_nodes(b"fn example() {}", [{"query": "(function_item) @wrong", "reason": "missing capture"}])


class ReviewTests(unittest.TestCase):
    def test_review_survives_line_moves_but_flags_body_and_related_changes(self):
        original = report()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "reviews.json"
            record_review(original, selector(original["functions"][0]), "preserve", "Distinct lifecycle", ["src/caller.rs"], path)
            ledger = read_reviews(path)
        changed = deepcopy(original)
        changed["functions"][0]["start"] += 10
        changed["functions"][0]["end"] += 10
        self.assertEqual(assess_reviews(changed, ledger)[0]["evidence"], "evidence_unchanged")
        changed["sources"][1]["sha256"] = "changed-caller"
        self.assertEqual(assess_reviews(changed, ledger)[0]["evidence"], "related_evidence_changed_or_absent")
        changed["functions"][0]["sha256"] = "changed-function"
        self.assertEqual(assess_reviews(changed, ledger)[0]["evidence"], "source_changed")
        changed["functions"] = []
        self.assertEqual(assess_reviews(changed, ledger)[0]["evidence"], "absent_from_scope")

    def test_ambiguous_identity_is_never_silently_paired_or_reviewed(self):
        ambiguous = report()
        ambiguous["functions"].append({**ambiguous["functions"][0], "start": 20})
        with self.assertRaises(AuditError):
            select_function(ambiguous, selector(ambiguous["functions"][0]))
        result = compare(ambiguous, report())
        self.assertEqual(len(result["ambiguous"]), 1)
        self.assertFalse(result["changed"])

    def test_split_mapping_includes_helper_complexity_and_unmatched_functions(self):
        before, after = report(), report()
        original = before["functions"][0]
        after["functions"] = [
            {**original, "name": "entry", "cognitive": 1},
            {**original, "name": "helper", "cognitive": 5},
            {**original, "name": "unrelated", "cognitive": 2},
        ]
        mapping = [{"label": "handler", "before": [selector(original)],
                    "after": [selector(f) for f in after["functions"][:2]]}]
        result = compare(before, after, mapping)
        self.assertEqual(result["correspondences"][0]["max_after"]["cognitive"], 5)
        self.assertEqual(result["correspondences"][0]["max_after"]["functions"], 2)
        self.assertEqual([f["name"] for f in result["added"]], ["unrelated"])
        self.assertFalse(result["removed"])
        with self.assertRaises(AuditError):
            compare(before, after, mapping + mapping)

    def test_scope_rule_and_tool_changes_are_not_comparable(self):
        before = report()
        for field, value in (("selection", "tests"), ("config", {"exclude": ["src/a.rs"]}),
                             ("requested_paths", ["src"]), ("rules_sha256", {"rule": "new"}),
                             ("tools", {"rca": {"sha256": "new"}}),
                             ("implementation", {"sources.py": "changed-classification"})):
            after = deepcopy(before)
            after[field] = value
            with self.subTest(field=field), self.assertRaises(AuditError):
                compare(before, after)
