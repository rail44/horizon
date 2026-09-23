//! The persisted knowledge document format, independent of storage and tool execution.

/// An entry's publication status. Only `Active` entries appear in the
/// always-loaded prompt index; `knowledge.read` returns any status.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum KnowledgeStatus {
    Active,
    NeedsReview,
    Expired,
}

impl KnowledgeStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::NeedsReview => "needs-review",
            Self::Expired => "expired",
        }
    }
}

pub(super) fn parse_status(value: &str) -> Option<KnowledgeStatus> {
    match value {
        "active" => Some(KnowledgeStatus::Active),
        "needs-review" => Some(KnowledgeStatus::NeedsReview),
        "expired" => Some(KnowledgeStatus::Expired),
        _ => None,
    }
}

/// A parsed knowledge entry — frontmatter fields plus the Markdown body.
/// `sources` is required (at least one); the parser rejects an entry
/// whose `sources` is empty or missing.
#[derive(Clone, Debug)]
pub(super) struct ParsedKnowledgeMd {
    pub(super) id: String,
    pub(super) description: String,
    pub(super) anchors: Vec<String>,
    pub(super) sources: Vec<String>,
    pub(super) created: String,
    pub(super) updated: String,
    pub(super) status: KnowledgeStatus,
    pub(super) body: String,
}

/// Whether `id` is a valid slug: non-empty, lowercase ASCII alphanumeric
/// and hyphens only.
pub(super) fn is_valid_slug(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Strips surrounding double or single quotes from `s`, if present.
fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parses an inline YAML array `[item1, item2, ...]`, stripping
/// surrounding quotes from each item. Empty brackets yield an empty
/// vec.
fn parse_inline_array(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|item| strip_quotes(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

/// Hand-parses a knowledge entry's frontmatter (same `---`-delimited
/// shape as `skills::parse_skill_md`, extended with `anchors`/`sources`
/// arrays and `created`/`updated`/`status` fields). Returns `None` for
/// anything that doesn't match the expected shape (missing delimiters,
/// a required field missing, an unrecognized `status`, or an empty
/// `sources` list). Both inline arrays (`key: [a, b]`) and block arrays
/// (`key:\n  - a\n  - b`) are accepted.
pub(super) fn parse_knowledge_md(source: &str) -> Option<ParsedKnowledgeMd> {
    let after_open = source.strip_prefix("---\n")?;
    let (frontmatter, body) = after_open.split_once("\n---\n")?;

    let mut id = None;
    let mut description = None;
    let mut anchors = Vec::new();
    let mut sources = Vec::new();
    let mut created = None;
    let mut updated = None;
    let mut status = None;

    enum CurrentArray {
        Anchors,
        Sources,
    }
    let mut current: Option<CurrentArray> = None;

    for line in frontmatter.lines() {
        // Block-array item: `  - value`
        if let Some(rest) = line.strip_prefix("  - ") {
            let item = strip_quotes(rest.trim());
            match &current {
                Some(CurrentArray::Anchors) => anchors.push(item),
                Some(CurrentArray::Sources) => sources.push(item),
                None => {}
            }
            continue;
        }

        current = None;

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "id" => id = Some(value.to_string()),
            "description" => description = Some(value.to_string()),
            "created" => created = Some(value.to_string()),
            "updated" => updated = Some(value.to_string()),
            "status" => status = parse_status(value),
            "anchors" => {
                if value.is_empty() {
                    current = Some(CurrentArray::Anchors);
                } else {
                    anchors = parse_inline_array(value);
                }
            }
            "sources" => {
                if value.is_empty() {
                    current = Some(CurrentArray::Sources);
                } else {
                    sources = parse_inline_array(value);
                }
            }
            _ => {}
        }
    }

    // `sources` is required and must be non-empty.
    if sources.is_empty() {
        return None;
    }

    Some(ParsedKnowledgeMd {
        id: id?,
        description: description?,
        anchors,
        sources,
        created: created?,
        updated: updated?,
        status: status?,
        body: body.trim_start_matches('\n').to_string(),
    })
}

/// Serializes an entry back to the `---`-delimited file format. Used by
/// `knowledge.write` to persist an upserted entry.
pub(super) fn serialize_entry(entry: &ParsedKnowledgeMd) -> String {
    fn fmt_array(items: &[String]) -> String {
        if items.is_empty() {
            return "[]".to_string();
        }
        let quoted: Vec<String> = items
            .iter()
            .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        format!("[{}]", quoted.join(", "))
    }

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", entry.id));
    out.push_str(&format!("description: {}\n", entry.description));
    out.push_str(&format!("anchors: {}\n", fmt_array(&entry.anchors)));
    out.push_str(&format!("sources: {}\n", fmt_array(&entry.sources)));
    out.push_str(&format!("created: {}\n", entry.created));
    out.push_str(&format!("updated: {}\n", entry.updated));
    out.push_str(&format!("status: {}\n", entry.status.as_str()));
    out.push_str("---\n");
    out.push_str(&entry.body);
    out
}
