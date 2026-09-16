//! Desktop notification dispatch — the OS-facing tail of the terminal
//! OSC 9/777 pipeline. `crates/horizon-terminal-core` extracts the request
//! from the PTY stream (`TerminalUpdate::Notification`), the workspace
//! gate (`WorkspaceShell::should_surface_notification`) decides whether
//! the user isn't already looking at the session, and this module hands
//! the request to the platform's notification center through gpui's
//! unified system-notification API (`App::show_system_notification`):
//! macOS via UNUserNotificationCenter, Linux via
//! `org.freedesktop.Notifications` (gpui's notify-rust/zbus backend — one
//! thread per posted notification, no async-runtime coupling).
//!
//! Both backends share one response contract: a banner-body activation
//! arrives as `SystemNotificationResponse { action_id: None }` (macOS
//! maps `UNNotificationDefaultActionIdentifier`, Linux the `"default"`
//! action key). Horizon posts no action buttons, so no `Some(action_id)`
//! response can originate from one of our posts. Dismissals and expiries
//! produce no response at all — the click answer is the
//! `on_system_notification_response` router installed once by
//! `WorkspaceShell::wire_notification_responses`, which maps the tag
//! back to the session and calls `reveal_session`; nothing is parked
//! awaiting a dismissal.
//!
//! Platform requirements (both degrade to a log line, never a crash):
//! - macOS delivers only from a bundled, (ad-hoc) code-signed `.app`
//!   (`just dev-bundle`); gpui's bundle guard detects a bare
//!   `target/debug/horizon` and disables the stack before the framework's
//!   not-in-a-bundle abort can fire. First-use authorization is requested
//!   asynchronously and never blocks the UI thread.
//! - Linux needs a session bus with a notification daemon; without one
//!   the post logs and drops.
//!
//! Both report through the `log` facade — `main.rs` installs the stderr
//! backend so those diagnostics are actually visible.

use gpui::AsyncApp;
use horizon_workspace::SessionId;

/// Posts one notification on behalf of `session_id`. Fire-and-forget:
/// gpui's platform backend owns delivery and its failure reporting; the
/// pump has already gated on visibility. The tag is the session id, so
/// the response router needs no per-post state.
pub(crate) fn post(session_id: SessionId, title: Option<String>, body: String, cx: &AsyncApp) {
    // OSC 9 carries a body only; the summary line is shown either way,
    // so fall back to the app name.
    let title = title.unwrap_or_else(|| "Horizon".to_string());
    // Terminal-controlled text: many daemons render the body as markup
    // (the XDG spec's body-markup capability), so escape it rather than
    // let PTY bytes forge links or formatting. macOS takes plain text and
    // is unaffected by the extra entities.
    let body = escape_markup(&body);
    cx.update(|app| {
        app.show_system_notification(gpui::SystemNotification {
            tag: notification_tag(session_id).into(),
            title: title.into(),
            body: body.into(),
            actions: vec![],
        });
    });
}

/// The tag posted with every notification: the session id in its UUID
/// form, so the response router can find the session without extra
/// state.
pub(crate) fn notification_tag(session_id: SessionId) -> String {
    session_id.as_uuid().to_string()
}

/// Inverse of [`notification_tag`].
pub(crate) fn session_from_tag(tag: &str) -> Option<SessionId> {
    uuid::Uuid::parse_str(tag).ok().map(SessionId::from_uuid)
}

/// Escapes the XML entities XDG notification bodies interpret (`&`, `<`,
/// `>`). Deliberately minimal: the body is terminal-controlled text, so
/// the goal is only to keep PTY bytes from being read as markup.
fn escape_markup(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through_unescaped() {
        assert_eq!(
            escape_markup("build finished in 3.2s"),
            "build finished in 3.2s"
        );
        // Multibyte text must survive byte-for-byte.
        assert_eq!(escape_markup("ビルド完了"), "ビルド完了");
    }

    #[test]
    fn markup_metacharacters_are_escaped() {
        assert_eq!(
            escape_markup("a & b <link>tail</link>"),
            "a &amp; b &lt;link&gt;tail&lt;/link&gt;"
        );
    }

    #[test]
    fn tag_round_trips_through_the_session_id() {
        let session_id = SessionId::new();
        assert_eq!(
            session_from_tag(&notification_tag(session_id)),
            Some(session_id)
        );
        // Not every string is a tag: a malformed one parses to nothing.
        assert_eq!(session_from_tag("not-a-uuid"), None);
    }
}
