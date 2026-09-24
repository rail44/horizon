"""Conditional and lexical identity must not pair unrelated implementations."""

from copy import deepcopy
from pathlib import Path
import tempfile
import unittest

from comparison import compare
from reviews import assess_reviews, read_reviews, record_review, select_function, selector
from sources import partition
from test_review import report
from tooling import AuditError


def scan(source):
    result = report()
    _, functions, _ = partition(source.encode(), False, "all", ["test"])
    result['functions'] = [
        {**f, 'file': 'src/a.rs', 'physical_code_lines': 1, 'cyclomatic': 1, 'cognitive': 0}
        for f in functions
    ]
    return result


class VariantTests(unittest.TestCase):
    def test_tests_only_partition_keeps_enclosing_identity(self):
        source = b'''#[cfg(unix)] mod platform {
impl First { #[cfg(test)] fn run() {} }
impl Second { #[cfg(test)] fn run() {} }
}'''
        _, functions, _ = partition(source, False, "tests", ["test"])
        self.assertEqual([f['owner'] for f in functions], ['First', 'Second'])
        self.assertTrue(all(f['variant'] == 'cfg(unix) / mod:platform / cfg(test)'
                            for f in functions))

    def test_inherits_same_file_conditions_and_distinguishes_modules_and_traits(self):
        result = scan('''#![cfg(unix)]
#[cfg(target_os = "linux")]
mod platform {
    #![cfg(feature = "two words")]
    #[cfg_attr(feature = "optional", cfg(enabled))]
    impl Trait for Thing { fn run() {} }
    impl Other for Thing { fn run() {} }
    impl Thing { fn run() {} }
}
mod alternate { fn run() {} }
trait First { fn run() {} }
trait Second { fn run() {} }
''')
        functions = result['functions']
        self.assertEqual(len(functions), 6)
        self.assertEqual(len({f['variant'] for f in functions}), 6)
        self.assertTrue(all('cfg(unix)' in f['variant'] for f in functions))
        first = functions[0]['variant']
        for part in ['mod:platform', 'cfg(target_os="linux")', 'cfg(feature="two words")',
                     'cfg_attr(feature="optional",cfg(enabled))', 'trait:Trait']:
            self.assertIn(part, first)
        self.assertNotIn('target_os', functions[3]['variant'])
        self.assertIn('trait_def:First', functions[4]['variant'])
        self.assertIn('trait_def:Second', functions[5]['variant'])

    def test_line_moves_and_cfg_formatting_keep_pairing_but_condition_changes_do_not(self):
        before = scan('#[cfg(target_os="linux")] fn run() {}')
        moved = scan('// moved\n\n#[cfg( /* note */ target_os = "linux" )]\nfn run() {}')
        self.assertNotEqual(before['functions'][0]['start'], moved['functions'][0]['start'])
        result = compare(before, moved)
        self.assertFalse(result['changed'] or result['added'] or result['removed'] or result['ambiguous'])
        changed = scan('#[cfg(target_os="macos")] fn run() {}')
        self.assertNotEqual(before['functions'][0]['sha256'], changed['functions'][0]['sha256'])
        result = compare(before, changed)
        self.assertEqual(len(result['added']), 1)
        self.assertEqual(len(result['removed']), 1)

    def test_explicit_variants_are_reviewable_and_legacy_selectors_stay_fail_closed(self):
        before = scan('#[cfg(unix)] fn run() {}\n#[cfg(not(unix))] fn run() {}')
        target = selector(before['functions'][0])
        self.assertIn('variant', target)
        self.assertEqual(select_function(before, target), before['functions'][0])
        legacy = {k:v for k,v in target.items() if k != 'variant'}
        with self.assertRaises(AuditError):
            select_function(before, legacy)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'reviews.json'
            record_review(before, target, 'preserve', 'Different host boundaries', [], path)
            ledger = read_reviews(path)
        moved = deepcopy(before)
        moved['functions'].reverse()
        moved['functions'][0]['start'] += 20
        self.assertEqual(assess_reviews(moved, ledger)[0]['evidence'], 'evidence_unchanged')
        changed = scan('#[cfg(windows)] fn run() {}\n#[cfg(not(unix))] fn run() {}')
        self.assertEqual(assess_reviews(changed, ledger)[0]['evidence'], 'absent_from_scope')

    def test_comparison_pairs_variants_and_rejects_overlap_with_an_all_variants_mapping(self):
        before = scan('#[cfg(unix)] fn run() {}\n#[cfg(not(unix))] fn run() {}')
        after = deepcopy(before)
        after['functions'].reverse()
        after['functions'][0]['cognitive'] = 7
        result = compare(before, after)
        self.assertEqual(len(result['changed']), 1)
        self.assertIn('not(unix)', result['changed'][0]['after']['variant'])
        self.assertFalse(result['ambiguous'] or result['added'] or result['removed'])
        target = selector(before['functions'][0])
        family = {k:v for k,v in target.items() if k != 'variant'}
        mappings = [{'label': 'family', 'before': [{**family, 'variants': 'all'}],
                     'after': [{**family, 'variants': 'all'}]}]
        self.assertEqual(compare(before, after, mappings)['correspondences'][0]['max_after']['functions'], 2)
        mappings.append({'label':'duplicate', 'before':[target], 'after':[target]})
        with self.assertRaises(AuditError):
            compare(before, after, mappings)

    def test_legacy_review_detects_changed_conditions_and_new_nested_scope(self):
        before = scan('#[cfg(unix)] fn run() {}')
        legacy = {k:v for k,v in selector(before['functions'][0]).items() if k != 'variant'}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'reviews.json'
            record_review(before, legacy, 'preserve', 'Reviewed condition', [], path)
            ledger = read_reviews(path)
        self.assertEqual(assess_reviews(before, ledger)[0]['evidence'], 'evidence_unchanged')
        changed = scan('#[cfg(windows)] fn run() {}')
        self.assertEqual(assess_reviews(changed, ledger)[0]['evidence'], 'source_changed')
        nested = scan('fn outer() { fn run() {} }')
        self.assertIn('fn:outer', nested['functions'][1]['variant'])
