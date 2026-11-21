use termwiz::input::{KeyCode, Modifiers};

use super::*;
use crate::core::TerminalCore;
use serde::{Serialize, de::DeserializeOwned};
use std::fmt::Debug;

#[test]
fn vt_stream_updates_snapshot() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"hello\r\n\x1b[31mred\x1b[0m");

    let snapshot = core.snapshot_text();
    assert!(snapshot.contains("hello"));
    assert!(snapshot.contains("red"));
}

/// `docs/session-daemon-design.md` decision 8: a cell's color is a logical
/// value (here, `NamedColor::Red`/`NamedColor::Foreground`), not a resolved
/// RGB triple — resolving against a theme is the host's job now
/// (`theme::resolve`, `src/theme/ansi.rs`).
#[test]
fn vt_stream_preserves_ansi_foreground_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[31mred\x1b[0m plain");

    let frame = core.snapshot_frame();
    assert!(frame.text().contains("red plain"));
    let first_line = &frame.lines[0];
    assert!(
        first_line
            .spans()
            .any(|span| { span.text == "red" && span.fg == TerminalColor::Named(NamedColor::Red) })
    );
    assert!(first_line.spans().any(|span| {
        span.text == "plain" && span.fg == TerminalColor::Named(NamedColor::Foreground)
    }));
}

/// `docs/session-daemon-design.md` decision 8: an app's live OSC 4 palette
/// override now rides the frame as `TerminalFrame::palette_overrides`, a
/// sparse index→RGB table the host consults at paint time (the host half
/// is `theme::resolve`, `src/theme/ansi.rs`).
#[test]
fn osc_4_palette_override_is_captured_in_the_frame() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b]4;1;rgb:00/ff/00\x1b\\");

    let frame = core.snapshot_frame();
    assert_eq!(
        frame
            .palette_overrides
            .iter()
            .find(|(index, _)| *index == 1),
        Some(&(1, [0, 255, 0]))
    );
}

/// OSC 10 (set foreground) writes `NamedColor::Foreground as usize == 256`
/// in `Term::colors()`, so the override shows up at index 256.
#[test]
fn osc_10_foreground_override_lands_at_index_256() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b]10;rgb:11/22/33\x07");

    let frame = core.snapshot_frame();
    assert_eq!(
        frame
            .palette_overrides
            .iter()
            .find(|(index, _)| *index == 256),
        Some(&(256, [17, 34, 51]))
    );
}

/// OSC 104 (reset color) clears a previously-set slot back out of the
/// sparse table entirely, rather than leaving a stale entry behind.
#[test]
fn osc_104_reset_clears_a_previously_set_slot() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b]4;1;rgb:00/ff/00\x1b\\");
    assert!(
        core.snapshot_frame()
            .palette_overrides
            .iter()
            .any(|(index, _)| *index == 1)
    );

    core.write_vt(b"\x1b]104;1\x1b\\");
    assert!(
        !core
            .snapshot_frame()
            .palette_overrides
            .iter()
            .any(|(index, _)| *index == 1)
    );
}

/// Ground truth for the "gaps in bg-colored regions" dogfooding report
/// (owner report, 2026-07-18): `CSI K` (erase line) fills the erased
/// region with whatever background is *currently active* on the cursor's
/// SGR template -- the classic `bce` terminfo semantics real apps (an
/// ink/React-based CLI logo, a status line) rely on to paint solid-color
/// bars without emitting a literal colored space for every cell. That
/// erased region must still carry the active background into the frame
/// as a span -- `push_styled_cell` represents a run of blank cells as an
/// empty-`text` span carrying only `columns`/`bg`, distinct from (but
/// just as real as) a span with visible glyphs. The paint side's fix
/// (`src/terminal/mod.rs`'s `paint_terminal`) depends on this contract
/// holding, so it's locked down here independently of the paint code.
#[test]
fn bce_erased_line_carries_the_active_background_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[48;2;10;20;30mhi\x1b[K");

    let frame = core.snapshot_frame();
    let first_line = &frame.lines[0];
    // The erased cells have no text and the background: they are in the
    // run "hi" is in, which covers the whole row.
    let erased = first_line.run_at(19).expect("the erased tail has a run");
    assert_eq!(erased.style.bg, TerminalColor::Rgb([10, 20, 30]));
    assert_eq!(erased.columns, 0..20);
    assert_eq!(&first_line.text[erased.bytes.clone()], "hi");
}

/// The other route to the same span shape: literal space characters
/// written mid-line under an active background (a status line's padding
/// between segments is the real-world case -- see e.g. neovim's own
/// statusline, which pads with plain spaces under a highlight group's
/// background rather than erasing). Unlike the BCE case above, this run
/// is followed by more text, not the end of the line.
#[test]
fn mid_line_space_run_with_background_is_a_blank_colored_span() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[48;2;9;9;9mA   B\x1b[0m");

    let frame = core.snapshot_frame();
    let first_line = &frame.lines[0];
    // The spaces have no text, and the background of the run they are in.
    for column in 1..4 {
        let padding = first_line.run_at(column).expect("the spaces have a run");
        assert_eq!(padding.style.bg, TerminalColor::Rgb([9, 9, 9]));
    }
    assert_eq!(first_line.text, "AB");
    assert_eq!(first_line.cells, vec![(0, 0), (1, 4)]);
}

/// Goal 2 of `docs/terminal-protocol-goals.md`: selection is semantic
/// frame metadata (`TerminalFrame::selection`), not literal RGB baked into
/// span colors -- a selected frame's rows are byte-identical to the
/// unselected frame's, and only the `selection` field moves. (Before v7,
/// `snapshot_frame` overrode selected cells' `fg`/`bg` to a hardcoded
/// highlight, so every drag rewrote row content.)
#[test]
fn selection_is_frame_metadata_and_leaves_spans_untouched() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"hi");
    let unselected = core.snapshot_frame();
    assert_eq!(unselected.selection, None);

    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 0 },
        TerminalSelectionKind::Simple,
    );
    core.update_selection(TerminalSelectionPoint { row: 0, col: 19 });

    let selected = core.snapshot_frame();
    assert_eq!(selected.lines, unselected.lines);
    assert_eq!(
        selected.selection,
        Some(TerminalSelection {
            start: TerminalSelectionPoint { row: 0, col: 0 },
            end: TerminalSelectionPoint { row: 0, col: 19 },
        })
    );
}

/// `TerminalFrame::selection` is viewport-space and window-clamped: a
/// selection made on the live screen then scrolled out through history
/// browsing either clamps to the visible intersection (partially visible)
/// or drops to `None` (fully outside), never reporting an out-of-range
/// row.
#[test]
fn selection_is_clamped_to_the_viewport_or_dropped() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    // 12 numbered lines on a 4-row screen: rows 8..=11 are visible, the
    // rest is scrollback history above the live screen.
    for n in 0..12 {
        core.write_vt(format!("line{n}\r\n").as_bytes());
    }
    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 1 },
        TerminalSelectionKind::Simple,
    );
    core.update_selection(TerminalSelectionPoint { row: 1, col: 2 });
    assert_eq!(
        core.snapshot_frame().selection,
        Some(TerminalSelection {
            start: TerminalSelectionPoint { row: 0, col: 1 },
            end: TerminalSelectionPoint { row: 1, col: 2 },
        })
    );

    // Scroll one line back into history: the selection's rows shift down
    // by one and its tail row leaves the window, clamping the end to the
    // bottom edge's full-row boundary.
    core.scroll_display(3);
    assert_eq!(
        core.snapshot_frame().selection,
        Some(TerminalSelection {
            start: TerminalSelectionPoint { row: 3, col: 1 },
            end: TerminalSelectionPoint { row: 3, col: 19 },
        })
    );

    // Scroll far enough that the whole selection is below the viewport.
    core.scroll_display(5);
    assert_eq!(core.snapshot_frame().selection, None);
}

/// Backlog #44: SGR text styles reach the frame as span attributes.
/// Covers the plain forms (3/4/9) plus SGR 4's colon sub-parameter styles
/// (`4:2`..`4:5` -- `4:3` is nvim's undercurl probe) and reset (`4:0`).
#[test]
fn sgr_text_styles_reach_the_span() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 8));
    core.write_vt(
        b"\x1b[3mI\x1b[0m\r\n\
          \x1b[9mS\x1b[0m\r\n\
          \x1b[4mU\x1b[0m\r\n\
          \x1b[4:2mD\x1b[0m\r\n\
          \x1b[4:3mC\x1b[0m\r\n\
          \x1b[4:4mO\x1b[0m\r\n\
          \x1b[4:5mA\x1b[0m\r\n\
          \x1b[4mX\x1b[4:0mP",
    );

    let frame = core.snapshot_frame();
    let span = |row: usize, text: &str| {
        frame.lines[row]
            .spans()
            .find(|span| span.text == text)
            .unwrap_or_else(|| panic!("no span {text:?} on row {row}"))
    };

    assert!(span(0, "I").italic);
    assert!(!span(0, "I").strikethrough);
    assert!(span(1, "S").strikethrough);
    assert_eq!(span(2, "U").underline, TerminalUnderline::Single);
    assert_eq!(span(3, "D").underline, TerminalUnderline::Double);
    assert_eq!(span(4, "C").underline, TerminalUnderline::Curl);
    assert_eq!(span(5, "O").underline, TerminalUnderline::Dotted);
    assert_eq!(span(6, "A").underline, TerminalUnderline::Dashed);
    assert_eq!(span(7, "X").underline, TerminalUnderline::Single);
    assert_eq!(span(7, "P").underline, TerminalUnderline::None);
    // Style is part of the span-merge key: the underlined "X" and plain
    // "P" may not collapse into one span.
    assert!(frame.lines[7].runs.len() >= 2);
}

/// SGR 7 changes the places of a cell's two colors, whichever they are:
/// text written in the default colors is the background's color on the
/// foreground's, not the foreground's on itself.
#[test]
fn inverse_text_is_its_background_s_color_on_its_foreground_s() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 4));
    core.write_vt(b"\x1b[7mD\x1b[0m\r\n\x1b[7;31;44mC\x1b[0m\r\n\x1b[7;1mB\x1b[0m");

    let frame = core.snapshot_frame();
    let span = |row: usize, text: &str| {
        frame.lines[row]
            .spans()
            .find(|span| span.text == text)
            .unwrap_or_else(|| panic!("no span {text:?} on row {row}"))
    };
    let named = TerminalColor::Named;

    let plain = span(0, "D");
    assert_eq!(plain.fg, named(NamedColor::Background));
    assert_eq!(plain.bg, named(NamedColor::Foreground));
    let colored = span(1, "C");
    assert_eq!(colored.fg, named(NamedColor::Blue));
    assert_eq!(colored.bg, named(NamedColor::Red));
    // Bold's brighter color goes with the foreground to where it is drawn.
    let bold = span(2, "B");
    assert_eq!(bold.fg, named(NamedColor::Background));
    assert_eq!(bold.bg, named(NamedColor::BrightForeground));
}

/// SGR 58 (underline color) reaches the span in both its truecolor and
/// indexed forms, and is normalized away on a cell that is not underlined
/// (`TerminalSpan::underline_color`'s contract).
#[test]
fn sgr_58_underline_color_reaches_the_span() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 4));
    core.write_vt(
        b"\x1b[4:3m\x1b[58:2::255:10:20mR\x1b[0m\r\n\
          \x1b[4m\x1b[58:5:208mI\x1b[0m\r\n\
          \x1b[58:2::1:2:3mN",
    );

    let frame = core.snapshot_frame();
    let span = |row: usize, text: &str| {
        frame.lines[row]
            .spans()
            .find(|span| span.text == text)
            .unwrap_or_else(|| panic!("no span {text:?} on row {row}"))
    };

    let curl = span(0, "R");
    assert_eq!(curl.underline, TerminalUnderline::Curl);
    assert_eq!(
        curl.underline_color,
        Some(TerminalColor::Rgb([255, 10, 20]))
    );

    let indexed = span(1, "I");
    assert_eq!(indexed.underline, TerminalUnderline::Single);
    assert_eq!(indexed.underline_color, Some(TerminalColor::Indexed(208)));

    // No underline -> the SGR 58 color is presentation-dead and dropped.
    let plain = span(2, "N");
    assert_eq!(plain.underline, TerminalUnderline::None);
    assert_eq!(plain.underline_color, None);
}

/// Style attributes are part of `push_styled_cell`'s span-merge key: a
/// blank (space) run only extends the previous blank span while *every*
/// attribute matches, so an underlined blank run and a plain blank run
/// stay separate spans instead of silently collapsing into one.
#[test]
fn style_attributes_are_part_of_the_span_merge_key() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 4));
    // Two underlined spaces, then two plain spaces, then a marker.
    core.write_vt(b"\x1b[4m  \x1b[0m  X");

    let frame = core.snapshot_frame();
    let spans: Vec<_> = frame.lines[0].spans().collect();
    assert_eq!(spans[0].text, "");
    assert_eq!(spans[0].columns, 2);
    assert_eq!(spans[0].underline, TerminalUnderline::Single);
    // The plain spaces are in the run of the plain text after them.
    assert_eq!(spans[1].text, "X");
    assert_eq!(spans[1].underline, TerminalUnderline::None);
    assert!(!spans[1].italic);
    assert_eq!(frame.lines[0].cells, vec![(0, 4)]);
}

/// Goal 4 of `docs/terminal-protocol-goals.md`, first step: frame size is
/// proportional to *style runs*, not characters. Before run merging every
/// printable cell was its own span, so a full 80-column single-style row
/// cost 80 spans -- and the GUI pays one shape_line + scene-layer
/// insertion per span (the measured 31% of main-thread time). Merged, the
/// same row is exactly one span.
#[test]
fn same_style_printable_run_merges_into_one_span() {
    let mut core = TerminalCore::new(TerminalSize::new(80, 4));
    let row = "0123456789".repeat(8); // 80 columns, one style
    core.write_vt(row.as_bytes());

    let frame = core.snapshot_frame();
    let spans: Vec<_> = frame.lines[0].spans().collect();
    assert_eq!(
        spans.len(),
        1,
        "80 same-style printable cells must merge into one span (pre-merge: one span per cell)"
    );
    assert_eq!(spans[0].text, row);
    assert_eq!(spans[0].columns, 80);
    assert_eq!(frame.text().lines().next(), Some(row.as_str()));
}

/// Every style change is a run boundary: fg, italic, underline, and the
/// return to the default style each start a fresh span. The reassembled
/// `frame.text()` is byte-identical to what the per-cell spans produced.
#[test]
fn style_changes_split_the_merged_run() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 4));
    core.write_vt(b"ab\x1b[31mcd\x1b[0m\x1b[3mef\x1b[0m\x1b[4mgh\x1b[0mij");

    let frame = core.snapshot_frame();
    let spans: Vec<_> = frame.lines[0].spans().collect();
    assert_eq!(
        spans.iter().map(|span| span.text).collect::<Vec<_>>(),
        // (The rest of the row, blank, is in the last run.)
        vec!["ab", "cd", "ef", "gh", "ij"]
    );
    assert_eq!(spans[1].fg, TerminalColor::Named(NamedColor::Red));
    assert!(spans[2].italic);
    assert_eq!(spans[3].underline, TerminalUnderline::Single);
    assert_eq!(spans[4].fg, TerminalColor::Named(NamedColor::Foreground));
    assert_eq!(frame.text().lines().next(), Some("abcdefghij"));
}

/// Each character has its column: a wide character two, a combining
/// character its base's. One style is one run, whatever the widths in it
/// (the view places each character at its column).
#[test]
fn every_character_has_its_column() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt("ab日本cd".as_bytes());
    let frame = core.snapshot_frame();
    let line = &frame.lines[0];
    let columns: Vec<_> = line.cells.iter().map(|(_, column)| *column).collect();
    assert_eq!(columns, vec![0, 1, 2, 4, 6, 7]);
    assert_eq!(line.runs.len(), 1);
    assert_eq!(frame.text().lines().next(), Some("ab日本cd"));
    assert_eq!(frame.cursor.map(|cursor| cursor.col), Some(8));

    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt("abc\u{0301}de".as_bytes());
    let frame = core.snapshot_frame();
    let line = &frame.lines[0];
    let columns: Vec<_> = line.cells.iter().map(|(_, column)| *column).collect();
    assert_eq!(columns, vec![0, 1, 2, 2, 3, 4]);
    assert_eq!(frame.text().lines().next(), Some("abc\u{0301}de"));
}

/// DECSCUSR (`CSI Ps SP q`) shape changes ride `TerminalCursor::shape`;
/// DECTCEM hide (`CSI ?25l`) drops the cursor from the frame entirely.
#[test]
fn cursor_shape_and_visibility_reach_the_frame() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let shape = |core: &TerminalCore| core.snapshot_frame().cursor.map(|cursor| cursor.shape);

    assert_eq!(shape(&core), Some(TerminalCursorShape::Block));

    core.write_vt(b"\x1b[4 q");
    assert_eq!(shape(&core), Some(TerminalCursorShape::Underline));

    core.write_vt(b"\x1b[6 q");
    assert_eq!(shape(&core), Some(TerminalCursorShape::Beam));

    core.write_vt(b"\x1b[?25l");
    assert_eq!(shape(&core), None);

    core.write_vt(b"\x1b[?25h");
    assert_eq!(shape(&core), Some(TerminalCursorShape::Beam));

    core.write_vt(b"\x1b[2 q");
    assert_eq!(shape(&core), Some(TerminalCursorShape::Block));
}

#[test]
fn vt_stream_tracks_wide_character_columns() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt("日本語".as_bytes());

    let frame = core.snapshot_frame();
    assert!(frame.text().contains("日本語"));
    assert_eq!(frame.text().lines().next(), Some("日本語"));
    assert_eq!(frame.cursor.map(|cursor| cursor.col), Some(6));
    // Each wide character two columns on from the last.
    let columns: Vec<_> = (frame.lines[0].cells.iter())
        .map(|(_, column)| *column)
        .collect();
    assert_eq!(columns, vec![0, 2, 4]);
}

/// Backlog 45: SGR 8 (conceal) sets `Flags::HIDDEN` on a cell but leaves
/// its column in `alacritty_terminal`'s grid otherwise ordinary -- unlike
/// `Flags::WIDE_CHAR_SPACER`, whose partner cell already counts both
/// columns' width, a concealed cell has no such partner. If
/// `core::render::snapshot_frame` skips it without emitting a
/// placeholder span, every span painted after it on that row silently
/// shifts left by the concealed run's width (`src/terminal/mod.rs`'s
/// `paint_terminal` walks `line.spans` left to right, advancing its `x`
/// only by each span's own `columns` -- a cell with no span at all just
/// vanishes from that accounting instead of leaving a gap).
#[test]
fn concealed_text_still_occupies_its_columns() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[8msecret\x1b[0mvisible");

    let frame = core.snapshot_frame();
    // "secret" is 6 columns; "visible" must start at column 6, not slide
    // left to fill the gap the concealed run would otherwise leave.
    assert_eq!(frame.text().lines().next(), Some("      visible"));

    // The hidden characters are blank cells: "visible" begins at their
    // end.
    let first_line = &frame.lines[0];
    assert_eq!(first_line.text, "visible");
    assert_eq!(
        first_line.cells[0].1, 6,
        "visible text must start after the concealed run's columns"
    );
}

#[test]
fn scroll_display_uses_alacritty_history() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");

    let bottom = core.snapshot_text();
    assert!(bottom.contains("seven"));
    assert_eq!(core.display_offset(), 0);

    assert_eq!(core.handle_scroll(test_scroll(3)), None);
    let history = core.snapshot_text();
    assert!(!history.contains("seven"));
    assert!(core.display_offset() > 0);

    assert_eq!(core.handle_scroll(test_scroll(-3)), None);
    assert_eq!(core.display_offset(), 0);
}

/// Scrolling toward history is clamped at both edges, never wrapping or
/// going negative -- `alacritty_terminal::grid::Grid::scroll_display`'s own
/// `min(max(offset + count, 0), history_size)` clamp, exercised through the
/// core with real content on both sides of the boundary.
#[test]
fn scroll_clamps_at_both_history_edges() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 5));
    let mut script = String::new();
    for i in 0..20 {
        if i > 0 {
            script.push_str("\r\n");
        }
        script.push_str(&format!("line{i:02}"));
    }
    core.write_vt(script.as_bytes());
    assert_eq!(core.display_offset(), 0);

    // A single-line viewport with 20 written lines has a small, known
    // history size -- scroll far past it and confirm the offset clamps
    // instead of overshooting or wrapping.
    let history_size = {
        core.handle_scroll(test_scroll(1_000));
        core.display_offset()
    };
    assert!(
        history_size > 0 && history_size < 1_000,
        "offset should clamp to the real history size, got {history_size}"
    );
    let clamped_frame = core.snapshot_text();

    // Scrolling further toward history from the clamped top must be a
    // true no-op: same offset, same content.
    core.handle_scroll(test_scroll(50));
    assert_eq!(core.display_offset(), history_size);
    assert_eq!(core.snapshot_text(), clamped_frame);

    // Scroll back past the live edge (0) the same way -- must clamp at 0,
    // not go negative.
    core.handle_scroll(test_scroll(-1_000));
    assert_eq!(core.display_offset(), 0);
    let live_frame = core.snapshot_text();

    // And a further downward scroll from the live edge is a no-op too.
    core.handle_scroll(test_scroll(-50));
    assert_eq!(core.display_offset(), 0);
    assert_eq!(core.snapshot_text(), live_frame);
}

/// Regression for the scroll-into-history frame content the wire delivers
/// (2026-07-18 dogfooding report: scrolling up appeared to leave the top
/// pinned while the bottom rows went blank). Since wire v11
/// (`docs/remoc-adoption-design.md` §5 Option A) every delivery is a full
/// `TerminalCore::snapshot_frame` on the `rch::watch<TerminalFrame>` signal
/// — no diff round-trip to reconstruct — so this asserts the post-scroll
/// snapshot itself carries real history text in every row. Seeds forty
/// numbered lines through a five-row viewport so a ten-line scroll shifts
/// every visible row to genuinely different content.
#[test]
fn scroll_snapshot_reproduces_history_content() {
    let rows = 5;
    let mut core = TerminalCore::new(TerminalSize::new(20, rows));
    let mut script = String::new();
    for i in 0..40 {
        if i > 0 {
            script.push_str("\r\n");
        }
        script.push_str(&format!("line{i:02}"));
    }
    core.write_vt(script.as_bytes());

    // Baseline: the live (unscrolled) viewport -- what an attach seeds the
    // client's frame watch with.
    let live_frame = core.snapshot_frame();
    assert_eq!(core.display_offset(), 0);
    assert!(live_frame.text().contains("line39"));

    // Scroll toward history by two viewports' worth of rows -- the shape a
    // real multi-tick trackpad gesture produces via `ScrollAccumulator`,
    // and enough that every visible row's content genuinely changes.
    assert_eq!(core.handle_scroll(test_scroll(10)), None);
    let scrolled_frame = core.snapshot_frame();
    assert_ne!(core.display_offset(), 0, "scroll must move the viewport");
    assert_ne!(
        live_frame, scrolled_frame,
        "the scrolled snapshot must actually differ from the live one"
    );

    // Content assertions: every row of the full scrolled snapshot must
    // carry real history text -- no row silently reverted to blank.
    for (index, line) in scrolled_frame.lines.iter().enumerate() {
        let text: String = line.spans().map(|span| span.text).collect();
        assert!(
            text.starts_with("line"),
            "row {index} should show scrollback content, got {text:?} (full frame: {:?})",
            scrolled_frame.text()
        );
    }
    assert!(
        !scrolled_frame.text().contains("line39"),
        "a scroll toward history must leave the live tail behind"
    );
}

/// The cursor position reported on `TerminalFrame` must be viewport-relative,
/// not the raw (scroll-independent) grid coordinate `alacritty_terminal`
/// tracks internally -- companion regression to the frame-content fix above,
/// same root cause (`render::snapshot_frame` forgetting to fold
/// `display_offset` into a grid line before treating it as a row). While
/// scrolled into history the live cursor sits below the current viewport, so
/// it must be reported as absent (`None`), not silently drawn at some
/// unrelated row.
#[test]
fn cursor_is_hidden_while_scrolled_and_correct_at_the_live_edge() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 5));
    let mut script = String::new();
    for i in 0..40 {
        if i > 0 {
            script.push_str("\r\n");
        }
        script.push_str(&format!("line{i:02}"));
    }
    core.write_vt(script.as_bytes());

    let live_cursor = core.snapshot_frame().cursor;
    assert!(
        live_cursor.is_some(),
        "the cursor must be visible in the live (unscrolled) viewport"
    );

    core.handle_scroll(test_scroll(10));
    assert_eq!(
        core.snapshot_frame().cursor,
        None,
        "the cursor must be hidden once scrolled away from the live edge"
    );

    core.handle_scroll(test_scroll(-10));
    assert_eq!(
        core.display_offset(),
        0,
        "scrolling back down by the same amount returns to the live edge"
    );
    assert_eq!(
        core.snapshot_frame().cursor,
        live_cursor,
        "returning to the live edge must restore the same cursor position"
    );
}

/// Seeds `count` numbered `lineNN` rows through a `rows`-tall viewport, so
/// the bottom `rows` are the live screen and the rest is scrollback history
/// (`history_size == count - rows`). Shared by the `snapshot_window` tests.
fn numbered_history_core(rows: u16, count: usize) -> TerminalCore {
    numbered_history_core_sized(20, rows, count)
}

/// [`numbered_history_core`] with an explicit column width — the height-clamp
/// tests need a realistic width (the clamp is byte-budget-derived from the
/// column count).
fn numbered_history_core_sized(cols: u16, rows: u16, count: usize) -> TerminalCore {
    let mut core = TerminalCore::new(TerminalSize::new(cols, rows));
    let mut script = String::new();
    for i in 0..count {
        if i > 0 {
            script.push_str("\r\n");
        }
        script.push_str(&format!("line{i:02}"));
    }
    core.write_vt(script.as_bytes());
    core
}

/// The visible text of one window/frame row (spans concatenated; blank runs
/// contribute their empty text).
fn window_row_text(line: &TerminalLine) -> String {
    line.spans().map(|span| span.text).collect()
}

/// Phase 1 headline: `snapshot_window(anchor, height)` returns the correct
/// contiguous block for the requested position, with `viewport_offset`
/// locating the viewport inside it and `above`/`below` reporting the rows
/// that remain outside (`docs/terminal-scrollback-design.md` §3.2, §8). With
/// 40 numbered lines through a 5-row viewport there are 35 history rows;
/// anchoring at the live bottom (`anchor == 0`) with a 15-row height centers
/// the 5-row viewport in a 10-row block clamped at the live edge.
#[test]
fn snapshot_window_returns_the_anchored_block_with_metadata() {
    let core = numbered_history_core(5, 40); // history_size == 35

    let window = core.snapshot_window(0, 15);

    // margin = 15 - 5 = 10 (5 above, 5 below); the block clamps at the live
    // edge below (below == 0), so it spans line30..=line39 (10 rows) with the
    // live viewport (line35..) starting at offset 5.
    assert_eq!(window.lines.len(), 10);
    assert_eq!(window.viewport_offset, 5);
    assert_eq!(window.above, 30, "line00..line29 remain above the block");
    assert_eq!(window.below, 0, "the block's bottom is the live tail");
    assert_eq!(window_row_text(&window.lines[0]), "line30");
    assert_eq!(
        window_row_text(&window.lines[window.viewport_offset]),
        "line35",
        "viewport_offset must point at the live viewport's top row"
    );
    assert_eq!(window_row_text(&window.lines[9]), "line39");
}

/// The invariant the whole design rests on: reading a window must **not**
/// move the live `display_offset` (`docs/terminal-scrollback-design.md`
/// §2.2, §8) — unlike the `scroll_display` round-trip, `snapshot_window` is a
/// pure `iter_from` read. The live frame keeps showing the tail before and
/// after, even when the window is anchored deep in history.
#[test]
fn snapshot_window_does_not_move_the_live_display_offset() {
    let core = numbered_history_core(5, 40);
    assert_eq!(core.display_offset(), 0);
    let live_before = core.snapshot_frame();
    assert!(live_before.text().contains("line39"));

    // Anchor 20 rows above the live bottom: the returned block is centered on
    // line15, nowhere near the live tail.
    let window = core.snapshot_window(20, 15);
    assert_eq!(
        window_row_text(&window.lines[window.viewport_offset]),
        "line15",
        "the window must read history at the requested anchor"
    );

    // The read left the live viewport exactly where it was.
    assert_eq!(
        core.display_offset(),
        0,
        "snapshot_window must not move the live display_offset"
    );
    assert_eq!(
        core.snapshot_frame(),
        live_before,
        "the live frame must be byte-identical after a window read"
    );
}

/// `above == 0` is the true-top signal and `below == 0` the live-edge signal
/// (`docs/terminal-scrollback-design.md` §3.2, §5, §8). Anchoring at the very
/// top (`anchor == history_size`) clamps the block's top to the oldest row,
/// so `above == 0` and `viewport_offset == 0`.
#[test]
fn snapshot_window_clamps_at_the_true_top() {
    let core = numbered_history_core(5, 40); // history_size == 35

    let window = core.snapshot_window(35, 15);

    assert_eq!(window.above, 0, "the block's top is the oldest history row");
    assert_eq!(window.viewport_offset, 0);
    assert_eq!(window_row_text(&window.lines[0]), "line00");
    // margin_below = 5 below the 5-row viewport (rows line00..line09).
    assert_eq!(window.lines.len(), 10);
    assert_eq!(window.below, 30);
}

/// When the scrollback is shorter than the requested height the window
/// clamps at *both* edges and returns the whole buffer (`above == below ==
/// 0`) — the boundary the design's §8 "true edges" and hard-case §5 call for.
#[test]
fn snapshot_window_smaller_than_height_returns_the_whole_buffer() {
    // 6 lines through a 4-row viewport: history_size == 2, total == 6 rows.
    let core = numbered_history_core(4, 6);

    let window = core.snapshot_window(0, 40);

    assert_eq!(window.above, 0);
    assert_eq!(window.below, 0);
    assert_eq!(window.lines.len(), 6, "the whole buffer, clamped both ends");
    assert_eq!(
        window.viewport_offset, 2,
        "2 history rows above the viewport"
    );
    assert_eq!(window_row_text(&window.lines[0]), "line00");
    assert_eq!(window_row_text(&window.lines[5]), "line05");
}

/// Review fix (High): `snapshot_window` **hard-clamps** the requested height
/// to a safe row count, so a served window can never serialize past the
/// events-channel item cap (`TERMINAL_EVENT_MAX_ITEM_BYTES`). An unbounded
/// height would let a decorated full-scrollback window balloon to many
/// megabytes as one `TerminalUpdate::ScrollWindow`, trip remoc's over-cap
/// latch, tear down the shared events channel, and orphan the pane. Even
/// `height == usize::MAX` yields a bounded block.
#[test]
fn snapshot_window_clamps_an_oversized_height() {
    // 2000 lines through an 80x5 viewport: 1995 history rows, so a window
    // requested mid-history is bounded by the height clamp, not the grid
    // edges. max_window_rows(80, 5) = (5 + 64).min(409).max(5) = 69.
    let core = numbered_history_core_sized(80, 5, 2000);

    let window = core.snapshot_window(1000, usize::MAX);

    assert_eq!(
        window.lines.len(),
        69,
        "an oversized height must clamp to the safe row cap, not the buffer"
    );
    assert!(
        window.lines.len() < 2000,
        "the clamp must bite well below the full 2000-row buffer"
    );
}

/// Review fix: the clamp keeps `above`/`below`/`viewport_offset` exact for the
/// *clamped* block — `above + row_count + below` still accounts for every grid
/// row, and the viewport still fits inside the returned block. The identity
/// closes on the window actually served, not on the (larger) requested height.
#[test]
fn snapshot_window_metadata_identity_holds_under_clamp() {
    let rows = 5usize;
    let total = 2000usize; // history_size (1995) + screen_lines (5)
    let core = numbered_history_core_sized(80, rows as u16, total);

    let window = core.snapshot_window(1000, usize::MAX);

    assert_eq!(
        window.above + window.lines.len() + window.below,
        total,
        "above + row_count + below must still account for every grid row"
    );
    assert!(
        window.viewport_offset + rows <= window.lines.len(),
        "the viewport must fit inside the returned block after clamping"
    );
    assert!(
        window.above > 0 && window.below > 0,
        "an interior clamped window has real history on both sides"
    );
}

/// Issue 007: a tall single-pane viewport (screen_lines at or above the
/// byte-budget threshold where the old `.max(screen_lines)` floor collapsed
/// the overscan to zero) still gets a fixed overscan margin, so smooth
/// scrolling doesn't regress to line steps. The window is `screen_lines +
/// OVERSCAN_ROWS` rows tall (when the byte budget allows), with the viewport
/// centered by `viewport_offset = OVERSCAN_ROWS / 2`.
#[test]
fn snapshot_window_serves_overscan_for_a_tall_single_pane() {
    // 120 cols × 200 rows, with enough history that neither edge clamps.
    // max_window_rows(120, 200) = (200 + 64).min(273).max(200) = 264.
    let core = numbered_history_core_sized(120, 200, 2000);

    let window = core.snapshot_window(500, usize::MAX);

    assert_eq!(
        window.lines.len(),
        264,
        "a tall viewport gets viewport + OVERSCAN_ROWS, not the viewport alone"
    );
    assert_eq!(
        window.viewport_offset, 32,
        "the viewport is centered in the overscan (half above, half below)"
    );
    assert!(
        window.lines.len() > 200,
        "the window is taller than the viewport — overscan exists"
    );
}

/// Issue 007 fallback: in the extreme case where a single viewport alone
/// consumes the byte budget (screen_lines >= budget_rows), the overscan
/// collapses to zero and the window is exactly the viewport — the same
/// envelope the live-frame watch runs under. Scrolling then falls back to
/// line-step edge fetches, the documented fallback for extreme pane heights.
#[test]
fn snapshot_window_drops_overscan_when_the_viewport_fills_the_byte_budget() {
    // 120 cols: budget_rows = 4 MiB / (120 * 128) = 273. A 280-row viewport
    // exceeds it, so the floor collapses to screen_lines (280, no overscan).
    let core = numbered_history_core_sized(120, 280, 2000);

    let window = core.snapshot_window(500, usize::MAX);

    assert_eq!(
        window.lines.len(),
        280,
        "when the viewport alone fills the budget, the window is exactly the viewport"
    );
    assert_eq!(
        window.viewport_offset, 0,
        "no overscan margin — the viewport is pinned to the block top"
    );
}

/// The additive scrollback-availability flag (`docs/terminal-scrollback-design.md`
/// §2.3, §5): `true` on the primary screen, `false` while the alternate
/// screen is active (its grid has `max_scroll_limit == 0`, no scrollback),
/// restored on exit.
#[test]
fn scrollback_available_flag_tracks_the_alternate_screen() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    assert!(
        core.snapshot_frame().scrollback_available,
        "a fresh primary-screen session can serve scrollback windows"
    );

    core.write_vt(b"\x1b[?1049h");
    assert!(core.alternate_screen());
    assert!(
        !core.snapshot_frame().scrollback_available,
        "the alternate screen has no scrollback (max_scroll_limit == 0)"
    );

    core.write_vt(b"\x1b[?1049l");
    assert!(
        core.snapshot_frame().scrollback_available,
        "exiting the alternate screen restores scrollback availability"
    );
}

/// The other half of §2.3: a mouse-tracking app owns the wheel (it becomes
/// application input, not a `display_offset` move), so scrollback windowing
/// is suspended while `MOUSE_MODE` is set.
#[test]
fn scrollback_available_flag_is_false_in_mouse_mode() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[?1000h");
    assert!(
        !core.snapshot_frame().scrollback_available,
        "a mouse app's wheel is application input, not local scrollback"
    );

    core.write_vt(b"\x1b[?1000l");
    assert!(core.snapshot_frame().scrollback_available);
}

#[test]
fn scroll_in_alternate_screen_sends_application_input() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?1049h");

    assert!(core.alternate_screen());
    assert_eq!(
        core.handle_scroll(test_scroll(2)),
        Some(b"\x1b[A\x1b[A".to_vec())
    );
    assert_eq!(core.display_offset(), 0);
}

#[test]
fn sgr_mouse_mode_scroll_sends_wheel_reports() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?1000h\x1b[?1006h");

    assert_eq!(
        core.handle_scroll(TerminalScroll {
            lines: -1,
            point: TerminalSelectionPoint { row: 4, col: 7 },
        }),
        Some(b"\x1b[<65;8;5M".to_vec())
    );
    assert_eq!(core.display_offset(), 0);
}

#[test]
fn mouse_report_is_ignored_until_mouse_mode_is_enabled() {
    let core = TerminalCore::new(TerminalSize::new(20, 3));

    assert_eq!(
        core.handle_mouse_report(test_mouse(TerminalMouseKind::Press)),
        None
    );
}

#[test]
fn sgr_mouse_mode_click_sends_press_and_release_reports() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?1000h\x1b[?1006h");

    assert_eq!(
        core.handle_mouse_report(test_mouse(TerminalMouseKind::Press)),
        Some(b"\x1b[<0;8;5M".to_vec())
    );
    assert_eq!(
        core.handle_mouse_report(test_mouse(TerminalMouseKind::Release)),
        Some(b"\x1b[<3;8;5m".to_vec())
    );
}

#[test]
fn sgr_mouse_drag_requires_drag_or_motion_mode() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?1000h\x1b[?1006h");

    assert_eq!(
        core.handle_mouse_report(test_mouse(TerminalMouseKind::Drag)),
        None
    );

    core.write_vt(b"\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        core.handle_mouse_report(test_mouse(TerminalMouseKind::Drag)),
        Some(b"\x1b[<32;8;5M".to_vec())
    );
}

#[test]
fn paste_is_plain_text_by_default() {
    let core = TerminalCore::new(TerminalSize::new(20, 3));

    assert_eq!(core.paste_input("hello\n"), b"hello\n".to_vec());
}

#[test]
fn paste_wraps_text_when_bracketed_paste_is_enabled() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?2004h");

    assert_eq!(
        core.paste_input("hello\n"),
        b"\x1b[200~hello\n\x1b[201~".to_vec()
    );
}

#[test]
fn focus_input_is_none_without_mode_1004() {
    let core = TerminalCore::new(TerminalSize::new(20, 3));

    assert_eq!(core.focus_input(true), None);
    assert_eq!(core.focus_input(false), None);
}

#[test]
fn focus_input_reports_csi_i_and_o_once_mode_1004_is_enabled() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?1004h");

    assert_eq!(core.focus_input(true), Some(b"\x1b[I".to_vec()));
    assert_eq!(core.focus_input(false), Some(b"\x1b[O".to_vec()));
}

#[test]
fn focus_input_stops_once_mode_1004_is_disabled_again() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"\x1b[?1004h");
    core.write_vt(b"\x1b[?1004l");

    assert_eq!(core.focus_input(true), None);
}

#[test]
fn selection_to_string_uses_alacritty_selection() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"hello world");

    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 0 },
        TerminalSelectionKind::Simple,
    );
    core.update_selection(TerminalSelectionPoint { row: 0, col: 4 });

    assert_eq!(core.selected_text(), Some("hello".to_string()));
}

/// A drag back from where it began takes in the cell it began in and the
/// cell it has reached, as a drag forward does.
#[test]
fn a_selection_dragged_backwards_has_both_its_ends_in_it() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"0123456789\r\nabcdefghij");

    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 8 },
        TerminalSelectionKind::Simple,
    );
    core.update_selection(TerminalSelectionPoint { row: 0, col: 4 });
    assert_eq!(core.selected_text(), Some("45678".to_string()));

    // Up a row, too, and then forward past where it began.
    core.start_selection(
        TerminalSelectionPoint { row: 1, col: 2 },
        TerminalSelectionKind::Simple,
    );
    core.update_selection(TerminalSelectionPoint { row: 0, col: 7 });
    assert_eq!(core.selected_text(), Some("789\nabc".to_string()));
    core.update_selection(TerminalSelectionPoint { row: 1, col: 5 });
    assert_eq!(core.selected_text(), Some("cdef".to_string()));

    // A press alone selects nothing.
    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 3 },
        TerminalSelectionKind::Simple,
    );
    assert_eq!(core.selected_text().unwrap_or_default(), "");
}

/// `TerminalSelectionKind::Word` maps to alacritty's `SelectionType::
/// Semantic`, which selects the whole word under the click point as part
/// of construction -- no drag needed, and no need to port termy's own
/// char-class module (docs/research/gpui-terminal-presentation-2026-07-18.md,
/// "Selection").
#[test]
fn word_selection_selects_the_whole_word_under_the_click_with_no_drag() {
    let mut core = TerminalCore::new(TerminalSize::new(30, 3));
    core.write_vt(b"hello world");

    // Click lands mid-word ("wor|ld"), col 8.
    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 8 },
        TerminalSelectionKind::Word,
    );

    assert_eq!(core.selected_text(), Some("world".to_string()));
}

/// `TerminalSelectionKind::Line` maps to `SelectionType::Lines`, selecting
/// the whole row under the click point immediately.
#[test]
fn line_selection_selects_the_whole_line_under_the_click_with_no_drag() {
    let mut core = TerminalCore::new(TerminalSize::new(30, 3));
    core.write_vt(b"hello world");

    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 3 },
        TerminalSelectionKind::Line,
    );

    // Alacritty's `Lines` selection includes the row's trailing newline
    // (unlike `Semantic`/`Simple`, which stop at the last non-empty cell).
    assert_eq!(core.selected_text(), Some("hello world\n".to_string()));
}

/// A double-click-then-drag (`Word`) extends the selection by whole words,
/// not by character -- alacritty's own `Selection::update` honors the
/// selection's `SelectionType` for extension, not just for the initial
/// click; this is the "does it hold for drag too" half of the survey's
/// citation.
#[test]
fn word_selection_extends_by_whole_words_when_dragged() {
    let mut core = TerminalCore::new(TerminalSize::new(30, 3));
    core.write_vt(b"one two three");

    // Double-click on "two" (cols 4-6), then drag into "three" (starts
    // col 8) -- expect the selection to extend to cover the whole of
    // "three", not stop mid-word at the drag's exact column.
    core.start_selection(
        TerminalSelectionPoint { row: 0, col: 5 },
        TerminalSelectionKind::Word,
    );
    core.update_selection(TerminalSelectionPoint { row: 0, col: 10 });

    assert_eq!(core.selected_text(), Some("two three".to_string()));
}

#[test]
fn da1_query_reports_primary_device_attributes() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[c");

    // The engine's own answer: a VT220 (62) with ANSI color (22).
    assert_eq!(events.pty_writes, vec![b"\x1b[?62;22c".to_vec()]);
}

#[test]
fn da2_query_reports_secondary_device_attributes() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[>c");

    // The engine's own answer: a VT220 (1), firmware version 0, no
    // cartridge.
    assert_eq!(events.pty_writes, vec![b"\x1b[>1;0;0c".to_vec()]);
}

#[test]
fn dsr_query_reports_device_status_ok() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[5n");

    assert_eq!(events.pty_writes, vec![b"\x1b[0n".to_vec()]);
}

#[test]
fn cpr_query_reports_cursor_position() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"ab\r\ncd");
    let events = core.write_vt(b"\x1b[6n");

    assert_eq!(events.pty_writes, vec![b"\x1b[2;3R".to_vec()]);
}

#[test]
fn kitty_keyboard_query_reports_pushed_flags() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>5u");
    let events = core.write_vt(b"\x1b[?u");

    assert_eq!(events.pty_writes, vec![b"\x1b[?5u".to_vec()]);
}

#[test]
fn decrqm_2026_reports_set_while_a_synchronized_update_window_is_open() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));

    // Before any synchronized-update window: reset.
    let events = core.write_vt(b"\x1b[?2026$p");
    assert_eq!(events.pty_writes, vec![b"\x1b[?2026;2$y".to_vec()]);

    // Inside the window (BSU), in a separate write as a separate PTY read
    // would be: answered there and then, as set. The engine holds the
    // screen for the window, not the answers to what is asked in it.
    let events = core.write_vt(b"\x1b[?2026h");
    assert!(events.pty_writes.is_empty());
    let events = core.write_vt(b"\x1b[?2026$p");
    assert_eq!(events.pty_writes, vec![b"\x1b[?2026;1$y".to_vec()]);

    // Closed (ESU): nothing held back comes out, and reset again.
    let events = core.write_vt(b"\x1b[?2026l");
    assert!(events.pty_writes.is_empty());
    let events = core.write_vt(b"\x1b[?2026$p");
    assert_eq!(events.pty_writes, vec![b"\x1b[?2026;2$y".to_vec()]);
}

#[test]
fn xtwinops_18t_reports_size_in_characters() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[18t");

    assert_eq!(events.pty_writes, vec![b"\x1b[8;4;20t".to_vec()]);
}

#[test]
fn xtwinops_14t_reports_size_in_pixels_instead_of_hanging() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[14t");

    // Pixel geometry isn't known until a real resize carries it in (see
    // `TerminalSize::new`), so a freshly constructed core — never resized —
    // still answers honestly with 0 rather than hanging the caller.
    assert_eq!(events.pty_writes, vec![b"\x1b[4;0;0t".to_vec()]);
}

#[test]
fn xtwinops_14t_reports_real_pixel_dimensions_after_sized_resize() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.resize(TerminalSize {
        cols: 20,
        rows: 4,
        pixel_width: 180,
        pixel_height: 88,
    });
    let events = core.write_vt(b"\x1b[14t");

    assert_eq!(events.pty_writes, vec![b"\x1b[4;88;180t".to_vec()]);
}

/// `docs/session-daemon-design.md` decision 9: OSC 4/10/11/12 defaults
/// resolve against this crate's own [`TerminalColorScheme`] (a plain-data
/// mirror the host overwrites with its live theme via
/// `TerminalCore::set_color_scheme`), not `ui::theme` directly — a fresh
/// core (never given an explicit scheme, as here) falls back to
/// `TerminalColorScheme::default()`.
#[test]
fn osc11_query_reports_configured_background_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b]11;?\x07");

    let bg = TerminalColorScheme::default().background;
    let expected = format!(
        "\x1b]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x07",
        bg.r, bg.g, bg.b
    );
    assert_eq!(events.pty_writes, vec![expected.into_bytes()]);
}

#[test]
fn osc10_query_reports_configured_foreground_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b]10;?\x07");

    let fg = TerminalColorScheme::default().foreground;
    let expected = format!(
        "\x1b]10;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x07",
        fg.r, fg.g, fg.b
    );
    assert_eq!(events.pty_writes, vec![expected.into_bytes()]);
}

#[test]
fn osc4_query_reports_overridden_palette_color_over_theme_default() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b]4;1;#112233\x07");
    let events = core.write_vt(b"\x1b]4;1;?\x07");

    assert_eq!(
        events.pty_writes,
        vec![b"\x1b]4;1;rgb:1111/2222/3333\x07".to_vec()]
    );
}

#[test]
fn osc52_clipboard_write_reaches_terminal_events() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    // base64("hello") == "aGVsbG8="
    let events = core.write_vt(b"\x1b]52;c;aGVsbG8=\x07");

    assert_eq!(events.clipboard_writes, vec!["hello".to_string()]);
}

#[test]
fn osc52_clipboard_write_accepts_the_selection_target_too() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    // Horizon has no separate primary-selection buffer, so the `s`
    // (selection) target lands in the same `clipboard_writes` bucket as
    // `c` (clipboard) -- see `TerminalEvents::clipboard_writes`'s doc
    // comment.
    let events = core.write_vt(b"\x1b]52;s;aGVsbG8=\x07");

    assert_eq!(events.clipboard_writes, vec!["hello".to_string()]);
}

#[test]
fn osc52_clipboard_read_query_is_refused() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    // A bare `?` payload asks to read the clipboard back. `Osc52::OnlyCopy`
    // (see `TerminalCore::new`) means alacritty_terminal's own parser
    // refuses to even emit an event for this -- nothing is queued, and
    // nothing is written back to the PTY either (no error reply exists for
    // a refused OSC 52 load).
    let events = core.write_vt(b"\x1b]52;c;?\x07");

    assert!(events.clipboard_writes.is_empty());
    assert!(events.pty_writes.is_empty());
}

#[test]
fn osc52_clipboard_write_over_the_cap_is_dropped() {
    use base64::Engine;

    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let oversized = "a".repeat(300_000);
    let payload = base64::engine::general_purpose::STANDARD.encode(&oversized);
    let mut sequence = b"\x1b]52;c;".to_vec();
    sequence.extend_from_slice(payload.as_bytes());
    sequence.push(0x07);

    let events = core.write_vt(&sequence);

    assert!(
        events.clipboard_writes.is_empty(),
        "a payload past OSC52_CLIPBOARD_WRITE_CAP must be dropped, not forwarded"
    );
}

/// A real Claude Code redraw frame captured with `HORIZON_PTY_TRACE` while
/// reproducing the "Backspace deletes but the screen doesn't update" bug:
/// `\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[6C\x1b[25B\x1b[K\x1b[33;1H\x1b[26;7H\x1b[?25h\x1b[?2026l`
/// — hide cursor, home, move to row 26 col 7 (1-indexed), erase to end of
/// line (`\x1b[K`, the actual backspace delta), reposition, show cursor,
/// close the synchronized-update window (BSU/ESU, private mode 2026). Split
/// at byte 8 (`bsu_len` below) for the split-chunk/no-ESU cases: that is
/// exactly the end of the opening `\x1b[?2026h`.
const REAL_BACKSPACE_FRAME: &[u8] =
    b"\x1b[?2026h\x1b[?25l\x1b[H\r\x1b[6C\x1b[25B\x1b[K\x1b[33;1H\x1b[26;7H\x1b[?25h\x1b[?2026l";

/// Position the cursor at the same row/col the frame's own `\x1b[K` erases
/// from (1-indexed row 26, col 7) and plant a marker there, so a test can
/// assert on whether that marker survived.
fn seed_erase_target(core: &mut TerminalCore) {
    core.write_vt(b"\x1b[26;7HSTALE");
    assert!(core.snapshot_text().contains("STALE"));
}

fn sized_core() -> TerminalCore {
    // Tall/wide enough for the frame's `\x1b[33;1H`/`\x1b[26;7H` absolute
    // positioning.
    TerminalCore::new(TerminalSize::new(40, 40))
}

/// Suspect 1, case (a): a full synchronized-update frame delivered as a
/// single PTY read (`write_vt` call) applies immediately — no timer
/// involved, since `vte::ansi::Processor` sees BSU and ESU in the same
/// `advance`.
#[test]
fn sync_update_single_chunk_flushes_immediately() {
    let mut core = sized_core();
    seed_erase_target(&mut core);

    core.write_vt(REAL_BACKSPACE_FRAME);
    assert!(!core.snapshot_text().contains("STALE"));
}

/// Suspect 1, case (b): the same frame split across two `write_vt` calls
/// right after the opening BSU (mirroring a read-boundary split) stays
/// buffered until the second chunk's ESU arrives — `vte::ansi::Processor`
/// already tracks the open window across separate `advance` calls via its
/// own `sync_state.buffer`, scanning each newly appended chunk (plus a
/// small overlap) for the closing escape. Confirms cross-chunk buffering
/// itself was never the bug; only a *lost* ESU has no pump (case (c)).
#[test]
fn sync_update_split_across_chunks_flushes_only_after_second_chunk() {
    let mut core = sized_core();
    seed_erase_target(&mut core);

    let bsu_len = 8; // len(b"\x1b[?2026h")
    core.write_vt(&REAL_BACKSPACE_FRAME[..bsu_len]);
    assert!(
        core.snapshot_text().contains("STALE"),
        "content must stay buffered until ESU closes the window"
    );

    core.write_vt(&REAL_BACKSPACE_FRAME[bsu_len..]);
    assert!(!core.snapshot_text().contains("STALE"));
}

/// Suspect 1, case (c) — the actual bug: with the closing ESU truly lost
/// (never arrives in any later `write_vt` call either), the window stays
/// open and the erase never reaches the grid. `vte::ansi::Processor` has a
/// 150ms failsafe deadline (`sync_flush_deadline`) but never checks it
/// itself — nothing inside `advance` compares it to real time, since
/// `advance` only ever runs when new bytes show up (see the doc comment on
/// `SYNC_UPDATE_DECRQM_RESET` in `core.rs`). Without a caller pumping it,
/// this is a permanent freeze, not just a delay. `TerminalCore::
/// flush_sync_update` is that pump's mechanism (armed by
/// `session_loop`'s `sync_flush_rx` against `sync_flush_deadline`,
/// exercised end-to-end in
/// `session_loop::tests::sync_update_failsafe_flushes_a_stuck_window_after_the_deadline`).
#[test]
fn sync_update_with_no_esu_never_self_heals_but_flush_sync_update_forces_it() {
    let mut core = sized_core();
    seed_erase_target(&mut core);

    let bsu_len = 8;
    let esu_len = 8; // len(b"\x1b[?2026l")
    let without_esu = &REAL_BACKSPACE_FRAME[..REAL_BACKSPACE_FRAME.len() - esu_len];
    assert_eq!(&without_esu[..bsu_len], b"\x1b[?2026h");

    core.write_vt(without_esu);
    assert!(
        core.sync_flush_deadline().is_some(),
        "window should be open"
    );
    assert!(
        core.snapshot_text().contains("STALE"),
        "no ESU ever arrived: nothing should flush on its own"
    );

    // The runtime-level failsafe timer's actual mechanism: force-close the
    // window once its deadline has passed with no more PTY data.
    core.flush_sync_update();
    assert!(!core.snapshot_text().contains("STALE"));
    assert_eq!(
        core.sync_flush_deadline(),
        None,
        "window should now be closed"
    );
}

/// Suspect 1's stated "rolling freeze" theory — that once one ESU is lost,
/// *every subsequent* sync-wrapped frame also stays stuck, compounding
/// forever — turns out not to hold against the real `vte::ansi::Processor`
/// behavior: a stuck window's search for BSU/ESU resumes on the *next*
/// chunk's own bytes, so a later frame's own closing ESU flushes the
/// **entire** accumulated buffer at once (the old stuck delta and the new
/// frame's delta together), fully closing the window again. Documented here
/// as a regression guard on that specific (non-)compounding behavior, not
/// because it excuses skipping the case (c) fix above: this self-healing
/// only helps if the child ever emits another sync-wrapped frame at all,
/// which does not hold on a truly idle PTY (case (c)).
#[test]
fn sync_update_next_full_frame_esu_flushes_the_stuck_window_too() {
    let mut core = sized_core();
    seed_erase_target(&mut core);

    let esu_len = 8;
    core.write_vt(&REAL_BACKSPACE_FRAME[..REAL_BACKSPACE_FRAME.len() - esu_len]);
    assert!(core.snapshot_text().contains("STALE"));

    // A later, fully self-contained BSU..content..ESU frame arrives (e.g.
    // the next keystroke's echoed redraw).
    core.write_vt(b"\x1b[?2026h\x1b[1;1Hworld\x1b[?2026l");
    let snapshot = core.snapshot_text();
    assert!(
        !snapshot.contains("STALE"),
        "stuck frame's erase should apply too: {snapshot:?}"
    );
    assert!(
        snapshot.contains("world"),
        "new frame's content should apply too: {snapshot:?}"
    );
}

fn test_scroll(lines: i32) -> TerminalScroll {
    TerminalScroll {
        lines,
        point: TerminalSelectionPoint { row: 0, col: 0 },
    }
}

fn test_mouse(kind: TerminalMouseKind) -> TerminalMouseReport {
    TerminalMouseReport {
        kind,
        button: TerminalMouseButton::Left,
        point: TerminalSelectionPoint { row: 4, col: 7 },
        modifiers: TerminalMouseModifiers::default(),
    }
}

fn assert_serde_round_trip<T>(value: T)
where
    T: Serialize + DeserializeOwned + Debug + Eq,
{
    let encoded = serde_json::to_vec(&value).unwrap();
    let decoded = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(value, decoded);
}

#[test]
fn terminal_commands_round_trip_through_serde() {
    let commands = vec![
        TerminalCommand::Input(vec![0, 1, 255]),
        TerminalCommand::Resize(TerminalSize {
            cols: 120,
            rows: 40,
            pixel_width: 960,
            pixel_height: 800,
        }),
        TerminalCommand::Shutdown,
    ];
    for command in commands {
        assert_serde_round_trip(command);
    }
}

#[test]
fn terminal_updates_and_external_value_types_round_trip_through_serde() {
    let updates = vec![
        TerminalUpdate::Title(Some("title".into())),
        TerminalUpdate::Title(None),
        TerminalUpdate::Bell,
        TerminalUpdate::Clipboard {
            text: "clipboard".into(),
            destination: ClipboardDestination::Clipboard,
        },
        TerminalUpdate::Clipboard {
            text: "primary".into(),
            destination: ClipboardDestination::Primary,
        },
        TerminalUpdate::Exited,
        TerminalUpdate::Error("error".into()),
        TerminalUpdate::WorkingDirectory("/work/repo".into()),
    ];
    for update in updates {
        assert_serde_round_trip(update);
    }

    for color in [
        TerminalColor::Named(NamedColor::BrightCyan),
        TerminalColor::Indexed(217),
        TerminalColor::Rgb([1, 2, 3]),
    ] {
        assert_serde_round_trip(color);
    }
    assert_serde_round_trip(KeyCode::Function(12));
    assert_serde_round_trip(Modifiers::SHIFT | Modifiers::SUPER);
}

#[test]
fn terminal_wire_vocabulary_round_trips() {
    let session_id = uuid::Uuid::nil();
    let spec = TerminalSpawnSpec {
        shell: "/bin/sh".into(),
        args: vec!["-i".into()],
        term: "xterm-256color".into(),
        scrollback_lines: 10_000,
        color_scheme: TerminalColorScheme::default(),
        control_socket: "/tmp/horizon.sock".into(),
        fallback_cwd: "/tmp".into(),
        spawn_source_session_id: Some(session_id),
        initial_size: TerminalSize::new(80, 24),
    };
    assert_serde_round_trip(spec);
    assert_serde_round_trip(TerminalSummary { session_id });

    assert_serde_round_trip(TerminalCommand::Input(b"echo wire\n".to_vec()));
}

/// OSC 8 hyperlinks ride the frame: the URI alacritty parses onto each
/// cell surfaces on the span covering those cells, and disappears once
/// the link is closed (`OSC 8 ; ; ST`).
#[test]
fn osc8_hyperlink_carries_its_uri_on_the_span() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 4));
    core.write_vt(b"\x1b]8;;https://example.com/a\x1b\\link\x1b]8;;\x1b\\ plain");

    let frame = core.snapshot_frame();
    let row = &frame.lines[0];
    let linked = row
        .spans()
        .find(|span| span.text.contains("link"))
        .expect("linked text spans the row");
    assert_eq!(linked.url.as_deref(), Some("https://example.com/a"));
    let plain = row
        .spans()
        .find(|span| span.text.contains("plain"))
        .expect("plain text spans the row");
    assert_eq!(plain.url, None);
}

/// The URI is part of the span-merge key: adjacent cells sharing one URI
/// fuse into a single span, but two different URIs with otherwise
/// identical styling must never merge — a span carrying two URIs would
/// make the click target ambiguous.
#[test]
fn url_is_part_of_the_span_merge_key() {
    let mut core = TerminalCore::new(TerminalSize::new(40, 4));
    core.write_vt(
        b"\x1b]8;;https://a\x1b\\AB\x1b]8;;https://b\x1b\\CD\x1b]8;;https://a\x1b\\EF\x1b]8;;\x1b\\",
    );

    let frame = core.snapshot_frame();
    let spans: Vec<_> = frame.lines[0].spans().collect();
    let texts: Vec<(&str, Option<&str>)> = spans
        .iter()
        .filter(|span| !span.text.is_empty())
        .map(|span| (span.text, span.url.as_deref()))
        .collect();
    assert_eq!(
        texts,
        vec![
            ("AB", Some("https://a")),
            ("CD", Some("https://b")),
            ("EF", Some("https://a")),
        ]
    );
}

/// The scrollback window path shares `styled_rows`, so history rows carry
/// their URIs too — links stay openable in scrolled-back output.
#[test]
fn scrollback_window_spans_carry_osc8_uris() {
    let mut core = TerminalCore::with_scrollback(TerminalSize::new(40, 2), 10);
    core.write_vt(b"top\r\n");
    core.write_vt(b"\x1b]8;;https://example.com/h\x1b\\history\x1b]8;;\x1b\\\r\n");
    core.write_vt(b"tail");

    // `history` scrolled off the 2-row screen; anchor 1 looks back one line.
    let window = core.snapshot_window(1, 2);
    let line = window
        .lines
        .iter()
        .find(|line| line.spans().any(|span| span.text.contains("history")))
        .expect("the linked history row is inside the served window");
    let span = line
        .spans()
        .find(|span| span.text.contains("history"))
        .unwrap();
    assert_eq!(span.url.as_deref(), Some("https://example.com/h"));
}

#[test]
fn undersized_initial_geometry_keeps_vt_and_frame_usable() {
    let mut core = TerminalCore::new(TerminalSize::new(0, 0));
    core.write_vt("界".as_bytes());
    assert_eq!(core.snapshot_frame().lines.len(), 1);
    assert!(core.snapshot_text().contains("界"));
    assert_eq!(
        core.write_vt(b"\x1b[18t").pty_writes,
        [b"\x1b[8;1;2t".to_vec()]
    );
}

#[test]
fn undersized_resize_keeps_wide_cells_and_reports_effective_geometry() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    for cols in [0, 1, 2] {
        core.resize(TerminalSize {
            cols,
            rows: 0,
            pixel_width: 18,
            pixel_height: 22,
        });
        core.write_vt("\x1b[2J\x1b[H界".as_bytes());
        assert_eq!(core.snapshot_frame().lines.len(), 1);
        assert!(core.snapshot_text().contains("界"));
        assert_eq!(
            core.write_vt(b"\x1b[18t").pty_writes,
            [b"\x1b[8;1;2t".to_vec()]
        );
        assert_eq!(
            core.write_vt(b"\x1b[14t").pty_writes,
            [b"\x1b[4;22;18t".to_vec()]
        );
    }
    core.resize(TerminalSize::new(20, 4));
    assert_eq!(core.snapshot_frame().lines.len(), 4);
    assert_eq!(
        core.write_vt(b"\x1b[18t").pty_writes,
        [b"\x1b[8;4;20t".to_vec()]
    );
}

#[test]
fn a_program_asking_whether_its_status_is_read_is_answered() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b]7501;?\x1b\\");

    assert_eq!(
        events.pty_writes,
        [b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\".to_vec()]
    );
    assert!(events.program_status.is_empty());
    // Nothing of it reaches the screen.
    assert!(events.output.is_empty());
}

#[test]
fn questions_read_in_one_chunk_are_answered_in_the_order_they_were_asked() {
    let reply =
        b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    // What Claude Code writes as it starts, read at once: the keyboard
    // protocol's question, whether OSC 7501 is read, and DA1, whose
    // answer coming first tells it that OSC 7501 is not.
    let events = core.write_vt(b"\x1b[?u\x1b]7501;?\x07\x1b[c");

    assert_eq!(
        events.pty_writes,
        [
            b"\x1b[?0u".to_vec(),
            reply.to_vec(),
            b"\x1b[?62;22c".to_vec()
        ]
    );
    assert_eq!(events.output, b"\x1b[?u\x1b[c");
}

#[test]
fn a_programs_status_is_read_off_the_stream_and_leaves_the_screen_alone() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(
        b"hi\x1b]7501;state=blocked:app=claude-code:kind=permission\x1b\\\
          \x1b]7501;state=working:app=claude-code:id=bt7g05417\x1b\\\
          \x1b]7501;state=clear\x1b\\",
    );

    let [Some(status), None] = &events.program_status[..] else {
        panic!("read {:?}", events.program_status);
    };
    assert_eq!(status.state, ProgramState::Blocked);
    assert_eq!(status.blocked_on, Some(ProgramBlockedOn::Permission));
    assert_eq!(status.app.as_deref(), Some("claude-code"));
    assert!(events.pty_writes.is_empty());
    assert_eq!(core.snapshot_text().trim(), "hi");
}
