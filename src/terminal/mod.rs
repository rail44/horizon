mod core;
mod session;
mod types;
pub mod view;

pub use core::{TerminalCore, TerminalEvents};
pub use session::{
    initial_terminal_text, TerminalCommand, TerminalSession, TerminalSessionError, TerminalUpdate,
};
pub use types::{
    TerminalCursor, TerminalFrame, TerminalLine, TerminalMouseButton, TerminalMouseKind,
    TerminalMouseModifiers, TerminalMouseReport, TerminalScroll, TerminalSelectionPoint,
    TerminalSize, TerminalSpan,
};

#[cfg(test)]
pub(crate) use session::terminal_command;
#[cfg(test)]
pub(crate) use types::DEFAULT_FG;

#[cfg(test)]
mod tests {
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
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt(b"hello\r\n\x1b[31mred\x1b[0m");

        let snapshot = core.snapshot_text();
        assert!(snapshot.contains("hello"));
        assert!(snapshot.contains("red"));
    }

    #[test]
    fn kitty_keyboard_mode_switches_termwiz_encoding() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt(b"\x1b[>1u");

        let encoded = core.encode_key(KeyCode::Escape, Modifiers::NONE, true);
        assert!(!encoded.is_empty());
    }

    #[test]
    fn key_up_events_do_not_emit_legacy_input() {
        let core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        let encoded = core.encode_key(KeyCode::Char('a'), Modifiers::NONE, false);
        assert_eq!(encoded, "");
    }

    #[test]
    fn terminal_session_runs_shell_command() {
        let session = TerminalSession::spawn(TerminalSize { cols: 80, rows: 12 })
            .expect("terminal session should spawn");
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
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
        core.write_vt(b"\x1b[31mred\x1b[0m plain");

        let frame = core.snapshot_frame();
        assert!(frame.text.contains("red plain"));
        let first_line = &frame.lines[0];
        assert!(first_line
            .spans
            .iter()
            .any(|span| { span.text == "r" && span.fg == [224, 108, 117] }));
        assert!(first_line
            .spans
            .iter()
            .any(|span| { span.text == "p" && span.fg == DEFAULT_FG }));
    }

    #[test]
    fn vt_stream_tracks_wide_character_columns() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 4 });
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
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
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
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
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
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
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
        let core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });

        assert_eq!(
            core.handle_mouse_report(test_mouse(TerminalMouseKind::Press)),
            None
        );
    }

    #[test]
    fn sgr_mouse_mode_click_sends_press_and_release_reports() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
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
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
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
        let core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });

        assert_eq!(core.paste_input("hello\n"), b"hello\n".to_vec());
    }

    #[test]
    fn paste_wraps_text_when_bracketed_paste_is_enabled() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"\x1b[?2004h");

        assert_eq!(
            core.paste_input("hello\n"),
            b"\x1b[200~hello\n\x1b[201~".to_vec()
        );
    }

    #[test]
    fn selection_to_string_uses_alacritty_selection() {
        let mut core = TerminalCore::new(TerminalSize { cols: 20, rows: 3 });
        core.write_vt(b"hello world");

        core.start_selection(TerminalSelectionPoint { row: 0, col: 0 });
        core.update_selection(TerminalSelectionPoint { row: 0, col: 4 });

        assert_eq!(core.selected_text(), Some("hello".to_string()));
    }

    #[test]
    fn terminal_command_sanitizes_emulator_environment() {
        let cmd = terminal_command("/bin/sh");

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
}
