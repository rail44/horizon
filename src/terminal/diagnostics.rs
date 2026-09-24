//! Text and color dump used by isolated GUI verification.

use horizon_terminal_core::TerminalFrame;

/// Plain text plus a per-line span/color table (logical colors and style
/// bits as parsed, cursor position/shape, semantic selection) — the
/// headless half of visual verification; actual pixel output still needs
/// eyes on the window. Style attributes print only when set, keeping the
/// common unstyled line identical to the pre-v7 dump shape.
pub(super) fn dump_frame(frame: &TerminalFrame) -> String {
    use std::fmt::Write as _;

    let mut out = frame.text();
    out.push_str("\n--- spans ---\n");
    if let Some(cursor) = frame.cursor {
        let _ = writeln!(
            out,
            "cursor: row={} col={} shape={:?}",
            cursor.row, cursor.col, cursor.shape
        );
    }
    if let Some(selection) = frame.selection {
        let _ = writeln!(
            out,
            "selection: start=({},{}) end=({},{})",
            selection.start.row, selection.start.col, selection.end.row, selection.end.col
        );
    }
    for (row, line) in frame.lines.iter().enumerate() {
        for span in &line.spans {
            if span.text.trim().is_empty() {
                continue;
            }
            let _ = write!(
                out,
                "row {row}: {:?} fg={:?} bg={:?}",
                span.text, span.fg, span.bg
            );
            if span.italic {
                let _ = write!(out, " italic");
            }
            if span.strikethrough {
                let _ = write!(out, " strikethrough");
            }
            if span.underline != horizon_terminal_core::TerminalUnderline::None {
                let _ = write!(out, " underline={:?}", span.underline);
                if let Some(color) = span.underline_color {
                    let _ = write!(out, " underline_color={color:?}");
                }
            }
            if let Some(url) = &span.url {
                let _ = write!(out, " url={url}");
            }
            let _ = writeln!(out);
        }
    }
    out
}
