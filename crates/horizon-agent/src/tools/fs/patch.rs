//! What a call changed in a file, as Git's patch text (`--patch`) with
//! absolute paths and no `a/`/`b/` prefixes: the form ACP's `git_patch`
//! diffs take. The tools that write files make it from what they know of
//! the change -- an edit the spans it replaced, a write the content before
//! and after -- with no diff algorithm.

use std::ops::Range;
use std::path::Path;

/// Lines of context around each change.
const CONTEXT: usize = 3;

/// A span of the file before and the text put in its place.
type Replacement<'a> = (Range<usize>, &'a str);

/// One change at line level: `old_count` lines of the file before, from
/// line `old_start` (counted from 0), replaced by `new`.
#[derive(Debug, PartialEq, Eq)]
struct LineChange {
    old_start: usize,
    old_count: usize,
    new: Vec<String>,
}

/// A file's lines: its text split at `\n`, without the empty piece after a
/// final newline.
fn lines_of(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.is_empty() || text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// The patch of an edit of `path`: `before` (with `\n` line endings) with
/// each of `replacements` -- spans of it, not overlapping -- replaced by
/// its text. `None` when nothing changed.
pub(super) fn edited(
    path: &Path,
    before: &str,
    replacements: &[(Range<usize>, &str)],
) -> Option<String> {
    let mut replacements: Vec<&(Range<usize>, &str)> = replacements.iter().collect();
    replacements.sort_by_key(|(span, _)| span.start);
    // Replacements whose lines touch are one change of those lines.
    let mut groups: Vec<(Range<usize>, Vec<&Replacement>)> = Vec::new();
    for replacement in replacements {
        let lines = lines_around(before, replacement.0.clone());
        match groups.last_mut() {
            Some((block, members)) if lines.start <= block.end => {
                block.end = block.end.max(lines.end);
                members.push(replacement);
            }
            _ => groups.push((lines, vec![replacement])),
        }
    }
    let changes: Vec<LineChange> = (groups.into_iter())
        .filter_map(|(block, members)| line_change(before, block, &members))
        .collect();
    file_patch(path, &lines_of(before), &changes, false)
}

/// The bytes of the whole lines `span` touches, without the newline that
/// ends the last.
fn lines_around(before: &str, span: Range<usize>) -> Range<usize> {
    let start = before[..span.start].rfind('\n').map_or(0, |at| at + 1);
    let end = before[span.end..]
        .find('\n')
        .map_or(before.len(), |at| span.end + at);
    start..end
}

/// The patch of writing `after` to `path`, which held `before` (`None`:
/// the write created it). The lines both share at the start and the end
/// are kept; what is between is replaced whole.
pub(super) fn written(path: &Path, before: Option<&str>, after: &str) -> Option<String> {
    let old = lines_of(before.unwrap_or_default());
    let new = lines_of(after);
    let prefix = (old.iter().zip(&new)).take_while(|(a, b)| a == b).count();
    let suffix = (old[prefix..].iter().rev())
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let change = LineChange {
        old_start: prefix,
        old_count: old.len() - prefix - suffix,
        new: (new[prefix..new.len() - suffix].iter())
            .map(|line| line.to_string())
            .collect(),
    };
    let changes = match change.old_count == 0 && change.new.is_empty() {
        true => Vec::new(),
        false => vec![change],
    };
    file_patch(path, &old, &changes, before.is_none())
}

/// The replacements within `block`, whole lines of `before`, as a change
/// of those lines, with the lines the old and new text share at either end
/// kept out of it.
fn line_change(
    before: &str,
    block: Range<usize>,
    replacements: &[&(Range<usize>, &str)],
) -> Option<LineChange> {
    let block_start = block.start;
    let appended = block_start == before.len();
    let old_text = &before[block.clone()];
    let mut new_text = String::new();
    let mut at = block.start;
    for (span, text) in replacements {
        new_text.push_str(&before[at..span.start]);
        new_text.push_str(text);
        at = span.end;
    }
    new_text.push_str(&before[at..block.end]);
    // Text added after a final newline (or to an empty file) has no line
    // of the file around it.
    let (old, new): (Vec<&str>, Vec<&str>) = match appended {
        true => (Vec::new(), lines_of(&new_text)),
        false => (
            old_text.split('\n').collect(),
            new_text.split('\n').collect(),
        ),
    };
    let old_start = before[..block_start].matches('\n').count();
    let prefix = (old.iter().zip(&new)).take_while(|(a, b)| a == b).count();
    let suffix = (old[prefix..].iter().rev())
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_count = old.len() - prefix - suffix;
    let new: Vec<String> = (new[prefix..new.len() - suffix].iter())
        .map(|line| line.to_string())
        .collect();
    (old_count > 0 || !new.is_empty()).then_some(LineChange {
        old_start: old_start + prefix,
        old_count,
        new,
    })
}

/// A hunk header's range: its first line counted from 1, or the line
/// before it when it holds none, as Git writes it.
fn range(start: usize, count: usize) -> String {
    match count {
        0 => format!("{start},0"),
        _ => format!("{},{count}", start + 1),
    }
}

/// `path`'s section of a patch: its header, then a hunk for each run of
/// `changes` (in order, not overlapping) whose context touches.
fn file_patch(path: &Path, old: &[&str], changes: &[LineChange], created: bool) -> Option<String> {
    if changes.is_empty() {
        return None;
    }
    let path = path.display();
    let mut out = format!("diff --git {path} {path}\n");
    match created {
        true => out.push_str(&format!(
            "new file mode 100644\n--- /dev/null\n+++ {path}\n"
        )),
        false => out.push_str(&format!("--- {path}\n+++ {path}\n")),
    }
    // How many lines the changes before a hunk added, less those they took.
    let mut shift: isize = 0;
    let mut at = 0;
    while at < changes.len() {
        // The changes whose context runs together form one hunk.
        let mut until = at + 1;
        while until < changes.len() {
            let end = changes[until - 1].old_start + changes[until - 1].old_count;
            if changes[until].old_start > end + 2 * CONTEXT {
                break;
            }
            until += 1;
        }
        let group = &changes[at..until];
        let first = &group[0];
        let last = &group[group.len() - 1];
        let start = first.old_start.saturating_sub(CONTEXT);
        let end = (last.old_start + last.old_count + CONTEXT).min(old.len());
        let mut body = String::new();
        let (mut old_count, mut new_count) = (0, 0);
        let mut line = start;
        for change in group {
            for context in &old[line..change.old_start] {
                body.push_str(&format!(" {context}\n"));
            }
            old_count += change.old_start - line;
            new_count += change.old_start - line;
            for removed in &old[change.old_start..change.old_start + change.old_count] {
                body.push_str(&format!("-{removed}\n"));
            }
            for added in &change.new {
                body.push_str(&format!("+{added}\n"));
            }
            old_count += change.old_count;
            new_count += change.new.len();
            line = change.old_start + change.old_count;
        }
        for context in &old[line..end] {
            body.push_str(&format!(" {context}\n"));
        }
        old_count += end - line;
        new_count += end - line;
        let new_start = (start as isize + shift).max(0) as usize;
        out.push_str(&format!(
            "@@ -{} +{} @@\n{body}",
            range(start, old_count),
            range(new_start, new_count)
        ));
        shift += (group.iter())
            .map(|change| change.new.len() as isize - change.old_count as isize)
            .sum::<isize>();
        at = until;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> &'static Path {
        Path::new("/w/a.txt")
    }

    #[test]
    fn an_edit_is_a_hunk_with_its_context() {
        let before = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let at = before.find("5").unwrap();
        let patch = edited(path(), before, &[(at..at + 1, "five")]).unwrap();
        assert_eq!(
            patch,
            "diff --git /w/a.txt /w/a.txt\n--- /w/a.txt\n+++ /w/a.txt\n\
             @@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n"
        );
    }

    #[test]
    fn edits_far_apart_are_hunks_of_their_own_and_the_second_is_shifted() {
        let before: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let two = before.find("2\n").unwrap();
        let eighteen = before.find("18\n").unwrap();
        let patch = edited(
            path(),
            &before,
            &[
                (eighteen..eighteen + 2, "eighteen"),
                (two..two + 1, "two\ntwo"),
            ],
        )
        .unwrap();
        assert!(
            patch.contains("@@ -1,5 +1,6 @@\n 1\n-2\n+two\n+two\n 3\n 4\n 5\n"),
            "{patch}"
        );
        assert!(
            patch.contains("@@ -15,6 +16,6 @@\n 15\n 16\n 17\n-18\n+eighteen\n 19\n 20\n"),
            "{patch}"
        );
    }

    #[test]
    fn edits_near_each_other_share_one_hunk() {
        let before = "a\nb\nc\nd\ne\n";
        let b = before.find('b').unwrap();
        let d = before.find('d').unwrap();
        let patch = edited(path(), before, &[(b..b + 1, "B"), (d..d + 1, "D")]).unwrap();
        assert!(
            patch.ends_with("@@ -1,5 +1,5 @@\n a\n-b\n+B\n c\n-d\n+D\n e\n"),
            "{patch}"
        );
    }

    #[test]
    fn a_span_inside_a_line_keeps_the_rest_of_the_line() {
        let before = "let x = 1;\n";
        let at = before.find('1').unwrap();
        let patch = edited(path(), before, &[(at..at + 1, "2")]).unwrap();
        assert!(
            patch.ends_with("@@ -1,1 +1,1 @@\n-let x = 1;\n+let x = 2;\n"),
            "{patch}"
        );
    }

    #[test]
    fn text_appended_after_the_last_line_is_added_lines() {
        let before = "a\n";
        let patch = edited(path(), before, &[(2..2, "b\nc\n")]).unwrap();
        assert!(patch.ends_with("@@ -1,1 +1,3 @@\n a\n+b\n+c\n"), "{patch}");
    }

    #[test]
    fn replacements_on_one_line_are_one_change() {
        let before = "foo foo\nbar\n";
        let patch = edited(path(), before, &[(0..3, "x"), (4..7, "x")]).unwrap();
        assert!(
            patch.ends_with("@@ -1,2 +1,2 @@\n-foo foo\n+x x\n bar\n"),
            "{patch}"
        );
    }

    #[test]
    fn a_created_file_is_all_added() {
        let patch = written(path(), None, "x\ny\n").unwrap();
        assert_eq!(
            patch,
            "diff --git /w/a.txt /w/a.txt\nnew file mode 100644\n--- /dev/null\n+++ /w/a.txt\n\
             @@ -0,0 +1,2 @@\n+x\n+y\n"
        );
    }

    #[test]
    fn an_overwrite_replaces_what_lies_between_the_shared_ends() {
        let patch = written(path(), Some("a\nb\nc\n"), "a\nB\nc\n").unwrap();
        assert!(
            patch.ends_with("@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"),
            "{patch}"
        );
        assert_eq!(written(path(), Some("same\n"), "same\n"), None);
    }

    #[test]
    fn multibyte_text_is_kept_whole() {
        let before = "日本語\n";
        let at = before.find("本").unwrap();
        let patch = edited(path(), before, &[(at..at + "本".len(), "ほん")]).unwrap();
        assert!(patch.ends_with("-日本語\n+日ほん語\n"), "{patch}");
    }
}
