//! The session's title: its first message summarized by the small model
//! (`config.toml`'s `small_model`), recorded as `Event::SessionTitled` and
//! said to the client as the session's information (`session_info_update`'s
//! `title`, and `session/list`'s). A session has one at most; without a
//! small model it has none, and the client names it as it will.

use std::sync::Arc;

use horizon_agent::auxiliary::AuxiliaryClient;
use horizon_agent::contract::{Command, Event, MessageRole, SessionId};
use horizon_agent::live::LiveState;
use horizon_agent::summarize::summarize_session_title;

use super::events::persist_and_send_session_event;
use super::state::{AgentHostState, lock_unpoisoned};

/// The title recorded last in `history`.
fn title_in(history: &[Event]) -> Option<&str> {
    history.iter().rev().find_map(|event| match event {
        Event::SessionTitled { title } => Some(title.as_str()),
        _ => None,
    })
}

/// Gives the session's entry the title its history has.
pub(super) fn restore(state: &AgentHostState, session_id: SessionId, history: &[Event]) {
    let Some(title) = title_in(history) else {
        return;
    };
    if let Some(entry) = lock_unpoisoned(&state.sessions).get_mut(&session_id) {
        entry.title = Some(title.to_string());
    }
}

/// Asks the small model, off the session's thread, for the title of a
/// session that has none and was not asked for one, from its first message:
/// `text` when the session has had none before it. The answer comes back to
/// the session as [`Command::TitleSession`].
pub(super) fn ask(
    state: &Arc<AgentHostState>,
    live_state: &LiveState,
    session_id: SessionId,
    text: &str,
) {
    let events = live_state.events();
    if title_in(&events).is_some() {
        return;
    }
    let Some(config) = lock_unpoisoned(&state.agent_config).auxiliary.clone() else {
        return;
    };
    let first = (events.iter())
        .find_map(|event| match event {
            Event::MessageCommitted(message) if message.role == MessageRole::User => {
                Some(message.text.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| text.to_string());
    let inbound = {
        let mut sessions = lock_unpoisoned(&state.sessions);
        let Some(entry) = sessions
            .get_mut(&session_id)
            .filter(|entry| !entry.title_asked)
        else {
            return;
        };
        entry.title_asked = true;
        entry.inbound.clone()
    };
    std::thread::spawn(move || {
        let client = AuxiliaryClient::new(config);
        let Some(title) = summarize_session_title(&client, &first) else {
            return;
        };
        let _ = inbound.send(Command::TitleSession { title });
    });
}

/// Records the title, unless the session has one already.
pub(super) fn record(
    state: &AgentHostState,
    live_state: &LiveState,
    session_id: SessionId,
    title: String,
) {
    let title = title.trim().to_string();
    if title.is_empty() || title_in(&live_state.events()).is_some() {
        return;
    }
    if persist_and_send_session_event(
        state,
        live_state,
        session_id,
        Event::SessionTitled {
            title: title.clone(),
        },
    ) && let Some(entry) = lock_unpoisoned(&state.sessions).get_mut(&session_id)
    {
        entry.title = Some(title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_title_in_the_history_is_the_sessions() {
        let titled = |title: &str| Event::SessionTitled {
            title: title.into(),
        };
        assert_eq!(title_in(&[]), None);
        assert_eq!(
            title_in(&[titled("first"), Event::SessionResumed, titled("second")]),
            Some("second")
        );
    }
}
