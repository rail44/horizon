use termwiz::input::{KeyCode, Modifiers};

use super::*;
use alacritty_terminal::vte::ansi::NamedColor;

#[test]
fn vt_stream_updates_snapshot() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"hello\r\n\x1b[31mred\x1b[0m");

    let snapshot = core.snapshot_text();
    assert!(snapshot.contains("hello"));
    assert!(snapshot.contains("red"));
}

#[test]
fn kitty_keyboard_mode_switches_termwiz_encoding() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>1u");

    let encoded = core.encode_key(KeyCode::Escape, Modifiers::NONE, KeyEventKind::Press);
    assert!(!encoded.is_empty());
}

#[test]
fn key_up_events_do_not_emit_legacy_input() {
    let core = TerminalCore::new(TerminalSize::new(20, 4));
    let encoded = core.encode_key(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Release);
    assert_eq!(encoded, "");
}

/// `TerminalFrame::keys_as_escape_codes` is the host view's routing
/// signal (send printable keys as `TerminalCommand::Key`, not plain
/// text) — it must track kitty's "report all keys" flag (8) and only
/// that flag: disambiguate-only (1) leaves plain text keys on the text
/// path, and popping the flags restores it.
#[test]
fn frame_mirrors_report_all_keys_flag() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    assert!(!core.snapshot_frame().keys_as_escape_codes);

    core.write_vt(b"\x1b[>1u");
    assert!(!core.snapshot_frame().keys_as_escape_codes);
    core.write_vt(b"\x1b[<u");

    core.write_vt(b"\x1b[>8u");
    assert!(core.snapshot_frame().keys_as_escape_codes);

    core.write_vt(b"\x1b[<u");
    assert!(!core.snapshot_frame().keys_as_escape_codes);
}

/// `docs/session-daemon-design.md` decision 8: a cell's color is a logical
/// value (here, `NamedColor::Red`/`NamedColor::Foreground`), not a resolved
/// RGB triple — resolving against a theme is the host's job now
/// (`terminal::view`'s golden test covers that RGB values are unchanged
/// once resolved with the default theme).
#[test]
fn vt_stream_preserves_ansi_foreground_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[31mred\x1b[0m plain");

    let frame = core.snapshot_frame();
    assert!(frame.text.contains("red plain"));
    let first_line = &frame.lines[0];
    assert!(first_line
        .spans
        .iter()
        .any(|span| { span.text == "r" && span.fg == TerminalColor::Named(NamedColor::Red) }));
    assert!(first_line.spans.iter().any(|span| {
        span.text == "p" && span.fg == TerminalColor::Named(NamedColor::Foreground)
    }));
}

/// `docs/session-daemon-design.md` decision 8: an app's live OSC 4 palette
/// override now rides the frame as `TerminalFrame::palette_overrides`, a
/// sparse index→RGB table the host consults at paint time (see
/// `terminal::view::color::resolve_color`'s test coverage for the host
/// half).
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
    assert!(core
        .snapshot_frame()
        .palette_overrides
        .iter()
        .any(|(index, _)| *index == 1));

    core.write_vt(b"\x1b]104;1\x1b\\");
    assert!(!core
        .snapshot_frame()
        .palette_overrides
        .iter()
        .any(|(index, _)| *index == 1));
}

#[test]
fn vt_stream_tracks_wide_character_columns() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt("日本語".as_bytes());

    let frame = core.snapshot_frame();
    assert!(frame.text.contains("日本語"));
    assert_eq!(frame.text.lines().next(), Some("日本語"));
    assert_eq!(frame.cursor.map(|cursor| cursor.col), Some(6));
    assert!(frame.lines[0]
        .spans
        .iter()
        .any(|span| span.text == "日" && span.columns == 2));
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

    core.start_selection(TerminalSelectionPoint { row: 0, col: 0 });
    core.update_selection(TerminalSelectionPoint { row: 0, col: 4 });

    assert_eq!(core.selected_text(), Some("hello".to_string()));
}

#[test]
fn da1_query_reports_primary_device_attributes() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[c");

    assert_eq!(events.pty_writes, vec![b"\x1b[?6c".to_vec()]);
}

#[test]
fn da2_query_reports_secondary_device_attributes() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b[>c");

    assert_eq!(events.pty_writes.len(), 1);
    let response = String::from_utf8(events.pty_writes[0].clone()).unwrap();
    assert!(response.starts_with("\x1b[>0;"));
    assert!(response.ends_with(";1c"));
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

    // Before any synchronized-update window: alacritty_terminal's own
    // (correct, in this case) reset reply passes through untouched.
    let events = core.write_vt(b"\x1b[?2026$p");
    assert_eq!(events.pty_writes, vec![b"\x1b[?2026;2$y".to_vec()]);

    // Open the window (BSU) and query again in a *separate* `write_vt`
    // call — mirroring separate PTY reads, as a real round-trip
    // verification would naturally be (the app must see the BSU take
    // effect before deciding to query). alacritty_terminal buffers
    // everything after BSU opaquely and only releases it once ESU (or its
    // failsafe timeout) closes the window, so this query's reply doesn't
    // surface yet.
    let events = core.write_vt(b"\x1b[?2026h");
    assert!(events.pty_writes.is_empty());
    let events = core.write_vt(b"\x1b[?2026$p");
    assert!(
        events.pty_writes.is_empty(),
        "the query is buffered until the window closes, not answered inline"
    );

    // Closing the window (ESU) flushes the buffered query. Upstream
    // hardcodes "reset" for this reply regardless of live state (see
    // `rewrite_sync_update_decrqm` in `core.rs`); patched, it must report
    // "set" since the window was open when the query was made.
    let events = core.write_vt(b"\x1b[?2026l");
    assert_eq!(events.pty_writes, vec![b"\x1b[?2026;1$y".to_vec()]);

    // After ESU, a fresh query goes back to reporting reset.
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

/// Byte-for-byte truth table for `TerminalCore::key_input` across every
/// meaningful Kitty progressive-enhancement flag combination, verified
/// against <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>. Flags are
/// pushed via the real `CSI > flags u` sequence (not injected directly) so
/// this exercises the exact path a foreground app like Claude Code drives.
///
/// This is the regression test for the "Shift+Enter kills all subsequent
/// terminal input" bug: under any non-zero flag set, Horizon used to emit
/// `\x1b[27;2;13~` for Shift+Enter — xterm's `modifyOtherKeys` format,
/// `~`-terminated — instead of Kitty's own `\x1b[13;2u`, because termwiz
/// 0.23.3's `KeyboardEncoding::Kitty(_)` variant is never actually matched
/// inside `KeyCode::encode` (confirmed by reading termwiz's vendored
/// source), and `TerminalCore::encode_modes` used to derive termwiz's
/// unrelated `modify_other_keys` xterm setting from Kitty's own
/// `DISAMBIGUATE_ESC_CODES` bit. A `~`-terminated sequence where a
/// Kitty-aware reader expects `u` is a plausible way to wedge a client's own
/// parser into swallowing everything that follows. See `kitty_override` in
/// `terminal::protocol::kitty_keyboard` for the fix.
///
/// Also covers the follow-up regression that first fix introduced: it made
/// Shift+Enter fall all the way back to bare `\r` under disambiguate-only
/// mode (the literal spec text's "same bytes as legacy mode" exception has
/// no modifier carve-out), which is exactly what Claude Code negotiates
/// (`CSI>1u`, confirmed by capturing its real startup handshake) — so
/// Shift+Enter *submitted* instead of inserting a newline. `kitty_override`
/// now promotes Enter/Tab/Backspace to `CSI u` under disambiguate alone once
/// any modifier is held (bare presses still stay legacy); see its doc
/// comment for the empirical justification against a real client.
#[test]
fn kitty_csi_u_truth_table() {
    fn push_flags(core: &mut TerminalCore, flags: u32) {
        if flags != 0 {
            core.write_vt(format!("\x1b[>{flags}u").as_bytes());
        }
    }

    // (Enter, Tab, Backspace, Escape) is the exact set termwiz's own
    // encoder cannot express correctly once Kitty flags are active; arrows/
    // Home/End/PageUp/PageDown/Delete already reuse xterm-compatible
    // sequences Kitty itself specifies, so they're intentionally excluded
    // here (see `kitty_override`'s doc comment).
    let cases: &[(&str, KeyCode, Modifiers)] = &[
        ("Enter", KeyCode::Enter, Modifiers::NONE),
        ("Shift+Enter", KeyCode::Enter, Modifiers::SHIFT),
        ("Ctrl+Enter", KeyCode::Enter, Modifiers::CTRL),
        ("Alt+Enter", KeyCode::Enter, Modifiers::ALT),
        ("Tab", KeyCode::Tab, Modifiers::NONE),
        ("Shift+Tab", KeyCode::Tab, Modifiers::SHIFT),
        ("Backspace", KeyCode::Backspace, Modifiers::NONE),
        ("Esc", KeyCode::Escape, Modifiers::NONE),
    ];

    // flags = 0: no Kitty protocol negotiated at all. Byte-identical to
    // Horizon's pre-existing (correct, untouched) behavior.
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    push_flags(&mut core, 0);
    let expect = |core: &TerminalCore, key, mods, want: &[u8]| {
        assert_eq!(
            core.key_input(key, mods, KeyEventKind::Press),
            want.to_vec()
        );
    };
    for (name, key, mods) in cases {
        let expected: &[u8] = match *name {
            "Enter" | "Shift+Enter" | "Ctrl+Enter" => b"\r",
            "Alt+Enter" => b"\x1b\r",
            "Tab" => b"\t",
            "Shift+Tab" => b"\x1b[Z",
            "Backspace" => b"\x7f",
            "Esc" => b"\x1b",
            other => panic!("unhandled case {other}"),
        };
        expect(&core, *key, *mods, expected);
    }

    // flags = 0b1 (disambiguate only) and 0b11 (+ report event types): the
    // *bare* Enter/Tab/Backspace stay legacy bytes (crash-recovery case:
    // `reset<Enter>` must still work); any modifier promotes them to
    // `CSI u` (the documented, empirically-verified deviation from the
    // spec's unconditional exception text — see `kitty_override`). Esc is
    // promoted unconditionally by disambiguate alone, per spec. Critically:
    // no `CSI 27;mods;codepoint~` (the original bug) anywhere.
    for flags in [0b1u32, 0b11] {
        let mut core = TerminalCore::new(TerminalSize::new(20, 4));
        push_flags(&mut core, flags);
        for (name, key, mods) in cases {
            let expected: &[u8] = match *name {
                "Enter" => b"\r",
                "Shift+Enter" => b"\x1b[13;2u",
                "Ctrl+Enter" => b"\x1b[13;5u",
                "Alt+Enter" => b"\x1b[13;3u",
                "Tab" => b"\t",
                "Shift+Tab" => b"\x1b[9;2u",
                "Backspace" => b"\x7f",
                "Esc" => b"\x1b[27u",
                other => panic!("unhandled case {other}"),
            };
            expect(&core, *key, *mods, expected);
        }
    }

    // flags = 0b1111 and 0b11111 (report-all-keys-as-escape-codes active):
    // every key, modified or not, becomes genuine Kitty `CSI u`.
    for flags in [0b1111u32, 0b11111] {
        let mut core = TerminalCore::new(TerminalSize::new(20, 4));
        push_flags(&mut core, flags);
        for (name, key, mods) in cases {
            let expected: &[u8] = match *name {
                "Enter" => b"\x1b[13u",
                "Shift+Enter" => b"\x1b[13;2u",
                "Ctrl+Enter" => b"\x1b[13;5u",
                "Alt+Enter" => b"\x1b[13;3u",
                "Tab" => b"\x1b[9u",
                "Shift+Tab" => b"\x1b[9;2u",
                "Backspace" => b"\x1b[127u",
                "Esc" => b"\x1b[27u",
                other => panic!("unhandled case {other}"),
            };
            expect(&core, *key, *mods, expected);
        }
    }
}

/// `Modifiers::encode_xterm()` (termwiz) drops the Super/Cmd/Win bit even
/// though `app::keymap::termwiz_modifiers` does carry it through — see the
/// comment in `kitty_override`. This is the regression test for the local
/// fix that adds it back for the four keys we encode ourselves.
#[test]
fn kitty_override_reports_super_modifier() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>1u");
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::SUPER, KeyEventKind::Press),
        b"\x1b[13;9u".to_vec()
    );

    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>31u");
    assert_eq!(
        core.key_input(
            KeyCode::Enter,
            Modifiers::SUPER | Modifiers::SHIFT,
            KeyEventKind::Press
        ),
        b"\x1b[13;10u".to_vec()
    );
}

/// Compliance coverage for the keys `kitty_override` deliberately leaves
/// alone: arrows, Home/End, PageUp/PageDown, Insert and Delete already
/// reuse the xterm-compatible `CSI 1;mods<letter>` / `CSI n;mods~` forms
/// the Kitty spec itself specifies for these ("Functional key definitions":
/// `HOME` = `1 H or 7 ~`, `END` = `1 F or 8 ~`, arrows = `1 A/B/C/D`,
/// `PAGE_UP`/`PAGE_DOWN` = `5 ~`/`6 ~`, `INSERT` = `2 ~`, `DELETE` = `3 ~`),
/// unconditionally — unlike Enter/Tab/Backspace these were never ambiguous
/// in legacy mode, so the spec doesn't move them to a different form under
/// any of disambiguate, report-event-types or report-all-keys. termwiz's
/// built-in encoder already gets this right without any Horizon-side
/// override, across every flag combination. This test is Press-only —
/// `csi_u_navigation_key_event_type_truth_table` covers Repeat/Release,
/// which *do* change once `REPORT_EVENT_TYPES` is active.
#[test]
fn navigation_keys_are_flag_invariant_and_spec_compliant() {
    let nav_cases: &[(&str, KeyCode, Modifiers, &[u8])] = &[
        ("Up", KeyCode::UpArrow, Modifiers::NONE, b"\x1b[A"),
        ("Shift+Up", KeyCode::UpArrow, Modifiers::SHIFT, b"\x1b[1;2A"),
        ("Home", KeyCode::Home, Modifiers::NONE, b"\x1b[H"),
        ("Ctrl+Home", KeyCode::Home, Modifiers::CTRL, b"\x1b[1;5H"),
        ("End", KeyCode::End, Modifiers::NONE, b"\x1b[F"),
        ("PageUp", KeyCode::PageUp, Modifiers::NONE, b"\x1b[5~"),
        ("PageDown", KeyCode::PageDown, Modifiers::NONE, b"\x1b[6~"),
        ("Insert", KeyCode::Insert, Modifiers::NONE, b"\x1b[2~"),
        ("Alt+Delete", KeyCode::Delete, Modifiers::ALT, b"\x1b[3;3~"),
    ];

    for flags in [0u32, 0b1, 0b1111, 0b11111] {
        let mut core = TerminalCore::new(TerminalSize::new(20, 4));
        if flags != 0 {
            core.write_vt(format!("\x1b[>{flags}u").as_bytes());
        }
        for (name, key, mods, want) in nav_cases {
            assert_eq!(
                core.key_input(*key, *mods, KeyEventKind::Press),
                want.to_vec(),
                "flags={flags:#b} case={name}"
            );
        }
    }
}

/// IMPLEMENTED (name kept for `TEST_REGISTRY`/`KITTY_COMPLIANCE`
/// continuity, even though it now documents the opposite of what it once
/// did): `TerminalCore::key_input`/`encode_key` now route a real
/// `KeyEventKind` all the way down to `terminal::protocol::kitty_keyboard`,
/// which owns `CSI u` encoding outright once any Kitty flag is active — it
/// no longer leans on termwiz's `KeyCode::encode` (whose vendored
/// `is_down == false` hardcode made a release genuinely unrepresentable)
/// for that path at all. A release now produces the spec's `:3` event-type
/// subfield (`;modifiers:3`) on any key already promoted to `CSI u`, once
/// `REPORT_EVENT_TYPES` is negotiated. Plain 'a' specifically needs
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` too — a text key has no `CSI u` form
/// to attach an event type to otherwise (see
/// `kitty_keyboard::encode_text_key`'s doc comment) — so this pushes flags
/// `1 (disambiguate) + 2 (report-event-types) + 8 (report-all-keys) = 11`,
/// not report-event-types alone. See `csi_u_event_type_truth_table` for
/// broader coverage (functional keys, un-promoted keys, repeats, and the
/// legacy no-Kitty-flags case) and `KITTY_COMPLIANCE`'s "Report event
/// types" rows for what's covered and what still isn't.
#[test]
fn release_events_are_unimplemented_regardless_of_flags() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>11u"); // disambiguate + report-event-types + report-all-keys
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Release),
        b"\x1b[97;1:3u".to_vec(),
        "spec's release-event form for plain 'a' once report-all-keys promotes it to CSI u"
    );
}

/// Broader event-type coverage across representative flag/key-class
/// combinations, extending `kitty_csi_u_truth_table`/`csi_u_text_key_truth_table`
/// (both Press-only) with Repeat and Release. Verified against
/// <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>'s "event types"
/// section: press omits the subfield entirely (value `1`, the implicit
/// default), repeat is `:2`, release is `:3` — and per its own qualifier
/// ("only key events represented as escape codes due to the other
/// enhancements in effect will be affected"), the subfield only ever
/// decorates a key already promoted to genuine `CSI u` by some other flag;
/// an un-promoted key's repeat is byte-identical to its press, and its
/// release produces nothing at all — the same rule `encode`/
/// `encode_text_key`'s "not down and not promoted -> empty" fallback
/// already encodes.
#[test]
fn csi_u_event_type_truth_table() {
    // No Kitty flags negotiated at all: repeat is indistinguishable from
    // press (termwiz's legacy encoder has no repeat/release concept), and
    // release is always empty — regardless of REPORT_EVENT_TYPES, which by
    // definition can't be set when `flags` is empty.
    let core = TerminalCore::new(TerminalSize::new(20, 4));
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::NONE, KeyEventKind::Press),
        b"\r".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::NONE, KeyEventKind::Repeat),
        b"\r".to_vec(),
        "repeat matches press with no Kitty flags active"
    );
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::NONE, KeyEventKind::Release),
        Vec::<u8>::new(),
        "release is always empty with no Kitty flags active"
    );

    // Disambiguate + report-event-types (no report-all-keys): a *modified*
    // Enter is promoted to CSI u under Horizon's own disambiguate-alone
    // deviation (see `kitty_override`'s doc comment) and gains full
    // event-type support the moment REPORT_EVENT_TYPES is active, even
    // without report-all-keys.
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>3u");
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::SHIFT, KeyEventKind::Press),
        b"\x1b[13;2u".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::SHIFT, KeyEventKind::Repeat),
        b"\x1b[13;2:2u".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::SHIFT, KeyEventKind::Release),
        b"\x1b[13;2:3u".to_vec()
    );
    // Esc has no crash-recovery exception at all: disambiguate alone
    // promotes it unmodified, so it gets the same treatment. The modifier
    // field stays explicit (`;1`) even with no real modifier held, per
    // spec: "If no modifiers are present, the modifiers field must have
    // the value 1 and the event type sub-field the type of event."
    assert_eq!(
        core.key_input(KeyCode::Escape, Modifiers::NONE, KeyEventKind::Repeat),
        b"\x1b[27;1:2u".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Escape, Modifiers::NONE, KeyEventKind::Release),
        b"\x1b[27;1:3u".to_vec()
    );
    // Bare Enter stays unpromoted at these flags (the crash-recovery
    // carve-out — see `kitty_override`'s doc comment): repeat matches
    // legacy press, and release has no representation to attach an event
    // type to, so it's empty — exactly the spec's "Enter ... will not have
    // release events unless report-all-keys is also set", which falls out
    // of the promotion test rather than being special-cased.
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::NONE, KeyEventKind::Repeat),
        b"\r".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Enter, Modifiers::NONE, KeyEventKind::Release),
        Vec::<u8>::new()
    );
    // Navigation keys never go through `kitty_override`, but
    // `navigation_key_event_override` still decorates their own legacy
    // `CSI` form with the modifiers:event-type sub-field once
    // REPORT_EVENT_TYPES is active — see `KITTY_COMPLIANCE`'s "Report event
    // types" navigation row (`csi_u_navigation_key_event_type_truth_table`
    // below has broader coverage of this).
    assert_eq!(
        core.key_input(KeyCode::UpArrow, Modifiers::NONE, KeyEventKind::Repeat),
        b"\x1b[1;1:2A".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::UpArrow, Modifiers::NONE, KeyEventKind::Release),
        b"\x1b[1;1:3A".to_vec()
    );

    // Report-event-types + report-all-keys (no disambiguate): text keys
    // are promoted purely by report-all-keys (see `encode_text_key`'s doc
    // comment), so this is enough on its own for their event types too.
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>10u");
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Press),
        b"\x1b[97u".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Repeat),
        b"\x1b[97;1:2u".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Release),
        b"\x1b[97;1:3u".to_vec()
    );

    // Report-event-types alone (no report-all-keys): a text key has no
    // CSI u form to decorate, so it behaves exactly like the no-flags
    // case above — repeat matches legacy press, release is empty.
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>2u");
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Press),
        b"a".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Repeat),
        b"a".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::NONE, KeyEventKind::Release),
        Vec::<u8>::new()
    );
}

/// Broader coverage for `navigation_key_event_override`: the "Report event
/// types" modifier sub-field applied to navigation keys' own legacy `CSI`
/// forms (letter-terminated arrows/Home/End, `~`-terminated
/// PageUp/PageDown/Insert/Delete), across several representative keys,
/// modifiers, and flag states. See `KITTY_COMPLIANCE`'s "Report event
/// types" navigation row for the primary-source verification (kitty's own
/// `key_encoding.c` and alacritty's `keyboard.rs`) backing this being
/// `Compliant` rather than a deviation from the spec text.
#[test]
fn csi_u_navigation_key_event_type_truth_table() {
    // No Kitty flags at all: repeat matches press, release is empty --
    // exactly as before, unaffected by this feature.
    let core = TerminalCore::new(TerminalSize::new(20, 4));
    assert_eq!(
        core.key_input(KeyCode::Home, Modifiers::NONE, KeyEventKind::Repeat),
        b"\x1b[H".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Home, Modifiers::NONE, KeyEventKind::Release),
        Vec::<u8>::new()
    );

    // REPORT_EVENT_TYPES alone (flags = 0b10): every navigation key gains a
    // repeat/release representation it never had before, decorating its
    // existing legacy CSI form rather than replacing it with a genuine
    // `CSI u` -- letter-terminated (arrows/Home/End) and `~`-terminated
    // (PageUp/PageDown/Insert/Delete) forms both covered.
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>2u");
    // A small named struct rather than a tuple -- a `(&str, KeyCode,
    // Modifiers, &[u8], &[u8])` tuple trips clippy's `type_complexity`.
    struct NavEventCase {
        name: &'static str,
        key: KeyCode,
        mods: Modifiers,
        repeat: &'static [u8],
        release: &'static [u8],
    }
    let cases = [
        NavEventCase {
            name: "Up",
            key: KeyCode::UpArrow,
            mods: Modifiers::NONE,
            repeat: b"\x1b[1;1:2A",
            release: b"\x1b[1;1:3A",
        },
        NavEventCase {
            name: "Shift+Up",
            key: KeyCode::UpArrow,
            mods: Modifiers::SHIFT,
            repeat: b"\x1b[1;2:2A",
            release: b"\x1b[1;2:3A",
        },
        NavEventCase {
            name: "Home",
            key: KeyCode::Home,
            mods: Modifiers::NONE,
            repeat: b"\x1b[1;1:2H",
            release: b"\x1b[1;1:3H",
        },
        NavEventCase {
            name: "End",
            key: KeyCode::End,
            mods: Modifiers::NONE,
            repeat: b"\x1b[1;1:2F",
            release: b"\x1b[1;1:3F",
        },
        NavEventCase {
            name: "PageUp",
            key: KeyCode::PageUp,
            mods: Modifiers::NONE,
            repeat: b"\x1b[5;1:2~",
            release: b"\x1b[5;1:3~",
        },
        NavEventCase {
            name: "PageDown",
            key: KeyCode::PageDown,
            mods: Modifiers::NONE,
            repeat: b"\x1b[6;1:2~",
            release: b"\x1b[6;1:3~",
        },
        NavEventCase {
            name: "Insert",
            key: KeyCode::Insert,
            mods: Modifiers::NONE,
            repeat: b"\x1b[2;1:2~",
            release: b"\x1b[2;1:3~",
        },
        NavEventCase {
            name: "Ctrl+Delete",
            key: KeyCode::Delete,
            mods: Modifiers::CTRL,
            repeat: b"\x1b[3;5:2~",
            release: b"\x1b[3;5:3~",
        },
    ];
    for case in cases {
        assert_eq!(
            core.key_input(case.key, case.mods, KeyEventKind::Repeat),
            case.repeat.to_vec(),
            "repeat {}",
            case.name
        );
        assert_eq!(
            core.key_input(case.key, case.mods, KeyEventKind::Release),
            case.release.to_vec(),
            "release {}",
            case.name
        );
    }

    // Press stays byte-for-byte identical regardless of this feature --
    // covered more broadly by
    // `navigation_keys_are_flag_invariant_and_spec_compliant`, spot-checked
    // here alongside the same flags/case set.
    assert_eq!(
        core.key_input(KeyCode::UpArrow, Modifiers::NONE, KeyEventKind::Press),
        b"\x1b[A".to_vec()
    );
}

/// Regression test for `docs/tasks/backlog.md` item 2's PUA-table half:
/// F13-F24 have no legacy encoding at all in the Kitty spec's own
/// "Functional key definitions" table (only dedicated Private-Use-Area
/// `CSI u` codepoints, `57376`-`57387` for F13-F24) — so once any Kitty
/// flag is negotiated, `kitty_override` now reports those instead of
/// termwiz's rxvt-derived legacy numbers (which are simply wrong in that
/// context). With no Kitty flags negotiated at all, those legacy numbers
/// remain exactly as before — a deliberate, documented deviation from
/// kitty's own reference (which always emits PUA codes for F13+, even at
/// zero flags) that the spec text itself explicitly permits; see
/// `kitty_override`'s doc comment and `KITTY_COMPLIANCE`'s "Functional key
/// definitions: F13-F24" row.
#[test]
fn high_function_keys_use_legacy_numbers_without_kitty_flags_and_pua_codes_with_them() {
    // No Kitty flags at all: termwiz's existing (xterm/rxvt-compatible)
    // numbers, unchanged.
    let core = TerminalCore::new(TerminalSize::new(20, 4));
    let legacy_cases: &[(u8, &[u8])] = &[(13, b"\x1b[25~"), (14, b"\x1b[26~"), (24, b"\x1b[45~")];
    for (n, want) in legacy_cases {
        assert_eq!(
            core.key_input(KeyCode::Function(*n), Modifiers::NONE, KeyEventKind::Press),
            want.to_vec(),
            "F{n} with no Kitty flags"
        );
    }

    // Any Kitty flag active: genuine PUA `CSI u` codes instead, decorated
    // with modifiers/event-type exactly like Enter/Tab/Backspace/Escape.
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>1u"); // disambiguate only
    assert_eq!(
        core.key_input(KeyCode::Function(13), Modifiers::NONE, KeyEventKind::Press),
        b"\x1b[57376u".to_vec()
    );
    assert_eq!(
        core.key_input(KeyCode::Function(24), Modifiers::SHIFT, KeyEventKind::Press),
        b"\x1b[57387;2u".to_vec()
    );

    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>3u"); // disambiguate + report-event-types
    assert_eq!(
        core.key_input(KeyCode::Function(13), Modifiers::NONE, KeyEventKind::Repeat),
        b"\x1b[57376;1:2u".to_vec()
    );
    assert_eq!(
        core.key_input(
            KeyCode::Function(13),
            Modifiers::NONE,
            KeyEventKind::Release
        ),
        b"\x1b[57376;1:3u".to_vec()
    );
}

/// UNIMPLEMENTED, structural: F25 and above have no representation at all
/// in termwiz's `KeyCode::Function(n)` encoder — its internal table only
/// covers `n <= 24` (reusing legacy rxvt-style `CSI n~` numbers, which for
/// F1-F12 happen to match the alternate numeric forms Kitty's own spec
/// documents, e.g. `F1` = `1 P or 11 ~`); anything higher hits a `bail!`
/// that `TerminalCore::encode_key`'s `.unwrap_or_default()` silently turns
/// into `""`. `docs/tasks/backlog.md` item 2, resolved for F13-F24: those
/// now report their genuine Private-Use-Area `CSI u` codes
/// (`kitty_override`, `F13` = `57376 u`, ..., `F24` = `57387 u`) once any
/// Kitty flag is negotiated — see
/// `high_function_keys_use_legacy_numbers_without_kitty_flags_and_pua_codes_with_them`
/// — and are reachable from the real UI too (`app::keymap::
/// terminal_key_from_input` now maps `NamedKey::F1..F24`). F25 and up
/// remain both UNIMPLEMENTED here (no PUA table entry — `kitty_override`'s
/// `Function` arm is bounded to `13..=24`, matching termwiz's own
/// `KeyCode::Function` doc: "F1-F24 are possible") and BYPASSED at the app
/// layer (`app::keymap` has no `NamedKey::F25..F35` arm, matching this
/// bug's own explicit scope) — see `KITTY_COMPLIANCE`'s "Functional key
/// definitions: F25-F35" row. Effort estimate for closing this remaining
/// gap: small — extend `kitty_override`'s `Function` range to `13..=35`
/// and add the matching `app::keymap` arms.
#[test]
#[ignore = "structural: termwiz/kitty_override have no F25+ encoding at all, and app::keymap never routes F25+ to core; see report"]
fn very_high_function_keys_are_unimplemented() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>1u");
    assert_eq!(
        core.key_input(KeyCode::Function(25), Modifiers::NONE, KeyEventKind::Press),
        b"\x1b[57388u".to_vec()
    );
}

/// UNIMPLEMENTED, structural: standalone modifier-key presses (bare Shift,
/// Ctrl, Alt, Super...) get dedicated `CSI u` codes under the disambiguate
/// flag per the spec's Functional key definitions table (`LEFT_SHIFT` =
/// `57441 u`, etc.). termwiz's `KeyCode::encode` puts every modifier
/// `KeyCode` variant (`Shift`, `Control`, `Alt`, `Super`, `LeftShift`, ...)
/// in its final catch-all "don't expand to anything" arm unconditionally,
/// ignoring `modes.encoding` entirely. Also moot today: `app::keymap` never
/// constructs a modifier `TermKeyCode` from a bare modifier keypress in the
/// first place. Effort estimate: medium — needs both a termwiz-side (or
/// locally-overridden) PUA code table for every modifier key/side, and new
/// app-layer wiring to recognize bare modifier `KeyEvent`s at all.
#[test]
#[ignore = "structural: termwiz never encodes standalone modifier keypresses, and app::keymap never constructs them; see report"]
fn standalone_modifier_keypresses_are_unimplemented() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>1u");
    assert_eq!(
        core.key_input(KeyCode::LeftShift, Modifiers::SHIFT, KeyEventKind::Press),
        b"\x1b[57441u".to_vec()
    );
}

/// WRONG, structural: under the disambiguate flag, keypad keys are supposed
/// to switch from "reported as their equivalent non-keypad key" (legacy
/// behavior, e.g. `KP_0` sends the same bytes as `Insert`) to their own
/// dedicated Private-Use-Area `CSI u` codes ("All keypad keys are reported
/// as their equivalent non-keypad keys. To distinguish these, use the
/// disambiguate flag" — spec's Legacy functional keys section; `KP_0` =
/// `57399 u` in the Functional key definitions table). termwiz's
/// `KeyCode::Numpad0` encoder never consults `modes.encoding` at all — it
/// always emits the legacy `\x1b[2~` form (coincidentally reasonable
/// *without* disambiguate, since `2~` is also `Insert`'s code, but wrong
/// once disambiguate is active). Moot today regardless: `app::keymap` has
/// no path from any keypad `KeyEvent` to a `TermKeyCode::NumpadN` at all.
/// Effort estimate: small-medium — a `kitty_override`-style PUA table for
/// the keypad keys, gated on the disambiguate flag; the app-layer wiring
/// gap is a separate, larger effort (floem/winit keypad key detection).
#[test]
#[ignore = "structural: termwiz's keypad encoding ignores the Kitty disambiguate flag entirely, and app::keymap has no keypad wiring; see report"]
fn keypad_keys_ignore_disambiguate_flag() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>1u"); // disambiguate
    assert_eq!(
        core.key_input(KeyCode::Numpad0, Modifiers::NONE, KeyEventKind::Press),
        b"\x1b[57399u".to_vec()
    );
}

/// Regression test for the fix to a known, pre-existing compliance gap:
/// with every Kitty flag active (including `REPORT_ALL_KEYS_AS_ESCAPE_CODES`,
/// which per spec turns *every* key, including plain letters, into `CSI u`),
/// a shifted letter run through `TerminalCore::key_input` now produces
/// genuine `CSI u` instead of the bare uppercased legacy byte this test used
/// to document as a known gap (`shift_letter_ignores_kitty_flags_even_with_
/// report_all_keys_active`, before `TerminalCore::encode_key` started
/// special-casing `KeyCode::Char` through `kitty_keyboard::encode_text_key`
/// — see its doc comment and `KITTY_COMPLIANCE`'s former "Report all keys
/// as escape codes (text keys)" BYPASSED row). `97` is `'a'`, the base/
/// unshifted codepoint the spec mandates; `65` (`'A'`) is the "report
/// alternate keys" shifted-key subfield, included here since flags=31
/// negotiates that flag too.
#[test]
fn shift_letter_produces_csi_u_under_report_all_keys() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>31u");

    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::SHIFT, KeyEventKind::Press),
        b"\x1b[97:65;2u".to_vec()
    );
}

/// Truth table for `kitty_keyboard::encode_text_key`'s `CSI u` branch, spot
/// verified against <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>'s
/// own worked example for the general escape code format ("If the user
/// presses, for example, ctrl+shift+a the escape code would be `CSI
/// 97;<modifiers>u`. It must not be `CSI 65;<modifiers>u`"). Only
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` (`0b1000`) is pushed — no
/// `REPORT_ALTERNATE_KEYS`, so no key here carries the alternate-key
/// subfield (see `csi_u_text_key_reports_alternate_for_shifted_letter_only`
/// for that). `KeyCode::Char`'s `char` argument follows termwiz's own
/// convention (base/unshifted char, Shift carried separately in
/// `Modifiers`) — see `kitty_keyboard::encode_text_key`'s doc comment.
#[test]
fn csi_u_text_key_truth_table() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>8u");

    let cases: &[(&str, char, Modifiers, &[u8])] = &[
        ("a", 'a', Modifiers::NONE, b"\x1b[97u"),
        ("Shift+a", 'a', Modifiers::SHIFT, b"\x1b[97;2u"),
        ("Ctrl+a", 'a', Modifiers::CTRL, b"\x1b[97;5u"),
        (
            "Ctrl+Shift+a",
            'a',
            Modifiers::CTRL | Modifiers::SHIFT,
            b"\x1b[97;6u",
        ),
        ("Alt+a", 'a', Modifiers::ALT, b"\x1b[97;3u"),
        ("1", '1', Modifiers::NONE, b"\x1b[49u"),
        ("Ctrl+1", '1', Modifiers::CTRL, b"\x1b[49;5u"),
        // Shifted digit: no base-codepoint inverse available (see
        // KITTY_COMPLIANCE's "shifted digits/punctuation" row), so the
        // shifted codepoint '!' (33) is reported as-is rather than '1' (49).
        ("!", '!', Modifiers::SHIFT, b"\x1b[33;2u"),
    ];
    for (name, c, mods, expected) in cases {
        assert_eq!(
            core.key_input(KeyCode::Char(*c), *mods, KeyEventKind::Press),
            expected.to_vec(),
            "case={name}"
        );
    }
}

/// "Report alternate keys" (`0b100`) only ever emits a shifted-key subfield
/// for the ASCII-letter case (case-folding needs no keyboard-layout data);
/// a shifted digit/punctuation key carries no alternate at all. See
/// `KITTY_COMPLIANCE`'s "Report alternate keys" row.
#[test]
fn csi_u_text_key_reports_alternate_for_shifted_letter_only() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>12u"); // report-alternate-keys (4) + report-all-keys (8)

    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::SHIFT, KeyEventKind::Press),
        b"\x1b[97:65;2u".to_vec(),
        "shifted letter carries the alternate (shifted) codepoint"
    );
    assert_eq!(
        core.key_input(KeyCode::Char('!'), Modifiers::SHIFT, KeyEventKind::Press),
        b"\x1b[33;2u".to_vec(),
        "shifted punctuation has no known alternate, so none is reported"
    );
}

/// Regression guard for `kitty_keyboard::legacy_text_key`: byte-identical
/// to `app::keymap::character_input`/`control_input`'s pre-existing
/// algorithm, which computed these bytes independently in the app layer
/// before routing moved the decision into `TerminalCore` (see
/// `KITTY_COMPLIANCE`'s former "Report all keys as escape codes (text
/// keys)" BYPASSED row). No Kitty flags are pushed here — this exercises
/// exactly the "otherwise" branch `encode_text_key` falls to when
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` isn't negotiated, which is every shell
/// that never opts into the Kitty protocol at all.
///
/// Covers the full printable ASCII range under plain/Shift/Alt, plus every
/// entry in `ctrl_mapping`'s table (this module's canonical, wezterm-
/// derived Ctrl table) under Ctrl — a strictly wider set than
/// `control_input`'s own smaller hand-written table, so e.g. Ctrl+Space now
/// sends NUL where it used to send nothing; letters agree between the two
/// tables already. Also covers the one deliberate mismatch this port keeps
/// rather than "fixes": Ctrl+Alt+<letter> sends the bare Ctrl byte with no
/// `ESC` prefix, because `character_input`'s Ctrl branch returns before
/// ever checking Alt — unlike termwiz's real `Char` encoder (`encode_char`
/// in this file), which does ESC-prefix it.
#[test]
fn legacy_text_key_matches_pre_existing_bytes_over_printable_range_and_ctrl_table() {
    let core = TerminalCore::new(TerminalSize::new(20, 4));

    for c in ('a'..='z').chain('0'..='9') {
        assert_eq!(
            core.key_input(KeyCode::Char(c), Modifiers::NONE, KeyEventKind::Press),
            vec![c as u8],
            "plain {c:?}"
        );
        let shifted = if c.is_ascii_lowercase() {
            vec![c.to_ascii_uppercase() as u8]
        } else {
            vec![c as u8]
        };
        assert_eq!(
            core.key_input(KeyCode::Char(c), Modifiers::SHIFT, KeyEventKind::Press),
            shifted,
            "shift+{c:?}"
        );
        assert_eq!(
            core.key_input(KeyCode::Char(c), Modifiers::ALT, KeyEventKind::Press),
            vec![0x1b, c as u8],
            "alt+{c:?}"
        );
    }

    for c in 'a'..='z' {
        assert_eq!(
            core.key_input(KeyCode::Char(c), Modifiers::CTRL, KeyEventKind::Press),
            vec![c as u8 - b'a' + 1],
            "ctrl+{c:?}"
        );
    }

    let ctrl_cases: &[(char, u8)] = &[
        ('@', 0x00),
        ('`', 0x00),
        (' ', 0x00),
        ('2', 0x00),
        ('[', 0x1b),
        ('3', 0x1b),
        ('{', 0x1b),
        ('\\', 0x1c),
        ('4', 0x1c),
        ('|', 0x1c),
        (']', 0x1d),
        ('5', 0x1d),
        ('}', 0x1d),
        ('^', 0x1e),
        ('6', 0x1e),
        ('~', 0x1e),
        ('_', 0x1f),
        ('7', 0x1f),
        ('/', 0x1f),
        ('8', 0x7f),
        ('?', 0x7f),
    ];
    for &(c, expected) in ctrl_cases {
        assert_eq!(
            core.key_input(KeyCode::Char(c), Modifiers::CTRL, KeyEventKind::Press),
            vec![expected],
            "ctrl+{c:?}"
        );
    }

    // Not in `ctrl_mapping` either: silently swallowed, same as before.
    for c in ['0', '1', '9'] {
        assert!(
            core.key_input(KeyCode::Char(c), Modifiers::CTRL, KeyEventKind::Press)
                .is_empty(),
            "ctrl+{c:?}"
        );
    }

    // Ctrl+Alt: Alt is ignored entirely, matching `character_input`'s
    // pre-existing behavior (see doc comment above).
    assert_eq!(
        core.key_input(
            KeyCode::Char('a'),
            Modifiers::CTRL | Modifiers::ALT,
            KeyEventKind::Press
        ),
        vec![0x01]
    );

    // Super/Cmd alone drops the key entirely (`character_input`'s
    // `modifiers.meta()` check).
    assert!(core
        .key_input(KeyCode::Char('a'), Modifiers::SUPER, KeyEventKind::Press)
        .is_empty());
}
