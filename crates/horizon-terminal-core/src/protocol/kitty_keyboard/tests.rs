use super::*;

/// Pragmatic mechanism for checking `KITTY_COMPLIANCE` entries against real
/// test state: Rust has no reflection over `#[test]`/`#[ignore]` attributes,
/// so this is a hand-maintained `(test name, is_ignored)` registry kept
/// beside the table it checks. `compliance_table_tests_are_registered_and_correctly_flagged`
/// fails loudly (unknown-test panic, or an ignored-flag mismatch) if this
/// registry drifts from `KITTY_COMPLIANCE` itself; it can *not* detect a
/// registry entry going stale relative to the real `#[test]` attribute in
/// `terminal::tests` (e.g. someone adding `#[ignore]` there without updating
/// this list) — that class of drift needs a human to notice the two files
/// disagree, same as it would for any other hand-synced doc-vs-code pair.
const TEST_REGISTRY: &[(&str, bool)] = &[
    ("kitty_csi_u_truth_table", false),
    ("kitty_override_reports_super_modifier", false),
    (
        "navigation_keys_are_flag_invariant_and_spec_compliant",
        false,
    ),
    ("csi_u_text_key_truth_table", false),
    ("shift_letter_produces_csi_u_under_report_all_keys", false),
    (
        "csi_u_text_key_reports_alternate_for_shifted_letter_only",
        false,
    ),
    (
        "release_events_are_unimplemented_regardless_of_flags",
        false,
    ),
    ("csi_u_event_type_truth_table", false),
    ("csi_u_navigation_key_event_type_truth_table", false),
    (
        "high_function_keys_use_legacy_numbers_without_kitty_flags_and_pua_codes_with_them",
        false,
    ),
    ("very_high_function_keys_are_unimplemented", true),
    ("standalone_modifier_keypresses_are_unimplemented", true),
    ("keypad_keys_ignore_disambiguate_flag", true),
];

/// (a) Every `Compliant`/`Deviation`/`Bypassed` entry must name a real,
/// non-`#[ignore]`d test (it's describing verified-working behavior); every
/// `Unimplemented` entry that names a test must point at one that actually
/// is `#[ignore]`d (it's describing a known gap, not a regression). A test
/// name that isn't in `TEST_REGISTRY` at all panics immediately, catching
/// typos in either file.
#[test]
fn compliance_table_tests_are_registered_and_correctly_flagged() {
    for entry in KITTY_COMPLIANCE {
        let expect_ignored = matches!(entry.verdict, Verdict::Unimplemented(_));
        for name in entry.tests {
            let ignored = TEST_REGISTRY
                .iter()
                .find_map(|(registered, ignored)| (registered == name).then_some(*ignored))
                .unwrap_or_else(|| {
                    panic!(
                        "KITTY_COMPLIANCE entry {:?} names test `{name}`, which isn't in \
                         TEST_REGISTRY — keep the registry in sync with terminal::tests",
                        entry.feature
                    )
                });
            assert_eq!(
                ignored, expect_ignored,
                "entry {:?} names test `{name}` with #[ignore] = {ignored}, but its verdict \
                 ({:?}) expects #[ignore] = {expect_ignored}",
                entry.feature, entry.verdict
            );
        }
    }
}

/// (c) Always-green report: `cargo test print_compliance_matrix --
/// -- --nocapture` answers "where are we against the Kitty keyboard
/// protocol spec" straight from the terminal.
#[test]
fn print_compliance_matrix() {
    println!(
        "Kitty keyboard protocol compliance — {} entries",
        KITTY_COMPLIANCE.len()
    );
    println!("{}", "=".repeat(78));
    for entry in KITTY_COMPLIANCE {
        let verdict = match entry.verdict {
            Verdict::Compliant => "COMPLIANT".to_string(),
            Verdict::Deviation(reason) => format!("DEVIATION — {reason}"),
            Verdict::Unimplemented(reason) => format!("UNIMPLEMENTED — {reason}"),
            Verdict::Bypassed(path) => format!("BYPASSED — bypassed by `{path}`"),
        };
        println!("* {} [{}]", entry.feature, entry.key_class);
        println!("  {verdict}");
        if entry.tests.is_empty() {
            println!("  tests: (none)");
        } else {
            println!("  tests: {}", entry.tests.join(", "));
        }
        println!();
    }
}
