//! Fractional indexing over retained lowercase rank strings.
/// Computes a rank strictly between `lo` and `hi`.
///
/// `lo = None` represents the conceptual minimum (before everything);
/// `hi = None` represents the conceptual maximum (after everything).
/// Returns `None` if no midpoint exists in the `a`-`z` alphabet.
pub fn between(lo: Option<&str>, hi: Option<&str>) -> Option<String> {
    if lo
        .into_iter()
        .chain(hi)
        .any(|s| s.is_empty() || !s.bytes().all(|c| c.is_ascii_lowercase()))
        || matches!((lo,hi),(Some(l),Some(h)) if l>=h)
    {
        return None;
    }
    match (lo, hi) {
        (None, None) => Some("n".to_string()),
        (None, Some(h)) => before(h),
        (Some(l), None) => Some(after(l)),
        (Some(l), Some(h)) => between_strs(l, h),
    }
}

/// Issues a rank strictly before `hi` (and after the conceptual minimum).
fn before(hi: &str) -> Option<String> {
    let bytes = hi.as_bytes();
    if bytes.is_empty() {
        return None;
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
            Some(before(rest).map_or_else(|| "a".into(), |s| format!("a{s}")))
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
                let rest = std::str::from_utf8(&hi_b[i..]).ok()?;
                before(rest).map(|s| format!("{lo}{s}"))
            }
        }
        (Some(_), None) => {
            // hi is a proper prefix of lo — means hi < lo, a precondition
            // violation. Defensive: return None.
            None
        }
        (None, None) => {
            // Equal bounds have no strict midpoint.
            None
        }
    }
}

// Fractional indexing: use spare space in the integer suffix before growing
// a key. Existing rank strings retain their bytewise order.
fn after(lo: &str) -> String {
    let mut bytes = lo.as_bytes().to_vec();
    if let Some(index) = bytes.iter().rposition(|b| *b < b'z') {
        bytes[index] += 1;
        bytes.truncate(index + 1);
        String::from_utf8(bytes).expect("ASCII rank")
    } else {
        format!("{lo}n")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_and_adjacent_prefix_bounds() {
        for (lo, hi) in [("n", "n"), ("z", "a"), ("a", "aa"), ("N", "z")] {
            assert_eq!(between(Some(lo), Some(hi)), None);
        }
    }
    #[test]
    fn repeated_insertions_preserve_strict_order() {
        let mut keys = vec!["n".to_string()];
        for _ in 0..1000 {
            let next = between(keys.last().map(String::as_str), None).unwrap();
            assert!(keys.last().unwrap() < &next);
            keys.push(next);
        }
        assert!(keys.last().unwrap().len() < 100);
        let mut upper = "n".to_string();
        for _ in 0..1000 {
            let next = between(None, Some(&upper)).unwrap();
            assert!(next < upper);
            upper = next;
        }
    }
    #[test]
    fn midpoint_checks_small_alphabet_pairs() {
        let mut bounds = vec![];
        for a in b'a'..=b'z' {
            bounds.push(char::from(a).to_string());
            for b in b'a'..=b'z' {
                bounds.push(format!("{}{}", char::from(a), char::from(b)));
            }
        }
        bounds.sort();
        for pair in bounds.windows(2) {
            if let Some(mid) = between(Some(&pair[0]), Some(&pair[1])) {
                assert!(pair[0] < mid && mid < pair[1]);
            }
        }
    }
}
