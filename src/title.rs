//! Content-derived session titles: the shared sanitizer/clamp both the
//! terminal (OSC 0/2 title, `TerminalUpdate::Title`) and the agent (first
//! user message) run their raw source text through before reporting a
//! derived title to the shell (`wire_session_title_updates` ->
//! `Workspace::set_session_derived_title`). Kept free-standing and GPUI-free
//! so the shaping rules stay unit-testable without a `Context`.

/// A derived title's longest length, in chars, ellipsis included. Long
/// enough for a cwd basename or a first sentence, short enough for a tab
/// label (the tab strip additionally elides at the layout level, but the
/// model-side clamp also bounds what gets persisted).
pub(crate) const MAX_TITLE_CHARS: usize = 40;

/// Sanitizes `raw` into a tab-title-shaped string: control characters
/// become spaces, whitespace runs collapse, and an over-long result is
/// clamped to [`MAX_TITLE_CHARS`] with an ellipsis. `None` when nothing
/// survives (an empty or blank title) -- callers treat that as "no derived
/// title", never as an empty replacement.
pub(crate) fn derive_session_title(raw: &str) -> Option<String> {
    let collapsed = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        return Some(collapsed);
    }
    let kept = collapsed
        .chars()
        .take(MAX_TITLE_CHARS - 1)
        .collect::<String>();
    Some(format!("{kept}…"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_whitespace_collapsed_and_control_stripped() {
        assert_eq!(
            derive_session_title("  my \t dir \n"),
            Some("my dir".to_string())
        );
        assert_eq!(derive_session_title("a\u{0}b"), Some("a b".to_string()));
    }

    #[test]
    fn an_empty_or_blank_title_yields_none() {
        assert_eq!(derive_session_title(""), None);
        assert_eq!(derive_session_title("   \t "), None);
        // A BEL alone collapses to a space, i.e. no title at all.
        assert_eq!(derive_session_title("\u{7}"), None);
    }

    #[test]
    fn a_long_title_is_clamped_with_an_ellipsis() {
        let long = "x".repeat(MAX_TITLE_CHARS + 10);
        let derived = derive_session_title(&long).expect("a non-empty title");
        assert_eq!(derived.chars().count(), MAX_TITLE_CHARS);
        assert!(derived.ends_with('…'));
    }

    #[test]
    fn multibyte_titles_clamp_by_chars_not_bytes() {
        // Japanese test data is deliberate (repo convention): the clamp
        // must count chars, not bytes, or CJK titles would truncate to a
        // third of the intended length.
        let source = "日本語のタイトルです";
        let derived = derive_session_title(source).expect("a non-empty title");
        assert_eq!(derived.chars().count(), source.chars().count());
    }
}
