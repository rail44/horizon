# IME control-key forwarding (spike)

- Date: 2026-09-10
- Status: shipped, on by default, deliberately no switch (owner decision
  2026-09-10 — no file key, no environment override); offer set
  allowlisted to C-j after the first field test (see "Field regression")
- Scope: macOS only (no-op elsewhere)
- Code: `src/ime_forward.rs` (bridge) + the hook in `TerminalView::handle_key`
  (`src/terminal/mod.rs`)

## Problem

The IME's own control-key shortcut — SKK's C-j kana/latin mode toggle, most
prominently — never fires in Horizon's terminal panes on macOS: the key is
consumed by the app (in the terminal, encoded into the PTY as LF) and the
input method never sees it. The same symptom reproduces in Zed itself, so
this is a gpui-platform gap, not a Horizon binding issue.

## Root cause

gpui_macos's key-down handler (zed @ `5f8a7413a31769e0882357f90dc424b3962ac72d`,
`crates/gpui_macos/src/window.rs`) forwards a native key event to the input
context only in three cases: while composing, for printable keys *without*
control/function/platform modifiers (plus IME-active + the input handler's
`prefers_ime_for_printable_keys`), and for modifier-less non-printing keys.
Control-modified keys are explicitly excluded ("we skip keys with control or
the input handler adds control-characters to the buffer"), and an unhandled
key equivalent with non-function modifiers returns without consulting the
input context — the subsequent `keyDown` redelivery is swallowed by the
`last_key_equivalent` dedup. The IME therefore never sees ctrl+<letter>
anywhere in a gpui app on macOS.

Upstream context (as of 2026-09-10): no upstream issue tracks this. PR
#52192 introduced the printable-key IME-first policy and deliberately left
control out; open issue #63412 proposes filtering control characters in
`insertText:`, which would retire the historical justification for that
exclusion. The other backends do not share the gap: X11 runs every event
through the XIM filter (`gpui_linux .../x11/client.rs`), Wayland delivers
keys through the compositor's input method when text-input is enabled, and
Windows' message loop consults the IME via `TranslateMessage` before
app-side key handling.

## Mechanism

`src/ime_forward.rs` is the one lever that works without patching gpui (its
public API exposes no macOS surface — no `NSView`, no input context, no
native events — verified by grep across `crates/gpui/src`):

1. `TerminalView::handle_key`, after the composing/phantom-Enter guards,
   offers the key to the IME when the keystroke is control plus exactly one
   ASCII lowercase letter with no other modifier *and* the letter is on the
   allowlist (`IME_CLAIMED_CONTROL_LETTERS`, currently just `j`;
   `offerable_control_letter` — pure, unit-tested).
2. The native event is recovered with `+[NSApplication currentEvent]`:
   `handle_key` runs inside AppKit's synchronous dispatch, so the very event
   gpui declined to forward is still current. Synthesizing an event was
   considered and rejected — `currentEvent` gives real characters, real
   key code, real window number, and correct layout handling for free.
3. The event goes to `+[NSTextInputContext currentInputContext]`'s public
   `handleEvent:` — the same call gpui's IME-first branch makes. `true`
   means the IME claimed the key: it is dropped and never reaches the PTY.
   `false` (any of: no native event, not a key down, no input context, IME
   passed through) falls through to the ordinary PTY encoding untouched.

Dependencies: `objc2 0.6` / `objc2-app-kit 0.3.2` under
`[target.'cfg(target_os = "macos")'.dependencies]`, matching the versions
already pinned in Cargo.lock via gpui_macos — only dependency edges were
added, no new package versions. Features are additive with gpui_macos's
set (`NSApplication`, `NSResponder`, `NSEvent`, `NSTextInputContext`).

## Decision: default on, no switch (owner decision 2026-09-10)

Default-on is safe because the population whose behavior changes is exactly
the input sources that claim ctrl+<letter> — SKK-family IMEs, whose users
want the claim. Every other input source (ABC, Google Japanese Input, ...)
answers `handleEvent:` with NO and sees byte-identical behavior. Even the
"control characters leak through insertText" fear that motivated gpui's
exclusion (open upstream as #63412) is benign in a terminal: a leaked
ctrl+letter control byte is the same byte the PTY encoder would have sent.

The owner explicitly declined both a config file key and an environment
override — the surface stays exactly what the 2026-07-18 narrowing left,
and no knob is added anywhere. The accepted trade is recorded here rather
than tunable: with the bridge active, the shell loses the C-<letter> chords
the IME claims. That is the standard terminal-with-SKK trade (iTerm2
behaves the same); keys the IME passes through (C-a, C-k, …) still reach
the shell. The one regression case — an IME claiming a ctrl+letter the
*shell* should get, concretely terminal emacs with skk.el where C-j belongs
to emacs's SKK — is accepted as part of that same trade. If it ever needs
relief, adding an off-switch is a small, local change to
`src/ime_forward.rs`; nothing else would move.

## Field regression: why the offer set is an allowlist (2026-09-10)

The spike's first cut offered every plain ctrl+letter to the input context.
Field test: C-j worked (SKK toggled, PTY skipped) but C-c and C-d stopped
reaching the terminal. The mechanism, reconstructed from AppKit behavior:

- Most ctrl+letters are bound to standard text-system selectors in
  AppKit's key-binding dispatch (ctrl+d → `deleteForward:`, ctrl+k →
  `deleteToEndOfParagraph:`, ctrl+a/e/f/b/n/p → movement, ...). Sending
  such an event through `handleEvent:` gets "handled = YES" from that
  dispatch — not from the IME. Normally the selector is routed back to the
  app via `doCommandBySelector:`, but gpui_macos primes that re-dispatch
  from private window state (`keystroke_for_do_command`) immediately before
  *its own* `handleEvent:` calls; an app-side call cannot prime it, so the
  selector lands in gpui's no-op `doCommandBySelector:` and the key dies.
- Chords without a selector go to the IMK server; a pass-through verdict is
  re-delivered into the app, where it collides with gpui's
  `last_key_equivalent` dedup (a ctrl+letter key-down always arrives as a
  key equivalent first) and can be dropped there.

Conclusion: only chords an IME actually claims survive the round trip.
Outside composition, SKK claims C-j (C-q, the half-width toggle, is the
plausible second entry — unverified). The gate is therefore an explicit
allowlist; every other ctrl+letter stays on the ordinary PTY path
byte-for-byte as before the spike. Extending the allowlist requires
verifying the chord with the real IME; selector-bound chords are
structurally unsafe and should never be listed.

Other known limits: if gpui ever forwards control keys itself, this shim
could double-offer the key; and gpui-component's `Input` (agent composer)
is not wired to the shim — scope is the terminal pane, where the SKK C-j
flow lives.

## Verification

Unit: `offerable_control_letter` gate tests in `src/ime_forward.rs`.

GUI (manual — needs a real IME; per AGENTS.md's GUI Verification section):

1. Run with `HORIZON_INPUT_TRACE=1`; focus a terminal pane; with a SKK IME
   active, press C-j → expect `ime_forward consumed (pty skipped)` in the
   trace, the SKK mode indicator toggling, and no LF reaching the shell.
2. Press C-a / C-k / C-d / C-c → expect `ime_forward passed through, falls
   through to pty` (they are not on the allowlist) and normal
   readline/flow-control behavior — this is the regression pin for the
   first field test.
3. With no IME active (ABC input source) → C-j sends LF exactly as before
   (trace shows `ime_forward passed through`).
