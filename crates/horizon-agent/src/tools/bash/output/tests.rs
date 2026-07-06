use std::path::Path;

use super::{cap, spill};

// --- short output: unchanged --------------------------------------------

#[test]
fn output_at_or_under_the_cap_is_returned_unchanged() {
    let short = "hello\nworld\n";
    let capped = cap(short, 30_000, None);

    assert!(!capped.truncated);
    assert_eq!(capped.shown, short);
}

#[test]
fn output_exactly_at_the_cap_is_not_truncated() {
    let exact: String = "a".repeat(100);
    let capped = cap(&exact, 100, None);

    assert!(!capped.truncated);
    assert_eq!(capped.shown, exact);
}

// --- over-cap: both head and tail survive --------------------------------

#[test]
fn truncation_keeps_both_a_head_and_a_tail() {
    // Build output where the head and tail are distinguishable so we can
    // check both ends survived, not just "some middle slice".
    let head_marker = "HEAD".repeat(50); // 200 chars
    let filler = "x".repeat(10_000);
    let tail_marker = "TAIL".repeat(50); // 200 chars
    let full = format!("{head_marker}{filler}{tail_marker}");

    let capped = cap(&full, 300, None);

    assert!(capped.truncated);
    assert!(
        capped.shown.starts_with("HEAD"),
        "head content should open the shown output: {}",
        &capped.shown[..20.min(capped.shown.len())]
    );
    assert!(
        capped.shown.ends_with("TAIL"),
        "tail content should close the shown output: {}",
        &capped.shown[capped.shown.len().saturating_sub(20)..]
    );
    // Neither end degenerated to nothing.
    assert!(capped.shown.starts_with(&"HEAD".repeat(10)));
    assert!(capped.shown.ends_with(&"TAIL".repeat(10)));
}

#[test]
fn truncation_notice_reports_the_omitted_count_and_the_spill_path() {
    let full = "a".repeat(1_000);
    let path = Path::new("/tmp/horizon-bash-deadbeef.log");

    let capped = cap(&full, 100, Some(path));

    assert!(capped.truncated);
    // cap 100, head 100/3 = 33, tail = 67, so 1000 - 100 = 900 omitted.
    assert!(
        capped.shown.contains("900 chars omitted"),
        "notice should state the omitted count: {}",
        capped.shown
    );
    assert!(
        capped.shown.contains("/tmp/horizon-bash-deadbeef.log"),
        "notice should inline the spill path so the model doesn't have to \
         separately notice `output_file`: {}",
        capped.shown
    );
}

#[test]
fn truncation_notice_still_says_something_useful_when_the_spill_failed() {
    let full = "a".repeat(1_000);

    let capped = cap(&full, 100, None);

    assert!(capped.truncated);
    assert!(
        capped.shown.contains("could not be saved"),
        "notice should not silently omit the fact that there's no spill \
         file to fall back on: {}",
        capped.shown
    );
}

// --- ratio: tail gets more of the budget than the head -------------------

#[test]
fn head_gets_roughly_a_third_of_the_cap_and_tail_the_rest() {
    let full = "a".repeat(10_000);
    let capped = cap(&full, 3_000, None);

    // The notice text sits between the head and tail runs of 'a's; measure
    // each run's length directly rather than assuming exact byte offsets.
    let first_gap = capped.shown.find("\n\n[...").expect("head/notice gap");
    let head_len = first_gap;
    let last_gap = capped.shown.rfind("...]\n\n").expect("notice/tail gap");
    let tail_len = capped.shown.len() - (last_gap + "...]\n\n".len());

    assert_eq!(head_len, 1_000, "head should be cap_chars/3");
    assert_eq!(tail_len, 2_000, "tail should get the rest of the cap");
}

// --- ratio boundary cases --------------------------------------------------

#[test]
fn tiny_cap_values_do_not_panic_and_still_omit_correctly() {
    for cap_chars in [0usize, 1, 2, 3, 4] {
        let full = "a".repeat(cap_chars + 10);
        let capped = cap(&full, cap_chars, None);
        assert!(capped.truncated, "cap={cap_chars}");

        let expected_head_len = cap_chars / 3;
        let expected_tail_len = cap_chars - expected_head_len;

        // The notice text itself can contain the letter 'a' (e.g. "saved"),
        // so measure the head/tail runs by locating the notice's
        // delimiters rather than counting 'a's across the whole string.
        let head_gap = capped.shown.find("\n\n[...").expect("head/notice gap");
        assert_eq!(head_gap, expected_head_len, "cap={cap_chars}");

        let tail_gap = capped.shown.rfind("...]\n\n").expect("notice/tail gap");
        let tail_start = tail_gap + "...]\n\n".len();
        assert_eq!(
            capped.shown.len() - tail_start,
            expected_tail_len,
            "cap={cap_chars}"
        );
    }
}

#[test]
fn exactly_one_char_over_the_cap_omits_exactly_one_char() {
    let full = "a".repeat(101);
    let capped = cap(&full, 100, None);

    assert!(capped.truncated);
    assert!(capped.shown.contains("1 chars omitted"));
}

// --- UTF-8 codepoint boundaries -------------------------------------------

#[test]
fn multibyte_characters_are_never_split_mid_codepoint() {
    // Japanese characters are 3 bytes each in UTF-8; a byte-index split at
    // an arbitrary offset would very likely land inside one of them and
    // either panic or corrupt the string. Char-based slicing must not.
    let full: String = "あ".repeat(1_000); // 1000 chars, 3000 bytes
    let capped = cap(&full, 100, None);

    assert!(capped.truncated);
    // No panic above is itself most of the proof; also check the surviving
    // pieces are made of whole, uncorrupted characters.
    let head_part = capped.shown.split("\n\n[...").next().unwrap();
    assert!(head_part.chars().all(|c| c == 'あ'));
    let tail_part = capped.shown.rsplit("...]\n\n").next().unwrap();
    assert!(!tail_part.is_empty());
    assert!(tail_part.chars().all(|c| c == 'あ'));
}

#[test]
fn mixed_width_characters_land_on_exact_char_counts() {
    // Mix of 1-, 2-, and 3-byte characters so a byte-oriented cap would cut
    // at inconsistent points relative to the char count.
    let unit = "a€あ"; // 1 + 2 + 3 = 3 chars, 6 bytes
    let full = unit.repeat(200); // 600 chars
    let capped = cap(&full, 60, None);

    assert!(capped.truncated);
    let head_part = capped.shown.split("\n\n[...").next().unwrap();
    let expected_head: String = full.chars().take(60 / 3).collect(); // head share = cap/3 = 20
    assert_eq!(head_part.chars().count(), 20);
    assert_eq!(head_part, expected_head);
}

// --- spill --------------------------------------------------------------

#[test]
fn spill_writes_the_full_text_and_returns_a_readable_path() {
    let full = "spill me\nline two\n";
    let path = spill(full).expect("spill should succeed in a normal environment");

    let contents = std::fs::read_to_string(&path).expect("spilled file should be readable");
    assert_eq!(contents, full);

    let _ = std::fs::remove_file(&path);
}
