//! Desktop notifications: the OS-facing end of what a program in a
//! terminal asks for with OSC 9 or OSC 777. `horizon-terminal-core` reads
//! the request out of the PTY's stream, the shell decides whether the
//! owner is looking at that session already, and this hands what is left
//! to the desktop's notification service.
//!
//! On Linux that is `org.freedesktop.Notifications`; a click on the
//! banner's body comes back as the session to show. Without a session bus
//! or a service on it a post is reported on stderr and dropped.

use std::sync::mpsc::{Receiver, Sender};

use horizon_workspace::SessionId;

use crate::wake::Wake;

/// The banners clicked since they were last asked for.
pub struct Clicked(Receiver<SessionId>);

impl Clicked {
    /// The sessions whose banners were clicked since the last call.
    pub fn take(&self) -> Vec<SessionId> {
        self.0.try_iter().collect()
    }
}

/// Posts notifications, each from a thread of its own.
#[derive(Clone)]
pub struct Poster {
    clicked: Sender<SessionId>,
    wake: Wake,
}

/// A poster, and where the clicks on what it posts arrive. `wake` has the
/// page polled when one does.
pub fn poster(wake: Wake) -> (Poster, Clicked) {
    let (clicked, clicks) = std::sync::mpsc::channel();
    (Poster { clicked, wake }, Clicked(clicks))
}

impl Poster {
    /// Posts one notification on behalf of `session`.
    pub fn post(&self, session: SessionId, title: Option<String>, body: &str) {
        // OSC 9 carries a body only; the summary line is shown either way.
        let title = title.unwrap_or_else(|| "Horizon".to_string());
        // The text is the terminal's: many services read a body as markup.
        let body = escape_markup(body);
        let this = self.clone();
        std::thread::spawn(move || {
            let mut notification = notify_rust::Notification::new();
            notification.summary(&title).body(&body);
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                // The banner's body as the one action there is.
                notification.action("default", "default");
                match notification.show() {
                    Ok(shown) => shown.wait_for_action(|action| {
                        if action == "default" {
                            let _ = this.clicked.send(session);
                            this.wake.wake();
                        }
                    }),
                    Err(error) => eprintln!("could not post a notification: {error}"),
                }
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            {
                let _ = (&this, session);
                if let Err(error) = notification.show() {
                    eprintln!("could not post a notification: {error}");
                }
            }
        });
    }
}

/// Escapes what a notification's body is read as markup by (`&`, `<`,
/// `>`): no more than keeps the PTY's bytes from being read as markup.
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
    use super::escape_markup;

    #[test]
    fn plain_text_passes_through_and_markup_is_escaped() {
        assert_eq!(
            escape_markup("build finished in 3.2s"),
            "build finished in 3.2s"
        );
        assert_eq!(escape_markup("ビルド完了"), "ビルド完了");
        assert_eq!(
            escape_markup("a & b <link>tail</link>"),
            "a &amp; b &lt;link&gt;tail&lt;/link&gt;"
        );
    }
}
