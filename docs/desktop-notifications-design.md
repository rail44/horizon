# Desktop Notifications Design

Status: implemented (2026-09-16). Scope: the shell's desktop-notification
pipeline — terminal OSC 9/777 requests reaching the OS notification
center on macOS and Linux through gpui's unified system-notification
API.

## 1. Pipeline (end to end)

1. **Extraction** — a process writing OSC 9 (`ESC ] 9 ; body BEL/ST`) or
   OSC 777 (`ESC ] 777 ; notify ; title ; body BEL/ST`) into a
   Horizon terminal's PTY gets picked up by
   `crates/horizon-terminal-core/src/core/osc_notify.rs`
   (`OscNotificationScanner`, streaming, 64 KiB payload cap, lossy
   UTF-8). The VT parser itself does not know these sequences, which is
   why the scanner feeds ahead of it. Requests travel the daemon→client
   attachment channel as `TerminalUpdate::Notification`
   (`TerminalNotification { title: Option<String>, body: String }`,
   wire protocol v20 — this design changed nothing on the wire).
2. **Gate** — the shell-owned pump (`WorkspaceShell::
   wire_terminal_notifications`, `src/workspace/session_lifecycle.rs`)
   consults `WorkspaceShell::should_surface_notification`
   (`src/workspace/mod.rs`): surface when the window is unfocused, or
   when the notifying session is not the active terminal pane (a
   background pane finishing a build still earns a banner; the pane
   being typed in does not). Nothing surfaces during workspace restore.
3. **Dispatch** — `crate::desktop_notify::post` builds a
   `gpui::SystemNotification` and hands it to
   `App::show_system_notification`. Fire-and-forget: the pump never
   waits, so one unnoticed banner can never stall later ones.
4. **Click answer** — `WorkspaceShell::wire_notification_responses`
   registers `App::on_system_notification_response` once at startup. A
   banner-body activation (`action_id: None`) carries the tag, which is
   the posting session's id (`desktop_notify::notification_tag` /
   `session_from_tag` round-trip through the UUID text form); the
   router answers with `reveal_session` — activate the session's pane,
   refocus, foreground the window. A detached or terminated session has
   no pane to reveal, so nothing happens (no window steal).

## 2. Why gpui's API, not our own notify-rust call

Both platform backends already live inside the gpui-pre platform crates
in our dependency graph:

- **Linux** (`gpui-pre-linux`): `system_notifications` posts via
  `notify-rust` (zbus 5 backend, pure Rust — no libdbus C dependency)
  on one thread per posted notification, bridging responses back to the
  main thread over an mpsc channel. `show` + `wait_for_action` block,
  which is exactly why they run off the UI thread — the pattern the
  crate author recommends for non-async contexts and what gpui itself
  ships.
- **macOS** (`gpui-pre-macos`): UNUserNotificationCenter via typed
  `objc2-user-notifications` bindings. Bundle guard (a bare, un-bundled
  binary disables the stack with a log line instead of the framework's
  `bundleProxyForCurrentProcess is nil` abort), asynchronous
  once-per-run authorization request, tag-as-request-identifier (same
  tag replaces the older notification).

Converging on the framework API replaced the previous bespoke path
(`mac-usernotifications` + a per-post click-watcher future):

- **One code path for every platform** — no `cfg` split in the pump, no
  per-post future, no detached watcher task, no oneshot plumbing.
- **It fixes a real bug by construction**: the old macOS path called
  `mac_usernotifications::blocking::request_auth()` on the main thread
  (its module comment claimed the opposite), parking the UI until the
  first-run permission dialog was answered. gpui's authorization is
  asynchronous.
- **Dependency shrink**: `mac-usernotifications` (and its
  `objc2-user-notifications` closure) left the lockfile; nothing was
  added — `log` was already in the graph.

## 3. Response semantics

`SystemNotificationResponse { tag, action_id: Option<SharedString> }`:

- `action_id: None` — the user activated the notification body (macOS
  `UNNotificationDefaultActionIdentifier`, Linux the `"default"` action
  key). Horizon posts no action buttons, so no `Some(_)` response can
  originate from one of our posts; the router ignores them anyway.
- Dismissals and expiries produce **no response** on any platform.
  Nothing is parked awaiting them — this is why the old design's
  "future resolves `false` on close" contract had to go: with gpui's
  fire-and-forget model there is no per-post task left to leak, and a
  dismissal requires no action.

## 4. Visibility of failures (`log`)

gpui and its platform crates report exclusively through the `log`
facade — authorization denied, bundle missing, no notification daemon,
delivery failure. With no backend installed every one of those
diagnostics vanishes, so `main.rs` installs a minimal stderr logger
(`StderrLogger`, info cap) before any platform code runs. This is a
general fix, not notification-specific: the whole gpui/platform stack's
diagnostics become visible. The CLI client path installs nothing and
keeps its stdout/stderr contract clean.

## 5. Content safety

Notification `body`/`title` are terminal-controlled text (up to the
scanner's 64 KiB cap, lossy UTF-8). XDG daemons may render the body as
markup (`body-markup` capability), so `desktop_notify::escape_markup`
escapes `&`, `<`, `>` before posting — PTY bytes cannot forge links,
formatting, or urgency hints. macOS takes plain text and is unaffected
by the extra entities. OSC 9 carries no title; the summary falls back
to `Horizon`.

## 6. Testing

- Bus-free unit tests only (colocated in `src/desktop_notify.rs`):
  markup escaping (plain pass-through, metacharacters, multibyte
  preservation) and the tag round-trip. Nothing in the suite connects
  to a session bus, so the `sandboxed` nextest profile needed no
  exclusion for this work.
- gpui's test platform (`platform/test/platform.rs`) ships a full
  headless double (`shown/delivered/dismissed_system_notifications`,
  `simulate_system_notification_response`) — available if a future
  change wants to drive the router headlessly.
- Manual smoke (requires a desktop session): trigger
  `printf '\033]777;notify;Title;Body\a'` in a background Horizon
  terminal pane, confirm the banner, click it, and confirm the pane
  activates and the window foregrounds. On macOS run from
  `just dev-bundle` (a bare binary logs `system notifications disabled`
  and shows nothing — by design).

## 7. Known platform asymmetries (accepted)

- **Same-tag replacement**: macOS replaces an older notification with
  the same tag (tag = UNNotificationRequest identifier); gpui's Linux
  backend does not wire the tag to the XDG notification id, so repeat
  posts from one session stack instead of replacing. Cosmetic.
- **Close events**: not observable through the unified API on any
  platform (gpui's Linux backend swallows `__closed`). Accepted — see
  §3; if a future feature needs close reasons (e.g. auto-repost),
  that is a gpui-side extension, not something to hand-roll around.
- **Authorization**: macOS asks once (asynchronously); Linux has no
  authorization concept — absence of a daemon is the only failure
  mode.
