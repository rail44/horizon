#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct MarkdownLine {
    pub(super) index: usize,
    pub(super) text: String,
    pub(super) kind: MarkdownLineKind,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum MarkdownLineKind {
    Heading,
    Paragraph,
    Bullet,
    Code,
    Blank,
}

pub(super) fn markdown_lines(text: &str) -> Vec<MarkdownLine> {
    let mut in_code = false;
    let mut lines = Vec::new();

    for (index, raw_line) in text.lines().enumerate() {
        let trimmed_end = raw_line.trim_end();
        let trimmed = trimmed_end.trim_start();

        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }

        let (kind, text) = if in_code {
            (MarkdownLineKind::Code, trimmed_end.to_string())
        } else if trimmed.is_empty() {
            (MarkdownLineKind::Blank, String::new())
        } else if trimmed.starts_with('#') {
            (
                MarkdownLineKind::Heading,
                trimmed.trim_start_matches('#').trim_start().to_string(),
            )
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            (
                MarkdownLineKind::Bullet,
                format!("- {}", strip_inline_markers(rest)),
            )
        } else {
            (
                MarkdownLineKind::Paragraph,
                strip_inline_markers(trimmed_end),
            )
        };

        lines.push(MarkdownLine { index, text, kind });
    }

    if lines.is_empty() {
        lines.push(MarkdownLine {
            index: 0,
            text: String::new(),
            kind: MarkdownLineKind::Blank,
        });
    }

    lines
}

fn strip_inline_markers(text: &str) -> String {
    text.replace("**", "").replace("__", "").replace('`', "")
}
