//! Lexicographic rank strings for ordering items in a queue.
//!
//! Ranks are lowercase-ASCII strings (`a`-`z`) that sort lexicographically.
//! New ranks are issued as midpoints between existing ones (or between an
//! existing rank and a conceptual min/max), so inserting an item never
//! requires renumbering its neighbours. The alphabet is `a`-`z` (26 glyphs);
//! the midpoint glyph is `n` (index 13 of 0-25), giving roughly equal room
//! above and below on the first insertion.
//!
//! `between` returns `None` only when the two bounds have converged to
//! adjacent characters at every position (e.g. `"a"` vs the conceptual
//! minimum) — extremely unlikely in practice because the first rank issued
//! is `"n"` (centre of the alphabet). A future rebalance pass could
//! re-derive all ranks if it ever happens; for v1 the caller reports an
//! error.

/// Computes a rank strictly between `lo` and `hi`.
///
/// `lo = None` represents the conceptual minimum (before everything);
/// `hi = None` represents the conceptual maximum (after everything).
/// Returns `None` if no midpoint exists in the `a`-`z` alphabet.
pub fn between(lo: Option<&str>, hi: Option<&str>) -> Option<String> {
    match (lo, hi) {
        (None, None) => Some("n".to_string()),
        (None, Some(h)) => before(h),
        (Some(l), None) => Some(format!("{l}n")),
        (Some(l), Some(h)) => between_strs(l, h),
    }
}

/// Issues a rank strictly before `hi` (and after the conceptual minimum).
fn before(hi: &str) -> Option<String> {
    let bytes = hi.as_bytes();
    if bytes.is_empty() {
        return Some("n".to_string());
    }
    let hv = bytes[0] - b'a';
    if hv > 0 {
        let mid = hv / 2;
        if mid > 0 {
            Some(char::from(mid + b'a').to_string())
        } else {
            // hv is 1 ('b'): midpoint rounds to 'a' (the minimum glyph),
            // which leaves no room below — so emit 'a' + midpoint-of-full-range.
            Some("an".to_string())
        }
    } else {
        // hi starts with 'a' — recurse into the tail to find room below.
        if bytes.len() > 1 {
            let rest = std::str::from_utf8(&bytes[1..]).ok()?;
            before(rest).map(|s| format!("a{s}"))
        } else {
            None // hi is exactly "a" — nothing below in the a-z alphabet
        }
    }
}

/// Issues a rank strictly between two known ranks `lo < hi`.
fn between_strs(lo: &str, hi: &str) -> Option<String> {
    let lo_b = lo.as_bytes();
    let hi_b = hi.as_bytes();

    // Find the first position where lo and hi differ (or one ends).
    let mut i = 0;
    while i < lo_b.len() && i < hi_b.len() && lo_b[i] == hi_b[i] {
        i += 1;
    }

    match (lo_b.get(i), hi_b.get(i)) {
        (Some(&lc), Some(&hc)) => {
            // First differing position: lo[i] < hi[i] (guaranteed by lo < hi).
            let (lv, hv) = (lc - b'a', hc - b'a');
            let mid = (lv + hv) / 2;
            if mid > lv {
                // Room for a fresh midpoint glyph at this position.
                let prefix = std::str::from_utf8(&lo_b[..i]).ok()?;
                Some(format!("{prefix}{}", char::from(mid + b'a')))
            } else {
                // Adjacent glyphs (e.g. 'n' and 'o'): append 'n' to lo.
                // lo + "n" is always > lo (prefix extension) and < hi
                // (because lo[i] < hi[i] at the first difference).
                Some(format!("{lo}n"))
            }
        }
        (None, Some(&hc)) => {
            // lo is a proper prefix of hi — lo < hi.
            let hv = hc - b'a';
            if hv > 0 {
                let mid = hv / 2;
                if mid > 0 {
                    Some(format!("{lo}{}", char::from(mid + b'a')))
                } else {
                    Some(format!("{lo}an"))
                }
            } else {
                // hi[i] == 'a': need room below 'a' at this position —
                // recurse into hi's tail, prefixed by lo + 'a'.
                let rest = std::str::from_utf8(&hi_b[i + 1..]).ok()?;
                before(rest).map(|s| format!("{lo}a{s}"))
            }
        }
        (Some(_), None) => {
            // hi is a proper prefix of lo — means hi < lo, a precondition
            // violation. Defensive: return None.
            None
        }
        (None, None) => {
            // lo == hi — append midpoint.
            Some(format!("{lo}n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts that `between(lo, hi)` yields a string strictly between them.
    fn assert_between(lo: Option<&str>, hi: Option<&str>) {
        let mid = between(lo, hi).expect("midpoint should exist");
        if let Some(l) = lo {
            assert!(l < mid.as_str(), "{l:?} < {mid:?} failed");
        }
        if let Some(h) = hi {
            assert!(mid.as_str() < h, "{mid:?} < {h:?} failed");
        }
    }

    #[test]
    fn first_item_gets_n() {
        assert_eq!(between(None, None), Some("n".to_string()));
    }

    #[test]
    fn midpoint_issuance() {
        // between("n", "s") → "p" (midpoint of n=13, s=18 → 15 → 'p')
        assert_eq!(between(Some("n"), Some("s")), Some("p".to_string()));
        // between("n", "p") → "o" (midpoint of n=13, p=15 → 14 → 'o')
        assert_eq!(between(Some("n"), Some("p")), Some("o".to_string()));
    }

    #[test]
    fn adjacent_chars_append_n() {
        // between("n", "o") → "nn" (adjacent, append midpoint-of-range)
        let r = between(Some("n"), Some("o")).unwrap();
        assert_between(Some("n"), Some("o"));
        assert_eq!(r, "nn");
    }

    #[test]
    fn between_insertion_stability() {
        // Simulate a sequence of insertions and verify sort order is maintained.
        let mut ranks = vec![between(None, None).unwrap()]; // "n"

        // Insert before first
        ranks.push(between(None, Some(&ranks[0])).unwrap()); // "g"
        ranks.sort();
        assert_eq!(ranks, vec!["g", "n"]);

        // Insert between "g" and "n"
        let mid = between(Some("g"), Some("n")).unwrap();
        ranks.push(mid.clone());
        ranks.sort();
        assert_eq!(ranks, vec!["g", &mid, "n"]);

        // Insert after last
        ranks.push(between(Some(ranks.last().unwrap()), None).unwrap());
        ranks.sort();
        // All still sorted
        for w in ranks.windows(2) {
            assert!(w[0] < w[1], "{} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn before_first_item() {
        // before("n") → "g" (midpoint of a=0, n=13 → 6 → 'g')
        assert_eq!(before("n"), Some("g".to_string()));
        // before("g") → "d" (midpoint of a=0, g=6 → 3 → 'd')
        assert_eq!(before("g"), Some("d".to_string()));
    }

    #[test]
    fn after_last_item() {
        // after("n") → "nn"
        assert_eq!(between(Some("n"), None), Some("nn".to_string()));
    }

    #[test]
    fn deep_recursion_before() {
        // before("an") → "a" + before("n") = "ag"
        assert_eq!(before("an"), Some("ag".to_string()));
        assert_between(None, Some("an"));
    }

    #[test]
    fn many_insertions_at_top_stay_sorted() {
        let mut ranks = vec!["n".to_string()];
        for _ in 0..20 {
            let first = ranks[0].as_str();
            let new = between(None, Some(first)).expect("room should exist");
            assert!(new.as_str() < first, "{new} < {first}");
            ranks.push(new);
            ranks.sort();
        }
        // Verify global sort order
        for w in ranks.windows(2) {
            assert!(w[0] < w[1], "{} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn many_insertions_at_bottom_stay_sorted() {
        let mut ranks = vec!["n".to_string()];
        for _ in 0..20 {
            let last = ranks.last().unwrap().as_str();
            let new = between(Some(last), None).unwrap();
            assert!(last < new.as_str(), "{last} < {new}");
            ranks.push(new);
            ranks.sort();
        }
        for w in ranks.windows(2) {
            assert!(w[0] < w[1], "{} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn many_insertions_in_middle_stay_sorted() {
        let mut ranks = vec!["g".to_string(), "n".to_string()];
        for _ in 0..20 {
            // Always insert between the first two
            let mid = between(Some(&ranks[0]), Some(&ranks[1])).expect("room");
            assert!(ranks[0] < mid, "{} < {}", ranks[0], mid);
            assert!(mid < ranks[1], "{} < {}", mid, ranks[1]);
            ranks.insert(1, mid);
        }
        for w in ranks.windows(2) {
            assert!(w[0] < w[1], "{} < {}", w[0], w[1]);
        }
    }
}
