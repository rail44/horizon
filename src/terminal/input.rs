//! GPUI keystroke → termwiz key-code mapping. Encoding — legacy escape
//! sequences AND negotiated kitty state — is horizon-terminal-core's job
//! (`protocol/kitty_keyboard`); this layer only names the key. Plain
//! printable text deliberately does NOT map here: on macOS it arrives
//! through the text-input pipeline (EntityInputHandler), and routing it
//! through Key too would double-feed every keypress. M1 revisits this
//! with kitty-flags-on-frame mode routing (docs/gpui-migration-design.md).

use std::time::{Duration, Instant};

use gpui::{px, Keystroke, Modifiers, MouseButton, Pixels, Point, ScrollDelta, TouchPhase};
use horizon_terminal_core::{
    TerminalMouseButton, TerminalMouseModifiers, TerminalSelectionKind, TerminalSelectionPoint,
};

/// Pixel position (window coordinates) → cell coordinates, given the
/// paint-time metrics. Mirrors the Floem shell's `cell_from_point`.
pub(crate) fn cell_from_position(
    position: Point<Pixels>,
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
) -> TerminalSelectionPoint {
    let col = (f32::from(position.x - origin.x) / f32::from(cell_width))
        .max(0.0)
        .floor() as usize;
    let row = (f32::from(position.y - origin.y) / f32::from(line_height))
        .max(0.0)
        .floor() as usize;
    TerminalSelectionPoint { row, col }
}

/// Fixed lines-per-tick for terminal-protocol passthrough
/// (`ScrollDelta::Lines`, e.g. a physical mouse wheel). The primary-screen
/// frontend instead uses [`viewport_row_delta`] for both precise and
/// imprecise events so its displacement matches GPUI lists.
const WHEEL_TICK_LINES: i32 = 3;

/// GPUI's `List` converts imprecise line deltas with a 20px logical line
/// height. Use the same conversion for the terminal's frontend viewport so a
/// wheel gesture has the same physical displacement in Agent and Terminal;
/// precise trackpad deltas already carry their exact pixel distance.
const GPUI_SCROLL_LINE_HEIGHT: Pixels = px(20.0);

/// Time-domain presentation for imprecise wheel events. GPUI's Linux backend
/// reports one ordinary wheel notch as `ScrollDelta::Lines(3)`, and `List`
/// maps that to 60 logical pixels in one frame. That is unobtrusive for a
/// variable-height transcript but visibly jumps several rows on a terminal
/// grid. Keep the same distance while converging to it across animation
/// frames. Exact `Pixels` input (touchpad/finger plus platform kinetic scroll)
/// bypasses this state entirely.
const DISCRETE_SCROLL_HALF_LIFE: Duration = Duration::from_millis(40);
const DISCRETE_SCROLL_MAX_DURATION: Duration = Duration::from_millis(140);
const DISCRETE_SCROLL_SETTLE_PIXELS: f32 = 0.5;

/// The frontend pixel distance represented by one GPUI wheel event. Kept
/// separate from terminal-row conversion so discrete events can be animated
/// in physical presentation space before reaching the row-addressed window.
pub(crate) fn viewport_pixel_delta(delta: ScrollDelta) -> f32 {
    let pixels = f32::from(delta.pixel_delta(GPUI_SCROLL_LINE_HEIGHT).y);
    if pixels.is_finite() {
        pixels
    } else {
        0.0
    }
}

/// Convert a GPUI wheel event into continuous terminal-row units for local
/// presentation. Unlike [`ScrollAccumulator`], this never truncates a precise
/// delta: the fractional row is painted by the frontend canvas.
pub(crate) fn viewport_row_delta(delta: ScrollDelta, terminal_line_height: f32) -> f32 {
    if !terminal_line_height.is_finite() || terminal_line_height <= 0.0 {
        return 0.0;
    }
    let rows = viewport_pixel_delta(delta) / terminal_line_height;
    if rows.is_finite() {
        rows
    } else {
        0.0
    }
}

/// Frame-driven smoothing state for `ScrollDelta::Lines`. `remaining_pixels`
/// is unapplied intent, not a second viewport position: every emitted step is
/// immediately folded into `TerminalSession`'s existing continuous scrollback
/// state, which remains the sole scroll authority.
#[derive(Debug, Default)]
pub(crate) struct DiscreteScrollAnimation {
    remaining_pixels: f32,
    last_input_at: Option<Instant>,
    last_tick_at: Option<Instant>,
}

impl DiscreteScrollAnimation {
    pub(crate) fn push(&mut self, pixels: f32, now: Instant) {
        if !pixels.is_finite() || pixels.abs() <= f32::EPSILON {
            return;
        }
        if !self.is_active() {
            self.last_tick_at = Some(now);
        }
        self.remaining_pixels += pixels;
        self.last_input_at = Some(now);
        if self.remaining_pixels.abs() <= f32::EPSILON {
            self.reset();
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.remaining_pixels.abs() > f32::EPSILON
    }

    pub(crate) fn reset(&mut self) {
        self.remaining_pixels = 0.0;
        self.last_input_at = None;
        self.last_tick_at = None;
    }

    /// Advance to `now`, returning the pixels to apply on this frame. An
    /// exponential approach composes naturally when more notches arrive or
    /// direction reverses; the hard duration bound consumes the small tail so
    /// input never leaves latent motion behind.
    pub(crate) fn advance(&mut self, now: Instant) -> Option<f32> {
        if !self.is_active() {
            return None;
        }
        let last_tick = self.last_tick_at.unwrap_or(now);
        let last_input = self.last_input_at.unwrap_or(now);
        let elapsed = now.saturating_duration_since(last_tick);
        self.last_tick_at = Some(now);

        if self.remaining_pixels.abs() <= DISCRETE_SCROLL_SETTLE_PIXELS
            || now.saturating_duration_since(last_input) >= DISCRETE_SCROLL_MAX_DURATION
        {
            let step = self.remaining_pixels;
            self.reset();
            return Some(step);
        }
        if elapsed.is_zero() {
            return Some(0.0);
        }

        let half_lives = elapsed.as_secs_f32() / DISCRETE_SCROLL_HALF_LIFE.as_secs_f32();
        let progress = 1.0 - 0.5_f32.powf(half_lives);
        let step = self.remaining_pixels * progress;
        self.remaining_pixels -= step;
        Some(step)
    }
}

/// Pixel-delta fallback accumulator (root-caused in
/// docs/research/gpui-terminal-presentation-2026-07-18.md, "Touchpad
/// scrolling"): a naive per-event `pixels / line_height` truncation drops
/// most trackpad deltas (each event is usually a fraction of one line), so
/// fractional lines are banked across events and only whole-line multiples
/// are consumed when the wheel must be encoded for an old peer or a terminal
/// application (termy's `scroll_debt`, tty7's trunc/bank). Local scrollback
/// does not use this debt: it paints the fraction directly. Reset on
/// `TouchPhase::Started` so a new passthrough gesture doesn't inherit an old
/// remainder.
#[derive(Debug, Default)]
pub(crate) struct ScrollAccumulator {
    fractional_lines: f32,
}

impl ScrollAccumulator {
    pub(crate) fn reset(&mut self) {
        self.fractional_lines = 0.0;
    }

    /// Consumes one wheel event, returning the whole-line scroll step due
    /// (if any) and banking the remainder. `line_height` is the pixel
    /// height of one terminal row; `phase` resets the accumulator on a
    /// fresh gesture. Positive `TerminalScroll::lines` scrolls toward
    /// history (alacritty `Scroll::Delta` convention), matching the old
    /// fixed ±3 step's sign.
    pub(crate) fn consume(
        &mut self,
        delta: ScrollDelta,
        phase: TouchPhase,
        line_height: f32,
    ) -> Option<i32> {
        if matches!(phase, TouchPhase::Started) {
            self.reset();
        }
        match delta {
            ScrollDelta::Lines(lines) => {
                if lines.y.abs() < f32::EPSILON {
                    return None;
                }
                Some(if lines.y > 0.0 {
                    WHEEL_TICK_LINES
                } else {
                    -WHEEL_TICK_LINES
                })
            }
            ScrollDelta::Pixels(pixels) if line_height > 0.0 => {
                self.fractional_lines += f32::from(pixels.y) / line_height;
                let whole_lines = self.fractional_lines.trunc();
                if whole_lines == 0.0 {
                    return None;
                }
                self.fractional_lines -= whole_lines;
                Some(whole_lines as i32)
            }
            ScrollDelta::Pixels(_) => None,
        }
    }
}

/// Click count → selection kind: 1 is a plain point-drag selection, 2 is
/// word (core-side `SelectionType::Semantic`), 3+ is line
/// (`SelectionType::Lines`) -- the convergent idiom across every surveyed
/// gpui terminal (docs/research/gpui-terminal-presentation-2026-07-18.md,
/// "Selection").
pub(crate) fn selection_kind_from_clicks(click_count: usize) -> TerminalSelectionKind {
    match click_count {
        0 | 1 => TerminalSelectionKind::Simple,
        2 => TerminalSelectionKind::Word,
        _ => TerminalSelectionKind::Line,
    }
}

pub(crate) fn terminal_mouse_button(button: MouseButton) -> Option<TerminalMouseButton> {
    match button {
        MouseButton::Left => Some(TerminalMouseButton::Left),
        MouseButton::Middle => Some(TerminalMouseButton::Middle),
        MouseButton::Right => Some(TerminalMouseButton::Right),
        _ => None,
    }
}

pub(crate) fn terminal_mouse_modifiers(modifiers: &Modifiers) -> TerminalMouseModifiers {
    TerminalMouseModifiers {
        shift: modifiers.shift,
        alt: modifiers.alt,
        control: modifiers.control,
    }
}

/// Named/function keys always map; character keys map when Ctrl is held
/// (never text) or when the session negotiated kitty's "report all keys
/// as escape codes" (`keys_as_escape_codes`, mirrored on the frame) —
/// otherwise they are text and belong to the input-handler pipeline.
/// Alt-held characters are left to macOS option-composition pending the
/// option-as-alt policy decision (M1).
pub(crate) fn term_key_code(
    keystroke: &Keystroke,
    keys_as_escape_codes: bool,
) -> Option<termwiz::input::KeyCode> {
    use termwiz::input::KeyCode;

    let named = match keystroke.key.as_str() {
        "enter" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "backspace" => Some(KeyCode::Backspace),
        "escape" => Some(KeyCode::Escape),
        "up" => Some(KeyCode::UpArrow),
        "down" => Some(KeyCode::DownArrow),
        "right" => Some(KeyCode::RightArrow),
        "left" => Some(KeyCode::LeftArrow),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" => Some(KeyCode::PageUp),
        "pagedown" => Some(KeyCode::PageDown),
        "delete" => Some(KeyCode::Delete),
        "insert" => Some(KeyCode::Insert),
        _ => None,
    };
    if let Some(key) = named {
        return Some(key);
    }
    if let Some(number) = keystroke
        .key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Some(KeyCode::Function(number));
    }

    let mut chars = keystroke.key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    (keystroke.modifiers.control || keys_as_escape_codes).then_some(KeyCode::Char(ch))
}

pub(crate) fn term_modifiers(modifiers: &Modifiers) -> termwiz::input::Modifiers {
    use termwiz::input::Modifiers as TermModifiers;

    let mut result = TermModifiers::NONE;
    if modifiers.control {
        result |= TermModifiers::CTRL;
    }
    if modifiers.alt {
        result |= TermModifiers::ALT;
    }
    if modifiers.shift {
        result |= TermModifiers::SHIFT;
    }
    if modifiers.platform {
        result |= TermModifiers::SUPER;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn one_click_is_simple() {
        assert_eq!(selection_kind_from_clicks(1), TerminalSelectionKind::Simple);
    }

    #[test]
    fn zero_clicks_is_simple() {
        // gpui's click_count is 1-based in practice, but this stays a safe
        // default should a 0 ever arrive.
        assert_eq!(selection_kind_from_clicks(0), TerminalSelectionKind::Simple);
    }

    #[test]
    fn two_clicks_is_word() {
        assert_eq!(selection_kind_from_clicks(2), TerminalSelectionKind::Word);
    }

    #[test]
    fn three_or_more_clicks_is_line() {
        assert_eq!(selection_kind_from_clicks(3), TerminalSelectionKind::Line);
        assert_eq!(selection_kind_from_clicks(4), TerminalSelectionKind::Line);
    }

    const LINE_HEIGHT: f32 = 20.0;

    fn pixels_delta(y: f32) -> ScrollDelta {
        ScrollDelta::Pixels(point(px(0.0), px(y)))
    }

    #[test]
    fn precise_delta_reaches_the_viewport_without_line_truncation() {
        assert_eq!(viewport_row_delta(pixels_delta(7.5), LINE_HEIGHT), 0.375);
    }

    #[test]
    fn imprecise_delta_uses_the_same_twenty_pixel_unit_as_gpui_list() {
        assert_eq!(
            viewport_row_delta(ScrollDelta::Lines(point(0.0, 1.0)), LINE_HEIGHT),
            1.0
        );
        assert_eq!(
            viewport_pixel_delta(ScrollDelta::Lines(point(0.0, 3.0))),
            60.0,
            "one ordinary Linux wheel notch is three GPUI lines"
        );
    }

    #[test]
    fn invalid_terminal_line_height_produces_no_viewport_motion() {
        assert_eq!(viewport_row_delta(pixels_delta(7.5), 0.0), 0.0);
    }

    #[test]
    fn a_discrete_notch_is_spread_across_animation_frames_without_losing_distance() {
        let start = Instant::now();
        let mut animation = DiscreteScrollAnimation::default();
        animation.push(60.0, start);

        let first = animation
            .advance(start + Duration::from_millis(16))
            .unwrap();
        assert!(
            (0.0..LINE_HEIGHT).contains(&first),
            "the first frame should move less than one terminal row, got {first}px"
        );

        let mut applied = first;
        for elapsed_ms in [32, 48, 64, 80, 96, 112, 128, 140] {
            applied += animation
                .advance(start + Duration::from_millis(elapsed_ms))
                .unwrap();
        }
        assert!((applied - 60.0).abs() < 0.001);
        assert!(!animation.is_active());
    }

    #[test]
    fn reversing_a_discrete_animation_composes_with_already_applied_motion() {
        let start = Instant::now();
        let mut animation = DiscreteScrollAnimation::default();
        animation.push(60.0, start);
        let first = animation
            .advance(start + Duration::from_millis(16))
            .unwrap();

        animation.push(-60.0, start + Duration::from_millis(16));
        let reverse = animation
            .advance(start + Duration::from_millis(32))
            .unwrap();
        assert!(reverse < 0.0, "the next frame should reverse direction");

        let mut applied = first + reverse;
        for elapsed_ms in [48, 64, 80, 96, 112, 128, 144, 156] {
            applied += animation
                .advance(start + Duration::from_millis(elapsed_ms))
                .unwrap();
        }
        assert!(
            applied.abs() < 0.001,
            "equal opposite notches should return to the starting position"
        );
        assert!(!animation.is_active());
    }

    #[test]
    fn a_delta_under_one_line_banks_the_remainder_and_reports_no_line_yet() {
        let mut accum = ScrollAccumulator::default();
        let step = accum.consume(pixels_delta(15.0), TouchPhase::Moved, LINE_HEIGHT);
        assert_eq!(step, None);
    }

    #[test]
    fn banked_remainders_accumulate_across_events_until_a_whole_line_is_due() {
        let mut accum = ScrollAccumulator::default();
        // 10px + 10px == half a line each (line_height 20), individually
        // below the threshold but a whole line once summed.
        assert_eq!(
            accum.consume(pixels_delta(10.0), TouchPhase::Moved, LINE_HEIGHT),
            None
        );
        assert_eq!(
            accum.consume(pixels_delta(10.0), TouchPhase::Moved, LINE_HEIGHT),
            Some(1)
        );
    }

    #[test]
    fn a_large_delta_consumes_multiple_whole_lines_at_once_and_banks_the_rest() {
        let mut accum = ScrollAccumulator::default();
        // 45px / 20px-per-line = 2.25 lines: two whole lines now, a quarter
        // line banked for next time.
        assert_eq!(
            accum.consume(pixels_delta(45.0), TouchPhase::Moved, LINE_HEIGHT),
            Some(2)
        );
        // The banked 0.25 line plus another 15px (0.75 line) crosses the
        // next whole-line boundary.
        assert_eq!(
            accum.consume(pixels_delta(15.0), TouchPhase::Moved, LINE_HEIGHT),
            Some(1)
        );
    }

    #[test]
    fn negative_pixel_deltas_scroll_the_opposite_direction() {
        let mut accum = ScrollAccumulator::default();
        assert_eq!(
            accum.consume(pixels_delta(-25.0), TouchPhase::Moved, LINE_HEIGHT),
            Some(-1)
        );
    }

    #[test]
    fn touch_phase_started_resets_a_banked_remainder() {
        let mut accum = ScrollAccumulator::default();
        // Bank 0.75 of a line.
        assert_eq!(
            accum.consume(pixels_delta(15.0), TouchPhase::Moved, LINE_HEIGHT),
            None
        );
        // A fresh gesture starts -- even a zero-magnitude Started event
        // must clear the old gesture's banked remainder.
        assert_eq!(
            accum.consume(pixels_delta(0.0), TouchPhase::Started, LINE_HEIGHT),
            None
        );
        // Without the reset, 0.75 (old) + 0.75 (this) would cross a whole
        // line; with the reset, this is a fresh 0.75 and stays banked.
        assert_eq!(
            accum.consume(pixels_delta(15.0), TouchPhase::Moved, LINE_HEIGHT),
            None
        );
    }

    #[test]
    fn explicit_reset_prevents_passthrough_debt_from_leaking_between_modes() {
        let mut accum = ScrollAccumulator::default();
        assert_eq!(
            accum.consume(pixels_delta(15.0), TouchPhase::Moved, LINE_HEIGHT),
            None
        );
        accum.reset();
        assert_eq!(
            accum.consume(pixels_delta(10.0), TouchPhase::Moved, LINE_HEIGHT),
            None
        );
    }

    #[test]
    fn imprecise_wheel_ticks_use_a_fixed_step_regardless_of_magnitude() {
        let mut accum = ScrollAccumulator::default();
        assert_eq!(
            accum.consume(
                ScrollDelta::Lines(point(0.0, 1.0)),
                TouchPhase::Moved,
                LINE_HEIGHT
            ),
            Some(3)
        );
        assert_eq!(
            accum.consume(
                ScrollDelta::Lines(point(0.0, -2.5)),
                TouchPhase::Moved,
                LINE_HEIGHT
            ),
            Some(-3)
        );
    }

    #[test]
    fn a_zero_wheel_tick_reports_no_line() {
        let mut accum = ScrollAccumulator::default();
        assert_eq!(
            accum.consume(
                ScrollDelta::Lines(point(0.0, 0.0)),
                TouchPhase::Moved,
                LINE_HEIGHT
            ),
            None
        );
    }
}
