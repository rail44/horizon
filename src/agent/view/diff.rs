//! Line-level diff computation for the `fs.edit` tool block body
//! (`docs/agent-output-ui-design.md` decision 3/4). `fs.edit`'s `old_string`/
//! `new_string` are already the small, unique snippet the tool matched
//! against (not a whole-file before/after), so a plain line diff of the two
//! strings -- no hunk collapsing, no surrounding-file context -- is enough;
//! see `tools/fs/edit.rs` for the requirement that `old_string` be unique in
//! the file.

use similar::{ChangeTag, TextDiff};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum DiffLineKind {
    Added,
    Removed,
    Context,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DiffLine {
    pub(super) kind: DiffLineKind,
    pub(super) text: String,
}

/// A `(added, removed)` line count -- the `+N -M` diffstat shown in a
/// finished `fs.edit` block's header.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct DiffStat {
    pub(super) added: usize,
    pub(super) removed: usize,
}

pub(super) fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|change| {
            let kind = match change.tag() {
                ChangeTag::Insert => DiffLineKind::Added,
                ChangeTag::Delete => DiffLineKind::Removed,
                ChangeTag::Equal => DiffLineKind::Context,
            };
            DiffLine {
                kind,
                // `similar`'s line changes keep their trailing newline in
                // `value()`; the view renders one line per `DiffLine`, so
                // trim it here rather than in every render call site.
                text: change.value().trim_end_matches('\n').to_string(),
            }
        })
        .collect()
}

pub(super) fn diff_stat(lines: &[DiffLine]) -> DiffStat {
    lines.iter().fold(DiffStat::default(), |mut stat, line| {
        match line.kind {
            DiffLineKind::Added => stat.added += 1,
            DiffLineKind::Removed => stat.removed += 1,
            DiffLineKind::Context => {}
        }
        stat
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_diff_reports_pure_addition() {
        let lines = line_diff("a\nb\n", "a\nb\nc\n");

        assert_eq!(
            lines,
            vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: "a".to_string()
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    text: "b".to_string()
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    text: "c".to_string()
                },
            ]
        );
        assert_eq!(
            diff_stat(&lines),
            DiffStat {
                added: 1,
                removed: 0
            }
        );
    }

    #[test]
    fn line_diff_reports_pure_removal() {
        let lines = line_diff("a\nb\nc\n", "a\nb\n");

        assert_eq!(
            diff_stat(&lines),
            DiffStat {
                added: 0,
                removed: 1
            }
        );
    }

    #[test]
    fn line_diff_reports_mixed_changes() {
        let lines = line_diff("one\ntwo\nthree\n", "one\ntwo-changed\nthree\nfour\n");

        assert_eq!(
            diff_stat(&lines),
            DiffStat {
                added: 2,
                removed: 1
            }
        );
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Removed && line.text == "two"));
        assert!(lines
            .iter()
            .any(|line| line.kind == DiffLineKind::Added && line.text == "two-changed"));
    }

    #[test]
    fn line_diff_treats_entirely_new_content_as_all_added() {
        let lines = line_diff("", "brand new file\nsecond line\n");

        assert!(lines.iter().all(|line| line.kind == DiffLineKind::Added));
        assert_eq!(
            diff_stat(&lines),
            DiffStat {
                added: 2,
                removed: 0
            }
        );
    }
}
