use std::time::{Duration, Instant};

use termwiz::input::{KeyCode, Modifiers};

use super::*;

#[test]
fn terminal_intro_mentions_backends() {
    let text = initial_terminal_text();
    assert!(text.contains("portable-pty"));
    assert!(text.contains("alacritty_terminal"));
    assert!(text.contains("termwiz"));
}

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

    let encoded = core.encode_key(KeyCode::Escape, Modifiers::NONE, true);
    assert!(!encoded.is_empty());
}

#[test]
fn key_up_events_do_not_emit_legacy_input() {
    let core = TerminalCore::new(TerminalSize::new(20, 4));
    let encoded = core.encode_key(KeyCode::Char('a'), Modifiers::NONE, false);
    assert_eq!(encoded, "");
}

#[test]
fn terminal_session_runs_shell_command() {
    let session =
        TerminalSession::spawn(TerminalSize::new(80, 12)).expect("terminal session should spawn");
    let tx = session.sender();
    let rx = session.updates();

    tx.send(TerminalCommand::Input(
        b"printf horizon-terminal-ok\\n\r".to_vec(),
    ))
    .expect("input should be sent");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_output = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(TerminalUpdate::Snapshot(snapshot)) => {
                if snapshot.text.contains("horizon-terminal-ok") {
                    saw_output = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }

    let _ = tx.send(TerminalCommand::Input(b"exit\r".to_vec()));
    let _ = tx.send(TerminalCommand::Shutdown);

    assert!(saw_output, "terminal session did not render shell output");
}

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
        .any(|span| { span.text == "r" && span.fg == [224, 108, 117] }));
    assert!(first_line.spans.iter().any(|span| {
        span.text == "p" && span.fg == crate::terminal::config::resolved_colors().foreground
    }));
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
fn selection_to_string_uses_alacritty_selection() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 3));
    core.write_vt(b"hello world");

    core.start_selection(TerminalSelectionPoint { row: 0, col: 0 });
    core.update_selection(TerminalSelectionPoint { row: 0, col: 4 });

    assert_eq!(core.selected_text(), Some("hello".to_string()));
}

#[test]
fn terminal_command_sanitizes_emulator_environment() {
    let cmd = terminal_command("/bin/sh", &[], "xterm-kitty");

    assert_eq!(
        cmd.get_env("TERM").and_then(|v| v.to_str()),
        Some("xterm-kitty")
    );
    assert_eq!(
        cmd.get_env("COLORTERM").and_then(|v| v.to_str()),
        Some("truecolor")
    );
    assert_eq!(
        cmd.get_env("TERM_PROGRAM").and_then(|v| v.to_str()),
        Some("horizon")
    );
    assert_eq!(
        cmd.get_env("TERM_PROGRAM_VERSION").and_then(|v| v.to_str()),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(cmd.get_env("GHOSTTY_RESOURCES_DIR"), None);
    assert_eq!(cmd.get_env("KITTY_LISTEN_ON"), None);
    assert_eq!(cmd.get_env("WEZTERM_PANE"), None);
    assert_eq!(cmd.get_env("TMUX"), None);
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

#[test]
fn osc11_query_reports_configured_background_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b]11;?\x07");

    let bg = crate::terminal::config::resolved_colors().background;
    let expected = format!(
        "\x1b]11;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x07",
        bg[0], bg[1], bg[2]
    );
    assert_eq!(events.pty_writes, vec![expected.into_bytes()]);
}

#[test]
fn osc10_query_reports_configured_foreground_color() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    let events = core.write_vt(b"\x1b]10;?\x07");

    let fg = crate::terminal::config::resolved_colors().foreground;
    let expected = format!(
        "\x1b]10;rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}\x07",
        fg[0], fg[1], fg[2]
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
/// `terminal::core::input` for the fix.
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
        assert_eq!(core.key_input(key, mods, true), want.to_vec());
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

    // flags = 0b1 (disambiguate only) and 0b11 (+ report event types):
    // spec's named exception keeps Enter/Tab/Backspace exactly as legacy
    // mode, regardless of modifier; Esc alone is promoted to `CSI 27u`.
    // Critically: no `CSI 27;mods;codepoint~` (the bug) anywhere.
    for flags in [0b1u32, 0b11] {
        let mut core = TerminalCore::new(TerminalSize::new(20, 4));
        push_flags(&mut core, flags);
        for (name, key, mods) in cases {
            let expected: &[u8] = match *name {
                "Enter" | "Shift+Enter" | "Ctrl+Enter" => b"\r",
                "Alt+Enter" => b"\x1b\r",
                "Tab" => b"\t",
                "Shift+Tab" => b"\x1b[Z",
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

/// Documents a known, pre-existing compliance gap that is NOT part of the
/// Shift+Enter bug this module's truth table fixes: even with every Kitty
/// flag active (including `REPORT_ALL_KEYS_AS_ESCAPE_CODES`, which per spec
/// should turn *every* key, including plain letters, into `CSI u`), a
/// shifted letter run through `TerminalCore::key_input` directly still
/// yields the bare uppercased legacy byte — `kitty_override` doesn't
/// special-case `KeyCode::Char`, and termwiz's own `Char` branch in
/// `KeyCode::encode` ignores `modes.encoding` entirely once `mods` is empty
/// (which it is here, after termwiz folds Shift into the uppercase letter
/// and strips the modifier). In practice this path isn't reachable from
/// Horizon's real UI anyway: `app::keymap::character_input` sends shifted
/// letters as raw literal bytes without ever consulting the terminal's
/// negotiated Kitty state, for every flag combination. Fixing "report all
/// keys" for text keys would mean routing that separate path through the
/// terminal's live mode, which is a larger change than this bug fix calls
/// for.
#[test]
fn shift_letter_ignores_kitty_flags_even_with_report_all_keys_active() {
    let mut core = TerminalCore::new(TerminalSize::new(20, 4));
    core.write_vt(b"\x1b[>31u");

    assert_eq!(
        core.key_input(KeyCode::Char('a'), Modifiers::SHIFT, true),
        b"A".to_vec()
    );
}
