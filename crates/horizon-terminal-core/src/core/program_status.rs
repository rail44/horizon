//! OSC 7501, the Program Status Protocol: what a program in the terminal
//! says it is doing (<https://www.superlogical.com/rex/docs/build/program-status>,
//! revision 0.4).
//!
//! A program reports with `ESC ] 7501 ; key=value(:key=value)* ST`, and
//! asks whether the terminal reads those reports with `ESC ] 7501 ; ? ST`.
//! Some programs report only to a terminal that answered the question, so
//! the answer ([`QUERY_REPLY`]) is what turns their reports on.
//!
//! Only the root record is read. A record with an `id` is one of the
//! program's own pieces of work (a background command, a subagent); those
//! reports and their clears are dropped, so that the end of one of them is
//! never taken for the end of the program's own state.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

/// What the terminal answers a query with: the states and the kinds of
/// `blocked` it reads.
pub(super) const QUERY_REPLY: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// The longest body the protocol allows, in bytes: 4096 for the whole
/// sequence, less `ESC ] 7501 ;` and `ESC \`.
const MAX_BODY: usize = 4087;
/// The longest `app`, in bytes.
const MAX_APP: usize = 32;
/// The longest `msg` once decoded, in bytes.
const MAX_MSG: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProgramState {
    /// At rest, waiting for the next instruction.
    Idle,
    Working,
    /// Finished a piece of work the user has not looked at yet.
    Done,
    /// Cannot go on until the user does something.
    Blocked,
    /// Failed and stopped.
    Error,
}

/// What a blocked program is waiting for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BlockedKind {
    Permission,
    Question,
    Auth,
}

/// A program's report of its own state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProgramStatus {
    pub state: ProgramState,
    /// Only with [`ProgramState::Blocked`], and not always then.
    pub kind: Option<BlockedKind>,
    /// The program's name for itself, such as `claude-code`.
    pub app: Option<String>,
    /// Why, or what about, in the program's words.
    pub msg: Option<String>,
}

/// A change to what the terminal knows of the program's state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProgramStatusUpdate {
    Set(ProgramStatus),
    /// The program took its report back, as one does on its way out.
    Clear,
}

/// One OSC 7501 sequence, read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Message {
    /// The program asks whether its reports are read.
    Query,
    Update(ProgramStatusUpdate),
}

/// Reads the body between `ESC ] 7501 ;` and the terminator. `None` is a
/// sequence to drop: one over the protocol's limits, one without a state
/// this terminal reads, or one about a record other than the root.
pub(super) fn parse(body: &[u8]) -> Option<Message> {
    if body.len() > MAX_BODY {
        return None;
    }
    let body = std::str::from_utf8(body).ok()?;
    if body == "?" {
        return Some(Message::Query);
    }
    let (mut state, mut kind, mut app, mut msg) = (None, None, None, None);
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "id" if !value.is_empty() => return None,
            "state" => state = Some(value),
            "kind" => kind = Some(value),
            "app" => app = Some(value),
            "msg" => msg = Some(value),
            _ => {}
        }
    }
    let state = match state? {
        "clear" => return Some(Message::Update(ProgramStatusUpdate::Clear)),
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "done" => ProgramState::Done,
        "blocked" => ProgramState::Blocked,
        "error" => ProgramState::Error,
        _ => return None,
    };
    let kind = match kind.filter(|_| state == ProgramState::Blocked) {
        Some("permission") => Some(BlockedKind::Permission),
        Some("question") => Some(BlockedKind::Question),
        Some("auth") => Some(BlockedKind::Auth),
        _ => None,
    };
    Some(Message::Update(ProgramStatusUpdate::Set(ProgramStatus {
        state,
        kind,
        app: app
            .filter(|app| !app.is_empty() && app.len() <= MAX_APP)
            .map(str::to_string),
        msg: msg.and_then(decoded),
    })))
}

/// A `msg` as text: base64 of UTF-8, with or without its padding.
fn decoded(value: &str) -> Option<String> {
    let bytes = (STANDARD.decode(value))
        .or_else(|_| STANDARD_NO_PAD.decode(value))
        .ok()?;
    if bytes.is_empty() || bytes.len() > MAX_MSG {
        return None;
    }
    let text: String = (String::from_utf8_lossy(&bytes).chars())
        .filter(|c| !c.is_control())
        .collect();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(body: &str) -> ProgramStatus {
        match parse(body.as_bytes()) {
            Some(Message::Update(ProgramStatusUpdate::Set(status))) => status,
            other => panic!("{body:?} read as {other:?}, not as a report"),
        }
    }

    #[test]
    fn a_question_mark_alone_is_the_query() {
        assert_eq!(parse(b"?"), Some(Message::Query));
        // What a terminal answers with is not a question to this one.
        assert_eq!(parse(b"?:states=idle"), None);
    }

    #[test]
    fn a_report_carries_its_state_and_who_sent_it() {
        let status = set("state=working:app=claude-code");
        assert_eq!(status.state, ProgramState::Working);
        assert_eq!(status.app.as_deref(), Some("claude-code"));
        assert_eq!(status.kind, None);
        assert_eq!(status.msg, None);
    }

    #[test]
    fn blocked_says_what_it_waits_for() {
        let msg = STANDARD.encode("approve Write: notes.md");
        let status = set(&format!(
            "state=blocked:app=codex:kind=permission:msg={msg}"
        ));
        assert_eq!(status.state, ProgramState::Blocked);
        assert_eq!(status.kind, Some(BlockedKind::Permission));
        assert_eq!(status.msg.as_deref(), Some("approve Write: notes.md"));
        assert_eq!(set("state=blocked").kind, None);
        assert_eq!(set("state=blocked:kind=other").kind, None);
    }

    #[test]
    fn a_kind_means_nothing_outside_blocked() {
        assert_eq!(set("state=working:kind=question").kind, None);
    }

    #[test]
    fn the_keys_come_in_any_order_and_unknown_ones_are_passed_over() {
        let status = set("progress=40:app=x:later=1:state=done:novalue");
        assert_eq!(status.state, ProgramState::Done);
        assert_eq!(status.app.as_deref(), Some("x"));
    }

    #[test]
    fn a_record_with_an_id_is_not_the_programs_own() {
        assert_eq!(parse(b"state=working:app=claude-code:id=bt7g05417"), None);
        // The end of a background command is not the end of the program.
        assert_eq!(parse(b"state=clear:id=bt7g05417"), None);
    }

    #[test]
    fn clear_without_an_id_takes_the_report_back() {
        assert_eq!(
            parse(b"state=clear"),
            Some(Message::Update(ProgramStatusUpdate::Clear))
        );
    }

    #[test]
    fn what_is_not_a_report_is_dropped() {
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"app=x"), None);
        assert_eq!(parse(b"state=sleeping"), None);
        assert_eq!(parse(b"state=\xff"), None);
        assert_eq!(parse(&[b'a'; MAX_BODY + 1]), None);
    }

    #[test]
    fn a_message_is_text_without_control_characters_and_within_the_limit() {
        let msg = STANDARD_NO_PAD.encode("line one\nline\x1b two");
        assert_eq!(
            set(&format!("state=error:msg={msg}")).msg.as_deref(),
            Some("line oneline two")
        );
        assert_eq!(set("state=error:msg=!!!").msg, None);
        let long = STANDARD.encode("x".repeat(MAX_MSG + 1));
        assert!(long.len() < MAX_BODY);
        assert_eq!(set(&format!("state=error:msg={long}")).msg, None);
        assert_eq!(set(&format!("state=idle:app={}", "a".repeat(33))).app, None);
    }
}
