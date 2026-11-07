//! What the Markdown view reads out of a file before drawing it: the
//! front matter split from the body, and where a link in the body points.

use std::path::{Path, PathBuf};

/// A file's source, split into its front matter and the Markdown body.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Document {
    /// The front matter's `title`, drawn as the document's heading.
    pub(crate) title: Option<String>,
    /// Every other top-level scalar of the front matter, in file order.
    pub(crate) fields: Vec<(String, String)>,
    /// The Markdown after the front matter.
    pub(crate) body: String,
}

/// Splits a leading YAML front matter block (`---` … `---`) off `source`.
///
/// Only top-level `key: value` scalars are read: a nested map, a list, or a
/// block scalar has no one-line rendering, so those lines are skipped
/// rather than shown half-parsed. Without a closing fence the whole source
/// is the body, as a Markdown renderer would treat it.
pub(crate) fn parse(source: &str) -> Document {
    let Some((front, body)) = split_front_matter(source) else {
        return Document {
            body: source.to_string(),
            ..Document::default()
        };
    };
    let mut title = None;
    let mut fields = Vec::new();
    for line in front.lines() {
        if line.starts_with([' ', '\t', '#', '-']) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = unquote(value.trim());
        if key.is_empty() || value.is_empty() || value == "|" || value == ">" {
            continue;
        }
        if key == "title" && title.is_none() {
            title = Some(value.to_string());
        } else {
            fields.push((key.to_string(), value.to_string()));
        }
    }
    Document {
        title,
        fields,
        body: body.to_string(),
    }
}

fn split_front_matter(source: &str) -> Option<(&str, &str)> {
    let rest = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == "---" || bare == "..." {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Where a clicked link goes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LinkTarget {
    /// Another Markdown file, opened in the same pane.
    Document(PathBuf),
    /// A URL with a scheme, handed to the desktop.
    External(String),
    /// A link the view does not follow: an in-page anchor, or a local file
    /// that is not Markdown.
    Unsupported,
}

/// Resolves `href` from a link in the document at `current`. A relative
/// path is taken from the document's own directory, the way a Markdown
/// renderer on a file system resolves it.
pub(crate) fn resolve_link(current: &Path, href: &str) -> LinkTarget {
    if href.starts_with('#') {
        return LinkTarget::Unsupported;
    }
    if has_scheme(href) {
        return LinkTarget::External(href.to_string());
    }
    let path = href.split(['#', '?']).next().unwrap_or_default();
    if !is_markdown(Path::new(path)) {
        return LinkTarget::Unsupported;
    }
    let base = current.parent().unwrap_or_else(|| Path::new(""));
    LinkTarget::Document(normalize(&base.join(path)))
}

/// `scheme:` per RFC 3986: a letter, then letters, digits, `+`, `-`, `.`.
/// A one-letter scheme is rejected so a Windows drive path is not taken
/// for a URL.
fn has_scheme(href: &str) -> bool {
    let Some((scheme, _)) = href.split_once(':') else {
        return false;
    };
    scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether `path` names a Markdown file, by its extension.
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

/// Folds `.` and `..` lexically, so the same file reached by two relative
/// links is one path (the history and the pane's persisted document
/// compare paths).
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_is_split_from_the_body_and_its_title_taken_out() {
        let doc = parse(
            "---\nconcept: sandbox\ntitle: \"Sandbox\"\ncode_at: a3521c17 (local main)\n---\n\n# Sandbox\n",
        );
        assert_eq!(doc.title.as_deref(), Some("Sandbox"));
        assert_eq!(
            doc.fields,
            vec![
                ("concept".to_string(), "sandbox".to_string()),
                ("code_at".to_string(), "a3521c17 (local main)".to_string()),
            ]
        );
        assert_eq!(doc.body, "\n# Sandbox\n");
    }

    #[test]
    fn nested_and_block_values_are_skipped_rather_than_half_shown() {
        let doc = parse("---\ntags:\n  - a\n  - b\nnote: |\n  text\nkind: page\n---\nbody");
        assert_eq!(doc.fields, vec![("kind".to_string(), "page".to_string())]);
        assert_eq!(doc.body, "body");
    }

    #[test]
    fn a_source_without_a_closed_front_matter_is_all_body() {
        assert_eq!(parse("# Title\n").body, "# Title\n");
        let unclosed = "---\ntitle: x\n# no fence\n";
        let doc = parse(unclosed);
        assert_eq!(doc.title, None);
        assert_eq!(doc.body, unclosed);
    }

    #[test]
    fn crlf_front_matter_is_split_too() {
        let doc = parse("---\r\ntitle: T\r\n---\r\nbody\r\n");
        assert_eq!(doc.title.as_deref(), Some("T"));
        assert_eq!(doc.body, "body\r\n");
    }

    #[test]
    fn a_relative_markdown_link_resolves_from_the_documents_directory() {
        let current = Path::new("/wiki/project/index.md");
        assert_eq!(
            resolve_link(current, "approval.md"),
            LinkTarget::Document("/wiki/project/approval.md".into())
        );
        assert_eq!(
            resolve_link(current, "../other/./page.md#section"),
            LinkTarget::Document("/wiki/other/page.md".into())
        );
        assert_eq!(
            resolve_link(current, "/abs/README.MARKDOWN"),
            LinkTarget::Document("/abs/README.MARKDOWN".into())
        );
    }

    #[test]
    fn urls_go_to_the_desktop_and_anchors_and_other_files_are_not_followed() {
        let current = Path::new("/wiki/index.md");
        assert_eq!(
            resolve_link(current, "https://example.com/a.md"),
            LinkTarget::External("https://example.com/a.md".into())
        );
        assert_eq!(
            resolve_link(current, "mailto:someone@example.com"),
            LinkTarget::External("mailto:someone@example.com".into())
        );
        assert_eq!(resolve_link(current, "#entry"), LinkTarget::Unsupported);
        assert_eq!(resolve_link(current, "src/lib.rs"), LinkTarget::Unsupported);
        assert_eq!(
            resolve_link(current, "C:notes.md"),
            LinkTarget::Document("/wiki/C:notes.md".into())
        );
    }
}
