//! Reconstructed line diffs (`fs.edit`'s body/chip diffstat).

/// One line of a reconstructed diff body (stage D's fs.edit expansion,
/// `docs/agent-output-ui-design.md` decision 4): `Context` lines are the
/// common prefix/suffix trimmed below, painted with neither role;
/// `Added`/`Removed` pair with `theme::diff_added_*`/`diff_removed_*` in
/// the view (line background carries the change, sign column colored
/// separately).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Reconstructs a full line diff between `old` and `new` by trimming the
/// common prefix/suffix (kept as `Context` lines) and pairing the
/// remaining middle as removed-then-added -- not a full diff algorithm
/// (no interior-line matching), matching one `fs.edit` entry's
/// old_string/new_string replacement shape. Operates on `&str` lines
/// throughout, so multibyte content (e.g. Japanese text) round-trips
/// unmodified -- no byte-level slicing here.
pub fn reconstruct_line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let mut lines = Vec::new();
    for text in &old_lines[..prefix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            text: (*text).to_string(),
        });
    }
    for text in &old_lines[prefix..old_lines.len() - suffix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Removed,
            text: (*text).to_string(),
        });
    }
    for text in &new_lines[prefix..new_lines.len() - suffix] {
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: (*text).to_string(),
        });
    }
    for text in &old_lines[old_lines.len() - suffix..] {
        lines.push(DiffLine {
            kind: DiffLineKind::Context,
            text: (*text).to_string(),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn reconstruct_line_diff_handles_a_pure_insert() {
        let lines = reconstruct_line_diff("a\nb", "a\nnew\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Added, "new"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_handles_a_pure_delete() {
        let lines = reconstruct_line_diff("a\nold\nb", "a\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "old"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_handles_a_mixed_change() {
        let lines = reconstruct_line_diff("a\nold1\nold2\nb", "a\nnew1\nb");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Removed, "old1"),
                (DiffLineKind::Removed, "old2"),
                (DiffLineKind::Added, "new1"),
                (DiffLineKind::Context, "b"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_of_identical_strings_is_all_context() {
        let lines = reconstruct_line_diff("a\nb\nc", "a\nb\nc");
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "a"),
                (DiffLineKind::Context, "b"),
                (DiffLineKind::Context, "c"),
            ]
        );
    }

    #[test]
    fn reconstruct_line_diff_round_trips_multibyte_content() {
        let lines = reconstruct_line_diff(
            "こんにちは\n古い行\nさようなら",
            "こんにちは\n新しい行\nさようなら",
        );
        assert_eq!(
            diff_texts(&lines),
            vec![
                (DiffLineKind::Context, "こんにちは"),
                (DiffLineKind::Removed, "古い行"),
                (DiffLineKind::Added, "新しい行"),
                (DiffLineKind::Context, "さようなら"),
            ]
        );
    }
}
