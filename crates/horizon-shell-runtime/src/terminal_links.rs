//! The links a terminal's rows carry: which one is at a cell, and which a
//! click may open. Both shells' terminal views use them.

/// The OSC 8 URI of the span covering `col` on `row`, if any — the
/// click-target lookup behind a modified click on a link.
/// `row` indexes the *painted* line list (live frame rows, or a held
/// scrollback window's served slice); `col` is a 0-based cell column.
/// Spans tile the row by `columns`, so a single column walk answers.
pub fn url_from_lines(
    lines: &[horizon_terminal_core::TerminalLine],
    row: usize,
    col: usize,
) -> Option<String> {
    let line = lines.get(row)?;
    let mut start = 0usize;
    for span in &line.spans {
        let end = start.saturating_add(span.columns);
        if (start..end).contains(&col) {
            return span.url.clone();
        }
        start = end;
    }
    None
}

/// The open-with-the-OS allow-list for terminal hyperlinks. Only http(s):
/// is opened: OSC 8 lets the *remote* side name arbitrary URIs
/// (`file://`, custom app schemes), and the terminal must not become a
/// remote code-execution primitive by proxy.
pub fn is_openable_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}
