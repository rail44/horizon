//! Re-anchoring: finding where a comment's quote is in a page's current
//! logical text after the page changed.
//!
//! The quote matching is a port of the Hypothesis client's
//! `src/annotator/anchoring/match-quote.ts` (Copyright (c) 2013-2019
//! Hypothes.is Project and contributors, BSD-2-Clause; the full notice is in
//! this crate's `LICENSE-hypothesis`): candidates are the exact occurrences
//! of the quote, or failing those the closest approximate ones within
//! `min(256, len / 2)` edits, each scored on the quote (weight 50), the
//! text before it against the stored prefix (20), the text after it against
//! the stored suffix (20) and its distance from the stored position (2).
//!
//! The approximate search returns what the `approx-string-match` package
//! Hypothesis uses returns — every match with the lowest edit count, its
//! start chosen to make it longest — but computes it with Sellers' dynamic
//! program and Ukkonen's cut-off rather than porting Myers' bit-vector
//! algorithm. Only quotes with no exact occurrence reach it.
//!
//! Everything counts characters (code points); Hypothesis counts UTF-16 code
//! units.

use std::ops::Range;

use crate::annotation::Quote;
use crate::logical::LogicalText;

/// Where a comment sits in the current text.
#[derive(Debug, Clone, PartialEq)]
pub enum Anchor {
    /// The quote is still at its stored position.
    Unchanged(Range<usize>),
    /// Found elsewhere, or with edits: `score` is in `0.0..=1.0`, and
    /// `exact` tells whether the matched text equals the quote.
    Moved {
        range: Range<usize>,
        score: f64,
        exact: bool,
    },
    /// No match within the error budget.
    Orphaned,
}

impl Anchor {
    pub fn range(&self) -> Option<Range<usize>> {
        match self {
            Anchor::Unchanged(range) | Anchor::Moved { range, .. } => Some(range.clone()),
            Anchor::Orphaned => None,
        }
    }
}

/// Anchors `quote` in `logical`: its stored position first, as Hypothesis
/// tries a position selector before the quote, then quote matching with
/// that position as the hint.
pub fn anchor(logical: &LogicalText, quote: &Quote) -> Anchor {
    let text: Vec<char> = logical.as_str().chars().collect();
    let exact: Vec<char> = quote.exact.chars().collect();
    if quote.end <= text.len()
        && quote.start < quote.end
        && text[quote.start..quote.end] == exact[..]
    {
        return Anchor::Unchanged(quote.start..quote.end);
    }
    let prefix: Vec<char> = quote.prefix.chars().collect();
    let suffix: Vec<char> = quote.suffix.chars().collect();
    match match_quote(&text, &exact, &prefix, &suffix, Some(quote.start)) {
        Some(found) => Anchor::Moved {
            exact: text[found.range.clone()] == exact[..],
            range: found.range,
            score: found.score,
        },
        None => Anchor::Orphaned,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuoteMatch {
    pub range: Range<usize>,
    pub score: f64,
}

/// `matchQuote`: the best approximate match for `quote` in `text`, given
/// the context it originally appeared in.
pub fn match_quote(
    text: &[char],
    quote: &[char],
    prefix: &[char],
    suffix: &[char],
    hint: Option<usize>,
) -> Option<QuoteMatch> {
    if quote.is_empty() {
        return None;
    }
    let max_errors = 256.min(quote.len() / 2);
    let matches = search(text, quote, max_errors);

    const QUOTE_WEIGHT: f64 = 50.0;
    const PREFIX_WEIGHT: f64 = 20.0;
    const SUFFIX_WEIGHT: f64 = 20.0;
    const POS_WEIGHT: f64 = 2.0;
    let score = |found: &Match| {
        let quote_score = 1.0 - found.errors as f64 / quote.len() as f64;
        let prefix_score = if prefix.is_empty() {
            1.0
        } else {
            let from = found.start.saturating_sub(prefix.len());
            text_match_score(&text[from..found.start], prefix)
        };
        let suffix_score = if suffix.is_empty() {
            1.0
        } else {
            let to = (found.end + suffix.len()).min(text.len());
            text_match_score(&text[found.end..to], suffix)
        };
        let pos_score = hint.map_or(1.0, |hint| {
            1.0 - found.start.abs_diff(hint) as f64 / text.len() as f64
        });
        let raw = QUOTE_WEIGHT * quote_score
            + PREFIX_WEIGHT * prefix_score
            + SUFFIX_WEIGHT * suffix_score
            + POS_WEIGHT * pos_score;
        raw / (QUOTE_WEIGHT + PREFIX_WEIGHT + SUFFIX_WEIGHT + POS_WEIGHT)
    };

    // The first of the best, as a stable descending sort's head.
    let mut best: Option<QuoteMatch> = None;
    for found in &matches {
        let score = score(found);
        if best.as_ref().is_none_or(|best| score > best.score) {
            best = Some(QuoteMatch {
                range: found.start..found.end,
                score,
            });
        }
    }
    best
}

/// `textMatchScore`: similarity of `text` to `pattern` in `0.0..=1.0`.
fn text_match_score(text: &[char], pattern: &[char]) -> f64 {
    if pattern.is_empty() || text.is_empty() {
        return 0.0;
    }
    match search(text, pattern, pattern.len()).first() {
        Some(found) => 1.0 - found.errors as f64 / pattern.len() as f64,
        None => 0.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Match {
    start: usize,
    end: usize,
    errors: usize,
}

/// match-quote's `search`: every exact occurrence (overlapping ones too),
/// or failing any, the approximate matches with the fewest errors.
fn search(text: &[char], pattern: &[char], max_errors: usize) -> Vec<Match> {
    if pattern.is_empty() || pattern.len() > text.len() + max_errors {
        return approximate(text, pattern, max_errors);
    }
    let exact: Vec<Match> = text
        .windows(pattern.len())
        .enumerate()
        .filter(|(_, window)| *window == pattern)
        .map(|(start, _)| Match {
            start,
            end: start + pattern.len(),
            errors: 0,
        })
        .collect();
    if exact.is_empty() {
        approximate(text, pattern, max_errors)
    } else {
        exact
    }
}

/// `approx-string-match`'s `search`: the matches of `pattern` in `text` with
/// the lowest edit count, if it is at most `max_errors`; each match's start
/// is the one that makes it longest.
fn approximate(text: &[char], pattern: &[char], max_errors: usize) -> Vec<Match> {
    let ends = match_ends(text, pattern, max_errors);
    let reversed_pattern: Vec<char> = pattern.iter().rev().copied().collect();
    ends.into_iter()
        .map(|(end, errors)| {
            let min_start = end.saturating_sub(pattern.len() + errors);
            let reversed_text: Vec<char> = text[min_start..end].iter().rev().copied().collect();
            let longest = match_ends(&reversed_text, &reversed_pattern, errors)
                .into_iter()
                .map(|(reversed_end, _)| reversed_end)
                .max()
                .unwrap_or(0);
            Match {
                start: end - longest,
                end,
                errors,
            }
        })
        .collect()
}

/// The end offsets (exclusive) at which some substring of `text` is within
/// the lowest edit count of `pattern`, with that count; empty when the
/// lowest exceeds `max_errors`.
///
/// Sellers' column-wise dynamic program (the empty prefix of the pattern
/// matches anywhere at no cost), with every value above `max_errors`
/// saturated and Ukkonen's cut-off: rows below the last one within the
/// budget are not computed, since a diagonal never decreases.
fn match_ends(text: &[char], pattern: &[char], max_errors: usize) -> Vec<(usize, usize)> {
    let m = pattern.len();
    if m == 0 {
        return Vec::new();
    }
    let k = max_errors.min(m);
    let over = k + 1;
    let mut column: Vec<usize> = (0..=m).map(|row| row.min(over)).collect();
    // The last row whose value is within the budget.
    let mut last_active = k;
    let mut ends: Vec<(usize, usize)> = Vec::new();
    let mut best = over;
    for (j, &ch) in text.iter().enumerate() {
        let top = (last_active + 1).min(m);
        let mut diagonal = column[0];
        for row in 1..=top {
            let substitution = diagonal + usize::from(pattern[row - 1] != ch);
            let value = substitution
                .min(column[row] + 1)
                .min(column[row - 1] + 1)
                .min(over);
            diagonal = column[row];
            column[row] = value;
        }
        last_active = top;
        while last_active > 0 && column[last_active] > k {
            column[last_active] = over;
            last_active -= 1;
        }
        if last_active == m {
            let errors = column[m];
            if errors < best {
                best = errors;
                ends.clear();
            }
            if errors == best {
                ends.push((j + 1, errors));
            }
        }
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    /// The edit distance of `pattern` to its best-matching substring ending
    /// at each offset, by the textbook full table.
    fn reference_ends(text: &[char], pattern: &[char], max_errors: usize) -> Vec<(usize, usize)> {
        let m = pattern.len();
        let mut previous: Vec<usize> = (0..=m).collect();
        let mut all = Vec::new();
        for (j, &ch) in text.iter().enumerate() {
            let mut current = vec![0; m + 1];
            for row in 1..=m {
                current[row] = (previous[row - 1] + usize::from(pattern[row - 1] != ch))
                    .min(previous[row] + 1)
                    .min(current[row - 1] + 1);
            }
            all.push((j + 1, current[m]));
            previous = current;
        }
        let Some(lowest) = all.iter().map(|(_, errors)| *errors).min() else {
            return Vec::new();
        };
        if lowest > max_errors {
            return Vec::new();
        }
        all.into_iter()
            .filter(|(_, errors)| *errors == lowest)
            .collect()
    }

    #[test]
    fn the_cut_off_search_agrees_with_the_full_table() {
        let texts = [
            "the quick brown fox jumps over the lazy dog",
            "abababababab",
            "サンドボックスはまず nono を起動する",
            "",
            "xyz",
        ];
        let patterns = [
            "quick",
            "quack",
            "brwn fx",
            "abba",
            "nono",
            "サンドボクス",
            "zzzz",
            "x",
        ];
        for text in texts {
            for pattern in patterns {
                for max_errors in 0..=pattern.chars().count() {
                    let (text, pattern) = (chars(text), chars(pattern));
                    assert_eq!(
                        match_ends(&text, &pattern, max_errors),
                        reference_ends(&text, &pattern, max_errors),
                        "{text:?} / {pattern:?} / {max_errors}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_approximate_match_starts_where_it_is_longest() {
        let found = approximate(&chars("say helo world"), &chars("hello"), 2);
        assert_eq!(
            found,
            vec![Match {
                start: 4,
                end: 8,
                errors: 1
            }]
        );
    }

    fn quote_in(text: &str, exact: &str, nth: usize) -> Quote {
        let logical = LogicalText::of_markdown(text);
        let start_byte = logical
            .as_str()
            .match_indices(exact)
            .nth(nth)
            .map(|(at, _)| at)
            .unwrap();
        let start = logical.as_str()[..start_byte].chars().count();
        Quote::of(&logical, start..start + exact.chars().count()).unwrap()
    }

    #[test]
    fn an_untouched_quote_stays_put() {
        let page = "The helper runs nono first.";
        let quote = quote_in(page, "nono", 0);
        assert_eq!(
            anchor(&LogicalText::of_markdown(page), &quote),
            Anchor::Unchanged(16..20)
        );
    }

    #[test]
    fn a_quote_follows_text_inserted_before_it() {
        let quote = quote_in("The helper runs nono first.", "nono", 0);
        let edited = LogicalText::of_markdown("Since 2026, the sandbox helper runs nono first.");
        let found = anchor(&edited, &quote);
        assert_eq!(edited.slice(found.range().unwrap()), "nono");
        assert!(matches!(found, Anchor::Moved { exact: true, .. }));
    }

    #[test]
    fn context_picks_among_repeated_quotes() {
        let page = "Agents run tools. Terminals run shells. Agents run tools again.";
        let second = quote_in(page, "run", 1);
        // The first sentence is removed: positions shift, context still fits.
        let edited = LogicalText::of_markdown("Terminals run shells. Agents run tools again.");
        assert_eq!(anchor(&edited, &second).range(), Some(10..13));
    }

    #[test]
    fn a_lightly_edited_quote_is_found_with_edits() {
        let quote = quote_in(
            "The resume path reads the whole event log on every call.",
            "reads the whole event log",
            0,
        );
        let edited =
            LogicalText::of_markdown("The resume path read the entire event log on every call.");
        let found = anchor(&edited, &quote);
        let Anchor::Moved {
            range,
            exact,
            score,
        } = found
        else {
            panic!("expected a moved anchor, got {found:?}")
        };
        assert!(!exact);
        assert!(score > 0.8, "{score}");
        assert_eq!(edited.slice(range), "read the entire event log");
    }

    #[test]
    fn a_long_edited_quote_in_a_long_page_is_found() {
        let filler: String = (0..600)
            .map(|n| format!("Sentence {n} says something about the shell. "))
            .collect();
        let passage =
            "The outbox tails the agent event log and delivers each reply once. ".repeat(8);
        let page = format!("{filler}{passage}{filler}");
        let quote = quote_in(&page, &passage, 0);
        let edited_passage = passage.replace("delivers", "hands over");
        let edited = LogicalText::of_markdown(&format!("{filler}{edited_passage}{filler}"));
        let found = anchor(&edited, &quote).range().unwrap();
        assert_eq!(found.start, filler.chars().count());
    }

    #[test]
    fn a_rewritten_passage_is_orphaned() {
        let quote = quote_in("The helper runs nono first.", "helper runs nono", 0);
        let edited = LogicalText::of_markdown("Completely different text about terminals.");
        assert_eq!(anchor(&edited, &quote), Anchor::Orphaned);
    }
}
