//! The pure decision behind `WorkspaceShell::sync_terminal_focus`, kept in
//! its own small module (rather than colocated in `workspace.rs`) so it
//! stays independently unit-testable.

use horizon_workspace::SessionId;

/// Given whether the window itself has OS focus, the active pane's
/// terminal session (`None` whenever the active pane isn't a terminal —
/// e.g. an agent pane), and the session last reported focused, returns the
/// `(unfocus, focus)` transition pair. `(None, None)` means the composed
/// target hasn't changed since the last call — nothing to send; otherwise
/// `unfocus` is the session that just stopped being the composed target
/// (gets `Focus(false)`) and `focus` is the one that just became it (gets
/// `Focus(true)`), either of which may be absent (losing/gaining focus
/// with nothing on the other side).
pub(crate) fn focus_transition(
    window_active: bool,
    active_terminal_session: Option<SessionId>,
    last_focused_terminal: Option<SessionId>,
) -> (Option<SessionId>, Option<SessionId>) {
    let focused = active_terminal_session.filter(|_| window_active);
    if focused == last_focused_terminal {
        (None, None)
    } else {
        (last_focused_terminal, focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_active_terminal_and_no_prior_focus_sends_nothing() {
        assert_eq!(focus_transition(true, None, None), (None, None));
    }

    #[test]
    fn gaining_an_active_terminal_focuses_it() {
        let session = SessionId::new();
        assert_eq!(
            focus_transition(true, Some(session), None),
            (None, Some(session))
        );
    }

    #[test]
    fn switching_the_active_terminal_unfocuses_the_old_one_and_focuses_the_new_one() {
        let previous = SessionId::new();
        let next = SessionId::new();
        assert_eq!(
            focus_transition(true, Some(next), Some(previous)),
            (Some(previous), Some(next))
        );
    }

    #[test]
    fn an_active_agent_pane_unfocuses_the_previously_active_terminal() {
        let previous = SessionId::new();
        assert_eq!(
            focus_transition(true, None, Some(previous)),
            (Some(previous), None)
        );
    }

    #[test]
    fn losing_window_focus_unfocuses_the_still_active_terminal() {
        let session = SessionId::new();
        assert_eq!(
            focus_transition(false, Some(session), Some(session)),
            (Some(session), None)
        );
    }

    #[test]
    fn regaining_window_focus_refocuses_the_still_active_terminal() {
        let session = SessionId::new();
        assert_eq!(
            focus_transition(true, Some(session), None),
            (None, Some(session))
        );
    }

    #[test]
    fn an_unchanged_active_terminal_sends_nothing() {
        let session = SessionId::new();
        assert_eq!(
            focus_transition(true, Some(session), Some(session)),
            (None, None)
        );
    }
}
