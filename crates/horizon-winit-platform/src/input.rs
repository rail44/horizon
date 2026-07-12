//! Pure winit-event -> gpui-input mapping tables: keyboard (extends the
//! spike's leg-1 minimal mapping,
//! docs/research/winit-backend-spike.md §6.2, to the full key set
//! `src/terminal/input.rs::term_key_code` recognizes), mouse buttons,
//! scroll deltas, and double/triple-click detection (ported from
//! `gpui_linux`'s wayland pointer handling, pinned checkout
//! `crates/gpui_linux/src/linux/wayland/client.rs`, since winit exposes no
//! click-count concept of its own). Kept free of winit's event-loop types
//! where possible so these are colocated unit-test targets, per AGENTS.md's
//! "key/mouse mapping tables are good test targets".

use std::time::{Duration, Instant};

use gpui::{point, px, Keystroke, Modifiers, MouseButton, NavigationDirection, Pixels, Point};
use winit::event::{KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub(crate) fn winit_modifiers_to_gpui(state: ModifiersState) -> Modifiers {
    Modifiers {
        control: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        platform: state.super_key(),
        function: false,
    }
}

pub(crate) fn winit_mouse_button_to_gpui(button: winit::event::MouseButton) -> Option<MouseButton> {
    use winit::event::MouseButton as WinitButton;
    match button {
        WinitButton::Left => Some(MouseButton::Left),
        WinitButton::Right => Some(MouseButton::Right),
        WinitButton::Middle => Some(MouseButton::Middle),
        WinitButton::Back => Some(MouseButton::Navigate(NavigationDirection::Back)),
        WinitButton::Forward => Some(MouseButton::Navigate(NavigationDirection::Forward)),
        // gpui's MouseButton has no catch-all for exotic hardware buttons.
        WinitButton::Other(_) => None,
    }
}

pub(crate) fn winit_scroll_delta_to_gpui(delta: MouseScrollDelta) -> gpui::ScrollDelta {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => gpui::ScrollDelta::Lines(point(x, y)),
        MouseScrollDelta::PixelDelta(position) => {
            gpui::ScrollDelta::Pixels(point(px(position.x as f32), px(position.y as f32)))
        }
    }
}

pub(crate) fn winit_touch_phase_to_gpui(phase: winit::event::TouchPhase) -> gpui::TouchPhase {
    use winit::event::TouchPhase as WinitPhase;
    match phase {
        WinitPhase::Started => gpui::TouchPhase::Started,
        WinitPhase::Moved => gpui::TouchPhase::Moved,
        // gpui has no "cancelled" phase; collapse into Ended (stop
        // scrolling), the same way a lifted finger would.
        WinitPhase::Ended | WinitPhase::Cancelled => gpui::TouchPhase::Ended,
    }
}

/// gpui_linux's double/triple-click thresholds
/// (`crates/gpui_linux/src/linux/platform.rs`'s `DOUBLE_CLICK_INTERVAL`/
/// `DOUBLE_CLICK_DISTANCE`), reused verbatim so click-count semantics don't
/// shift between the native and winit backends.
const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_DISTANCE: Pixels = px(5.0);

fn is_within_click_distance(a: Point<Pixels>, b: Point<Pixels>) -> bool {
    let diff = a - b;
    diff.x.abs() <= DOUBLE_CLICK_DISTANCE && diff.y.abs() <= DOUBLE_CLICK_DISTANCE
}

/// Per-window click-count state for `MouseDownEvent::click_count` /
/// `MouseUpEvent::click_count`. winit reports only press/release; gpui
/// (like every desktop toolkit) additionally wants "was this the 2nd/3rd
/// click in a row at roughly the same spot", which is on the application
/// to track — see module docs.
pub(crate) struct ClickTracker {
    last_click: Option<Instant>,
    last_button: Option<MouseButton>,
    last_position: Point<Pixels>,
    current_count: usize,
}

impl ClickTracker {
    pub(crate) fn new() -> Self {
        Self {
            last_click: None,
            last_button: None,
            last_position: Point::default(),
            current_count: 0,
        }
    }

    /// Call on every button press; returns the click count to attach to
    /// that press's `MouseDownEvent` (and any `MouseUpEvent` before the
    /// next press).
    pub(crate) fn register_press(&mut self, button: MouseButton, position: Point<Pixels>) -> usize {
        let now = Instant::now();
        let is_repeat = self.last_button == Some(button)
            && self
                .last_click
                .is_some_and(|last| now.duration_since(last) < DOUBLE_CLICK_INTERVAL)
            && is_within_click_distance(self.last_position, position);
        self.current_count = if is_repeat { self.current_count + 1 } else { 1 };
        self.last_click = Some(now);
        self.last_button = Some(button);
        self.last_position = position;
        self.current_count
    }

    /// The count to attach to a release, matching whatever the most recent
    /// press established.
    pub(crate) fn current_count(&self) -> usize {
        self.current_count.max(1)
    }
}

/// Maps a winit `KeyEvent`'s logical key to gpui's key-name convention:
/// lowercase names for named keys (`"up"`, `"pageup"`, `"f1"`, ...,
/// matching `gpui_linux::keystroke_from_xkb`'s special-cased names and its
/// xkb-keysym-name fallback) and the printable character itself
/// (lowercased, see [`normalize_letter_case`]) for everything else.
/// `logical_key` is winit's layout- and shift-aware key value (unaffected
/// only by Ctrl, per winit's own doc on `KeyEvent::logical_key`), so this
/// needs no separate layout handling the way gpui_linux's raw-xkb path
/// does.
///
/// Returns `None` for keys that shouldn't produce a `Keystroke` at all:
/// pure modifier keys (Shift/Control/Alt/...) — `gpui_linux` likewise
/// filters these via `!keysym.is_modifier_key()` before dispatching — dead
/// keys still composing, and the long tail of winit's multimedia/TV-remote
/// named keys that have no terminal-relevant meaning
/// (`src/terminal/input.rs::term_key_code` wouldn't recognize their name
/// anyway).
pub(crate) fn winit_key_event_to_keystroke(
    event: &KeyEvent,
    modifiers: Modifiers,
) -> Option<Keystroke> {
    let (key, modifiers, carries_text) = match &event.logical_key {
        Key::Character(text) => {
            let (key, modifiers) = normalize_letter_case(text.to_string(), modifiers);
            (key, modifiers, true)
        }
        // Space is the one winit `Named` key `term_key_code`
        // (src/terminal/input.rs) has no unconditional case for -- like any
        // other printable character, it only maps there when kitty mode or
        // Ctrl is active, so (unlike Enter/Tab/arrows/...) it still needs
        // `key_char` to reach the text-input fallback in direct-ASCII mode
        // the same way a letter does.
        Key::Named(NamedKey::Space) => ("space".to_string(), modifiers, true),
        // Every other named key (Enter, Tab, Backspace, Escape, arrows,
        // Home/End, PageUp/PageDown, Delete, Insert, F1-F24) is one
        // `term_key_code` always handles regardless of kitty mode -- it
        // carries no printable text of its own, matching
        // `Keystroke::key_char`'s own contract ("the character that could
        // have been typed") and gpui_linux's `keystroke_from_xkb`, which
        // never sets it for these either. winit's raw `event.text`
        // sometimes disagrees (Enter's `text` is `Some("\r")`, for
        // instance) -- surfacing that here would make `key_char.is_some()`
        // (the text-input fallback's gate in `window.rs`, and the dedup
        // guard in `src/terminal/mod.rs`) treat an always-otherwise-handled
        // named key as unhandled printable text, double-feeding it.
        Key::Named(named) => (named_key_string(*named)?.to_string(), modifiers, false),
        Key::Unidentified(_) | Key::Dead(_) => return None,
    };
    let key_char = if carries_text {
        event.text.as_ref().map(|text| text.to_string())
    } else {
        None
    };
    Some(Keystroke {
        modifiers,
        key,
        key_char,
    })
}

/// Lowercases printable characters, clearing `modifiers.shift` for
/// case-invariant characters (digits, symbols) whose shifted meaning is
/// already baked into the character itself (`shift+1` arrives as `"!"`,
/// not `"1"`) — the same convention `gpui_linux::keystroke_from_xkb`
/// applies, so keybindings defined against e.g. `shift-1` behave
/// identically under either backend.
fn normalize_letter_case(key: String, mut modifiers: Modifiers) -> (String, Modifiers) {
    if modifiers.shift && key.chars().count() == 1 && key.to_lowercase() == key.to_uppercase() {
        modifiers.shift = false;
    }
    (key.to_lowercase(), modifiers)
}

/// Named-key -> gpui key-name string, covering every name
/// `src/terminal/input.rs::term_key_code` recognizes (the enter/tab/arrows/
/// home/end/pageup/pagedown/delete/insert/backspace/escape/space literals,
/// plus f1..f24 — `term_key_code` caps function keys at f24, so this does
/// too) plus a few more `gpui_linux` itself special-cases
/// (`keystroke_from_xkb`) that are useful beyond the terminal (e.g. tab
/// completion in the palette). Everything else returns `None` — winit's
/// `NamedKey` also lists TV-remote/media-player keys with no desktop-app
/// meaning here.
fn named_key_string(named: NamedKey) -> Option<&'static str> {
    Some(match named {
        NamedKey::Enter => "enter",
        NamedKey::Tab => "tab",
        NamedKey::Space => "space",
        NamedKey::Backspace => "backspace",
        NamedKey::Escape => "escape",
        NamedKey::Delete => "delete",
        NamedKey::Insert => "insert",
        NamedKey::Home => "home",
        NamedKey::End => "end",
        NamedKey::PageUp => "pageup",
        NamedKey::PageDown => "pagedown",
        NamedKey::ArrowUp => "up",
        NamedKey::ArrowDown => "down",
        NamedKey::ArrowLeft => "left",
        NamedKey::ArrowRight => "right",
        NamedKey::F1 => "f1",
        NamedKey::F2 => "f2",
        NamedKey::F3 => "f3",
        NamedKey::F4 => "f4",
        NamedKey::F5 => "f5",
        NamedKey::F6 => "f6",
        NamedKey::F7 => "f7",
        NamedKey::F8 => "f8",
        NamedKey::F9 => "f9",
        NamedKey::F10 => "f10",
        NamedKey::F11 => "f11",
        NamedKey::F12 => "f12",
        NamedKey::F13 => "f13",
        NamedKey::F14 => "f14",
        NamedKey::F15 => "f15",
        NamedKey::F16 => "f16",
        NamedKey::F17 => "f17",
        NamedKey::F18 => "f18",
        NamedKey::F19 => "f19",
        NamedKey::F20 => "f20",
        NamedKey::F21 => "f21",
        NamedKey::F22 => "f22",
        NamedKey::F23 => "f23",
        NamedKey::F24 => "f24",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::MouseButton as WinitButton;

    #[test]
    fn maps_left_middle_right_directly() {
        assert_eq!(
            winit_mouse_button_to_gpui(WinitButton::Left),
            Some(MouseButton::Left)
        );
        assert_eq!(
            winit_mouse_button_to_gpui(WinitButton::Middle),
            Some(MouseButton::Middle)
        );
        assert_eq!(
            winit_mouse_button_to_gpui(WinitButton::Right),
            Some(MouseButton::Right)
        );
    }

    #[test]
    fn maps_back_forward_to_navigate() {
        assert_eq!(
            winit_mouse_button_to_gpui(WinitButton::Back),
            Some(MouseButton::Navigate(NavigationDirection::Back))
        );
        assert_eq!(
            winit_mouse_button_to_gpui(WinitButton::Forward),
            Some(MouseButton::Navigate(NavigationDirection::Forward))
        );
    }

    #[test]
    fn drops_unrecognized_buttons() {
        assert_eq!(winit_mouse_button_to_gpui(WinitButton::Other(7)), None);
    }

    #[test]
    fn click_tracker_counts_rapid_same_spot_presses() {
        let mut tracker = ClickTracker::new();
        let position = point(px(10.0), px(10.0));
        assert_eq!(tracker.register_press(MouseButton::Left, position), 1);
        assert_eq!(tracker.register_press(MouseButton::Left, position), 2);
        assert_eq!(tracker.register_press(MouseButton::Left, position), 3);
    }

    #[test]
    fn click_tracker_resets_on_button_change() {
        let mut tracker = ClickTracker::new();
        let position = point(px(10.0), px(10.0));
        assert_eq!(tracker.register_press(MouseButton::Left, position), 1);
        assert_eq!(tracker.register_press(MouseButton::Right, position), 1);
    }

    #[test]
    fn click_tracker_resets_when_far_apart() {
        let mut tracker = ClickTracker::new();
        assert_eq!(
            tracker.register_press(MouseButton::Left, point(px(0.0), px(0.0))),
            1
        );
        assert_eq!(
            tracker.register_press(MouseButton::Left, point(px(200.0), px(200.0))),
            1
        );
    }

    #[test]
    fn normalizes_shifted_letter_keeps_shift_modifier() {
        let modifiers = Modifiers {
            shift: true,
            ..Default::default()
        };
        let (key, modifiers) = normalize_letter_case("A".to_string(), modifiers);
        assert_eq!(key, "a");
        assert!(modifiers.shift);
    }

    #[test]
    fn normalizes_shifted_symbol_clears_shift_modifier() {
        let modifiers = Modifiers {
            shift: true,
            ..Default::default()
        };
        let (key, modifiers) = normalize_letter_case("!".to_string(), modifiers);
        assert_eq!(key, "!");
        assert!(!modifiers.shift);
    }

    #[test]
    fn named_key_covers_terminal_key_set() {
        for (named, expected) in [
            (NamedKey::Enter, "enter"),
            (NamedKey::Tab, "tab"),
            (NamedKey::Backspace, "backspace"),
            (NamedKey::Escape, "escape"),
            (NamedKey::ArrowUp, "up"),
            (NamedKey::ArrowDown, "down"),
            (NamedKey::ArrowLeft, "left"),
            (NamedKey::ArrowRight, "right"),
            (NamedKey::Home, "home"),
            (NamedKey::End, "end"),
            (NamedKey::PageUp, "pageup"),
            (NamedKey::PageDown, "pagedown"),
            (NamedKey::Delete, "delete"),
            (NamedKey::Insert, "insert"),
            (NamedKey::F1, "f1"),
            (NamedKey::F24, "f24"),
        ] {
            assert_eq!(named_key_string(named), Some(expected));
        }
    }

    #[test]
    fn named_key_drops_media_keys() {
        assert_eq!(named_key_string(NamedKey::AudioVolumeUp), None);
        assert_eq!(named_key_string(NamedKey::Shift), None);
        assert_eq!(named_key_string(NamedKey::Control), None);
    }
}
