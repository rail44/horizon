//! A session's title from its transcript: its first message from the
//! person, as a tab can show it.

use horizon_workspace::title::derive_session_title;

use crate::agent_model::{AgentFrameItem, MessageRole};

/// The transcript's first real user message text -- the same selection
/// rule [`derive_title_from_items`] applies (blank messages are skipped,
/// as are system-authored ones) -- but *unclamped*: this is the
/// summarizer's input, not a tab label. `None` while no usable message
/// exists yet.
pub fn first_user_message_text(items: &[AgentFrameItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        AgentFrameItem::Message(message) if message.role == MessageRole::User => {
            // `derive_session_title` returning `Some` is exactly the
            // "this text would survive as a title" predicate, reused
            // here so both paths skip the same messages.
            derive_session_title(&message.text).map(|_| message.text.clone())
        }
        _ => None,
    })
}

/// The title text for a transcript: the first user-authored message whose
/// text survives [`derive_session_title`] (blank messages are skipped, as
/// are system-authored ones -- [`MessageRole::TaskNotification`] and the
/// other injected roles deliberately never title a tab), or `None` while
/// no such message exists yet.
pub fn derive_title_from_items(items: &[AgentFrameItem]) -> Option<String> {
    first_user_message_text(items).and_then(|text| derive_session_title(&text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_model::Message;

    // Explicitly-typed builders keep the literals shallow and the
    // assertions' intent readable.
    fn message(role: MessageRole, text: &str) -> AgentFrameItem {
        AgentFrameItem::Message(Message {
            id: text.to_string(),
            role,
            text: text.to_string(),
        })
    }

    fn user_message(text: &str) -> AgentFrameItem {
        message(MessageRole::User, text)
    }

    fn task_notification(text: &str) -> AgentFrameItem {
        message(MessageRole::TaskNotification, text)
    }

    #[test]
    fn the_first_usable_user_message_supplies_the_title() {
        let items = vec![
            task_notification("task done"),
            user_message("  fix the flaky test in\nsession.rs  "),
        ];
        assert_eq!(
            derive_title_from_items(&items),
            Some("fix the flaky test in session.rs".to_string())
        );
    }

    #[test]
    fn a_blank_user_message_is_skipped_for_a_later_usable_one() {
        let items = vec![user_message("   "), user_message("real ask")];
        assert_eq!(
            derive_title_from_items(&items),
            Some("real ask".to_string())
        );
    }

    #[test]
    fn first_user_message_text_is_the_unclamped_source_message() {
        // The summarizer's input must be the raw message -- a 40-char
        // clamp here would summarize the already-truncated label instead
        // of what the user actually asked.
        let long = "please investigate ".repeat(20);
        let items = vec![task_notification("task done"), user_message(&long)];
        assert_eq!(first_user_message_text(&items), Some(long.clone()));
        assert!(derive_title_from_items(&items).unwrap().chars().count() < long.chars().count());
    }

    #[test]
    fn first_user_message_text_follows_the_same_skip_rules_as_the_title() {
        let items = vec![
            user_message("   "),
            task_notification("task done"),
            user_message("real ask"),
        ];
        assert_eq!(
            first_user_message_text(&items),
            Some("real ask".to_string())
        );
        assert_eq!(
            first_user_message_text(&[task_notification("task done")]),
            None
        );
    }

    #[test]
    fn a_transcript_without_a_user_message_yields_no_title() {
        assert_eq!(
            derive_title_from_items(&[AgentFrameItem::MemoryCheckpointMissed]),
            None
        );
        assert_eq!(derive_title_from_items(&[]), None);
    }
}
