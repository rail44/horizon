//! Renders a successful reply's [`EnvelopeBody`] to an output stream --
//! human-readable by default, or the raw contract payload under `--json`
//! (task spec: "`--json` で契約ペイロードをそのまま出す"). Takes `out:
//! &mut impl Write` rather than printing directly so tests can capture
//! output into a buffer instead of the process's real stdout.

use std::io::Write;

use horizon_control::contract::{EnvelopeBody, Sessions, State};

pub fn render(body: &EnvelopeBody, json: bool, out: &mut impl Write) {
    if json {
        render_json(body, out);
        return;
    }
    match body {
        EnvelopeBody::Ok => {
            let _ = writeln!(out, "OK");
        }
        EnvelopeBody::Sessions(sessions) => render_sessions(sessions, out),
        EnvelopeBody::State(state) => render_state(state, out),
        EnvelopeBody::Unknown { kind, payload } => {
            let _ = writeln!(out, "(unrecognized server response: kind={kind})");
            let _ = writeln!(out, "{payload}");
        }
        // `Connection::send_request` already folds `Error` into a returned
        // `Err`, and `Hello`/`Invoke`/`Query`/`HelloAck`/`Rejected`/`Error`
        // are otherwise not reply shapes a well-behaved server sends here --
        // rendered generically rather than treated as unreachable, since
        // nothing upstream of this function rules them out structurally.
        other => {
            let _ = writeln!(out, "{other:?}");
        }
    }
}

fn render_json(body: &EnvelopeBody, out: &mut impl Write) {
    match serde_json::to_string_pretty(body) {
        Ok(json) => {
            let _ = writeln!(out, "{json}");
        }
        Err(err) => {
            let _ = writeln!(out, "error: failed to render json: {err}");
        }
    }
}

fn render_sessions(sessions: &Sessions, out: &mut impl Write) {
    if sessions.sessions.is_empty() {
        let _ = writeln!(out, "(no sessions)");
        return;
    }
    for session in &sessions.sessions {
        let attached = if session.attached {
            "attached"
        } else {
            "detached"
        };
        let _ = writeln!(
            out,
            "{}  {}  {}  {}",
            session.session_id, session.kind, attached, session.title
        );
    }
}

fn render_state(state: &State, out: &mut impl Write) {
    let _ = writeln!(out, "tab_count: {}", state.tab_count);
    let _ = writeln!(out, "visible_pane_count: {}", state.visible_pane_count);
    let _ = writeln!(out, "has_active_session: {}", state.has_active_session);
    let _ = writeln!(
        out,
        "detached_session_count: {}",
        state.detached_session_count
    );
    let _ = writeln!(out, "has_pending_approval: {}", state.has_pending_approval);
    let _ = writeln!(out, "has_turn_in_flight: {}", state.has_turn_in_flight);
    let destructive = if state.destructive_commands.is_empty() {
        "(none)".to_string()
    } else {
        state.destructive_commands.join(", ")
    };
    let _ = writeln!(out, "destructive_commands: {destructive}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_control::contract::SessionEntry;

    fn rendered(body: &EnvelopeBody, json: bool) -> String {
        let mut buf = Vec::new();
        render(body, json, &mut buf);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn ok_renders_as_ok() {
        assert_eq!(rendered(&EnvelopeBody::Ok, false), "OK\n");
    }

    #[test]
    fn empty_sessions_renders_a_placeholder() {
        let body = EnvelopeBody::Sessions(Sessions { sessions: vec![] });
        assert_eq!(rendered(&body, false), "(no sessions)\n");
    }

    #[test]
    fn sessions_render_one_line_each() {
        let body = EnvelopeBody::Sessions(Sessions {
            sessions: vec![SessionEntry {
                session_id: "s-1".to_string(),
                kind: "agent".to_string(),
                attached: true,
                title: "agent: fix bug".to_string(),
            }],
        });
        assert_eq!(
            rendered(&body, false),
            "s-1  agent  attached  agent: fix bug\n"
        );
    }

    #[test]
    fn state_renders_every_field() {
        let body = EnvelopeBody::State(State {
            tab_count: 2,
            visible_pane_count: 3,
            has_active_session: true,
            detached_session_count: 1,
            has_pending_approval: false,
            has_turn_in_flight: true,
            destructive_commands: vec!["terminate-session".to_string()],
        });
        let text = rendered(&body, false);
        assert!(text.contains("tab_count: 2"));
        assert!(text.contains("destructive_commands: terminate-session"));
    }

    #[test]
    fn state_with_no_destructive_commands_says_none() {
        let body = EnvelopeBody::State(State {
            tab_count: 0,
            visible_pane_count: 0,
            has_active_session: false,
            detached_session_count: 0,
            has_pending_approval: false,
            has_turn_in_flight: false,
            destructive_commands: vec![],
        });
        assert!(rendered(&body, false).contains("destructive_commands: (none)"));
    }

    #[test]
    fn json_mode_emits_the_kind_and_payload() {
        let body = EnvelopeBody::Sessions(Sessions { sessions: vec![] });
        let text = rendered(&body, true);
        assert!(text.contains("\"kind\": \"sessions\""));
        assert!(text.contains("\"sessions\": []"));
    }
}
