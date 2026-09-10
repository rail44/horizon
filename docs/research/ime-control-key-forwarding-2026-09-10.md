# IME control-key forwarding — spike record (removed)

- Date: 2026-09-10
- Outcome: shipped in-terminal, removed the same day (revert `314d411`).
  C-c/C-d reach the PTY again; the SKK C-j gap in terminal panes remains
  open — as it does in every gpui app on macOS, Zed included.

## Root cause (unchanged, still true)

`gpui_macos`'s key-down handler (zed @ `5f8a7413…`,
`crates/gpui_macos/src/window.rs`) forwards a native key event to the input
context only while composing, for printable keys without
control/function/platform modifiers, and for modifier-less non-printing
keys. Control-modified keys are explicitly excluded ("we skip keys with
control …"), and an unhandled key equivalent with non-function modifiers
returns without consulting the input context — the subsequent `keyDown`
redelivery is swallowed by the `last_key_equivalent` dedup. The IME never
sees ctrl+<letter> anywhere in a gpui app on macOS.

Upstream context (as of 2026-09-10): no upstream issue tracks this. PR
zed-industries/zed#52192 introduced the printable-key IME-first policy and
deliberately left control out; open issue zed-industries/zed#63412 (control
characters delivered as IME text) would retire the historical justification
for that exclusion. Other backends do not share the gap: X11 runs every
event through the XIM filter, Wayland delivers keys through the compositor's
input method, and Windows consults the IME via `TranslateMessage` before
app-side key handling — macOS only.

## What the spike did, and why it was removed

App-side lever: during `TerminalView::handle_key` the original `NSEvent` is
still `+[NSApplication currentEvent]`, so it was handed to
`+[NSTextInputContext currentInputContext]`'s public `handleEvent:` — the
same call gpui's IME-first branch makes. A `true` return (IME claimed the
key) skipped the PTY; `false` fell through to the ordinary encoding.

First cut offered every plain ctrl+letter. Field test: C-j worked (SKK
toggled, PTY skipped) but C-c and C-d stopped reaching the terminal. Two
structural loss paths, both behind gpui_macos-private state:

1. Most ctrl+letters are bound to standard text-system selectors in
   AppKit's key-binding dispatch (ctrl+d → `deleteForward:`, ctrl+k →
   `deleteToEndOfParagraph:`, ctrl+a/e/f/b/n/p → movement, ...). Sending
   such an event through `handleEvent:` gets "handled = YES" from that
   dispatch — not from the IME. Normally the selector is routed back to the
   app via `doCommandBySelector:`, but gpui_macos primes that re-dispatch
   from private window state (`keystroke_for_do_command`) immediately
   before *its own* `handleEvent:` calls; an app-side call cannot prime it,
   so the selector lands in gpui's no-op `doCommandBySelector:` and dies.
2. Chords without a selector go to the IMK server; a pass-through verdict
   is re-delivered into the app, where it collides with gpui's
   `last_key_equivalent` dedup (a ctrl+letter key-down always arrives as a
   key equivalent first) and is dropped.

Conclusion: only chords an IME actually claims survive the round trip, and
*which* chords those are is per-IME knowledge. The follow-up allowlist
(C-j only) fixed the regression but hardcoded that knowledge — rejected as
a brittle shape. With no general app-side fix possible, the whole shim was
reverted.

## If revisited: the durable path is patching gpui_macos

The general fix belongs where the machinery is reachable: extend
gpui_macos's existing IME-first branch (`crates/gpui_macos/src/window.rs`,
the `is_ime_printable_key` block) to also route control+printable keys
through `inputContext handleEvent:` with the existing
`keystroke_for_do_command` priming — roughly fifteen lines, upstream-shaped.
Deployment: fork zed, point a `[patch.'https://github.com/zed-industries/zed']`
entry at the fork's URL — Cargo accepts a patch only against a *different*
source, which is why the same-URL rev-pin attempt described in the root
`Cargo.toml` comment was rejected but a fork URL is not. Cost: the fork must
be rebased whenever the gpui pin moves.
