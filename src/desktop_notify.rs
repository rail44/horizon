//! Desktop notification dispatch — the OS-facing tail of the terminal
//! OSC 9/777 pipeline. `crates/horizon-terminal-core` extracts the request
//! from the PTY stream (`TerminalUpdate::Notification`), the workspace
//! gate (`WorkspaceShell::should_surface_notification`) decides whether
//! the user isn't already looking at the session, and this module hands
//! the request to macOS's notification center.
//!
//! The API is `mac-usernotifications`' UNUserNotificationCenter wrapper,
//! whose one hard requirement is structural: the process must run from an
//! `.app` bundle with a `CFBundleIdentifier`, code-signed at least ad-hoc.
//! A bare `target/debug/horizon` (what `just dev` launches) is *silently*
//! ignored by macOS 26's notification routing — `check_bundle` catches the
//! missing bundle id up front and points at `just dev-bundle` instead of
//! failing invisibly. (The legacy NSUserNotification API older wrappers
//! used needs no bundle, but stopped being delivered on macOS 26 entirely,
//! which is why there is no fallback to it.)
//!
//! Called from the workspace's notification pump — a background-executor
//! task, never the main thread — which is also why the first-use
//! authorization request (`request_auth`, which blocks until the user
//! answers the permission dialog) lives here rather than on any UI path.

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::Once;

    /// macOS asks once per bundle identifier; every later `request_auth`
    /// is a cheap no-op confirmation. One process-wide attempt is enough —
    /// a denial is sticky until the user flips it in System Settings, and
    /// re-prompting on every notification would be hostile.
    static AUTH_REQUESTED: Once = Once::new();

    pub(super) fn dispatch(title: Option<&str>, body: &str) {
        if let Err(error) = mac_usernotifications::check_bundle() {
            eprintln!(
                "desktop notification dropped (not a bundle-signed process: {error}); \
                 launch via `just dev-bundle` for the .app wrapper \
                 UNUserNotificationCenter requires"
            );
            return;
        }
        AUTH_REQUESTED.call_once(|| {
            if let Err(error) = mac_usernotifications::blocking::request_auth() {
                eprintln!("desktop notification authorization failed: {error}");
            }
        });
        let mut notification = mac_usernotifications::Notification::new().message(body);
        if let Some(title) = title {
            notification = notification.title(title);
        }
        if let Err(error) = notification.send_blocking() {
            eprintln!("failed to post desktop notification: {error}");
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// Other platforms have no dispatcher yet — Linux would want
    /// `notify-rust` / `org.freedesktop.Notifications`. The gate upstream
    /// still works; only the OS hop is missing, so log instead of losing
    /// the request silently.
    pub(super) fn dispatch(title: Option<&str>, body: &str) {
        match title {
            Some(title) => eprintln!("[notification] {title}: {body}"),
            None => eprintln!("[notification] {body}"),
        }
    }
}

/// Post one desktop notification. Fire-and-forget: delivery problems are
/// logged, never propagated — a notification is best-effort by nature.
pub(crate) fn notify(title: Option<String>, body: String) {
    imp::dispatch(title.as_deref(), &body);
}
