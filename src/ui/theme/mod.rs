use std::collections::HashMap;
use std::sync::Arc;

use floem::peniko::Color;
use floem::reactive::{RwSignal, Scope, SignalUpdate, SignalWith};

use crate::config::RawThemeConfig;
use crate::terminal::config::TerminalColors;

pub(crate) mod ansi;

// --- built-in chrome-role defaults ---------------------------------------
//
// Named constants (rather than inline literals in each accessor below)
// exist for exactly one reason: [`ThemeState::build`] needs the same
// defaults `resolve` falls back to, computed once per `Reload Config` swap
// rather than read reactively per call -- see that function's doc comment.
// Keeping one named constant per role, read from both places, is what keeps
// the accessor and the reload-time terminal-color computation from ever
// drifting apart. Mirrors `ansi`'s existing `BLACK`/`RED`/... constants.

const TEXT_PRIMARY_DEFAULT: Color = Color::from_rgb8(233, 236, 242);
const TEXT_MUTED_DEFAULT: Color = Color::from_rgb8(178, 185, 198);
const TEXT_SUBTLE_DEFAULT: Color = Color::from_rgb8(115, 122, 136);
const ACCENT_DEFAULT: Color = Color::from_rgb8(132, 220, 198);
const DANGER_DEFAULT: Color = Color::from_rgb8(246, 137, 146);
const SURFACE_BASE_DEFAULT: Color = Color::from_rgb8(22, 24, 29);
const SURFACE_PANEL_DEFAULT: Color = Color::from_rgb8(24, 27, 32);
const SURFACE_RAISED_DEFAULT: Color = Color::from_rgb8(31, 34, 41);
const SURFACE_CHROME_DEFAULT: Color = Color::from_rgb8(25, 28, 34);
const SURFACE_SELECTED_DEFAULT: Color = Color::from_rgb8(54, 59, 70);
const BORDER_DEFAULT_DEFAULT: Color = Color::from_rgb8(54, 59, 70);
const BORDER_SUBTLE_DEFAULT: Color = Color::from_rgb8(42, 46, 55);
const CURSOR_ACCENT_DEFAULT: Color = Color::from_rgb8(229, 192, 123);
const DIFF_ADDED_SURFACE_DEFAULT: Color = Color::from_rgb8(30, 46, 34);
const DIFF_ADDED_TEXT_DEFAULT: Color = Color::from_rgb8(140, 209, 156);
const DIFF_REMOVED_SURFACE_DEFAULT: Color = Color::from_rgb8(48, 30, 32);
const DIFF_REMOVED_TEXT_DEFAULT: Color = Color::from_rgb8(224, 130, 138);
const USER_MESSAGE_SURFACE_DEFAULT: Color = Color::from_rgb8(30, 43, 63);
const USER_MESSAGE_BORDER_DEFAULT: Color = Color::from_rgb8(65, 94, 133);
const APPROVAL_SURFACE_DEFAULT: Color = Color::from_rgb8(38, 34, 26);
const APPROVAL_BORDER_DEFAULT: Color = Color::from_rgb8(78, 66, 44);
const APPROVAL_CONFIRM_SURFACE_DEFAULT: Color = Color::from_rgb8(48, 84, 75);
const APPROVAL_DENY_SURFACE_DEFAULT: Color = Color::from_rgb8(80, 50, 54);

pub(crate) fn text_primary() -> Color {
    resolve("text_primary", TEXT_PRIMARY_DEFAULT)
}

pub(crate) fn text_muted() -> Color {
    resolve("text_muted", TEXT_MUTED_DEFAULT)
}

pub(crate) fn text_subtle() -> Color {
    resolve("text_subtle", TEXT_SUBTLE_DEFAULT)
}

pub(crate) fn accent() -> Color {
    resolve("accent", ACCENT_DEFAULT)
}

/// The app's one destructive/danger accent — the same red used for the
/// agent pane's "Deny" approval action (`workspace/view/agent_controls.rs`).
/// Reused here for destructive command styling (`ui/list_row.rs`) so both
/// "reject this" and "this ends something" read as the same kind of
/// warning.
pub(crate) fn danger() -> Color {
    resolve("danger", DANGER_DEFAULT)
}

pub(crate) fn surface_base() -> Color {
    resolve("surface_base", SURFACE_BASE_DEFAULT)
}

pub(crate) fn surface_panel() -> Color {
    resolve("surface_panel", SURFACE_PANEL_DEFAULT)
}

pub(crate) fn surface_raised() -> Color {
    resolve("surface_raised", SURFACE_RAISED_DEFAULT)
}

pub(crate) fn surface_chrome() -> Color {
    resolve("surface_chrome", SURFACE_CHROME_DEFAULT)
}

pub(crate) fn surface_selected() -> Color {
    resolve("surface_selected", SURFACE_SELECTED_DEFAULT)
}

pub(crate) fn border_default() -> Color {
    resolve("border_default", BORDER_DEFAULT_DEFAULT)
}

pub(crate) fn border_subtle() -> Color {
    resolve("border_subtle", BORDER_SUBTLE_DEFAULT)
}

/// Workspace mode's cursor-frame border color
/// (`workspace::view::pane`/`docs/workspace-mode-design.md`) — deliberately
/// distinct from `accent()` (the focus border) so the two remain
/// simultaneously legible when the cursor has moved away from focus.
/// Defaults to the same amber already used for `ui::theme::ansi::yellow`,
/// reusing a hue already present in the app's palette rather than
/// introducing a new one.
pub(crate) fn cursor_accent() -> Color {
    resolve("cursor_accent", CURSOR_ACCENT_DEFAULT)
}

// --- diff roles ------------------------------------------------------------
//
// Line-level diff rendering (`agent::view`'s fs.edit body, see
// `docs/agent-output-ui-design.md` decision 4): a line's background carries
// the added/removed distinction, and the sign column (+/-) gets its own,
// slightly brighter color so it stays legible against the surface tint.
// Unchanged/context lines use the ordinary chrome roles above, not a
// dedicated role of their own.

pub(crate) fn diff_added_surface() -> Color {
    resolve("diff_added_surface", DIFF_ADDED_SURFACE_DEFAULT)
}

pub(crate) fn diff_added_text() -> Color {
    resolve("diff_added_text", DIFF_ADDED_TEXT_DEFAULT)
}

pub(crate) fn diff_removed_surface() -> Color {
    resolve("diff_removed_surface", DIFF_REMOVED_SURFACE_DEFAULT)
}

pub(crate) fn diff_removed_text() -> Color {
    resolve("diff_removed_text", DIFF_REMOVED_TEXT_DEFAULT)
}

// --- agent transcript roles -------------------------------------------
//
// The two distinctions backlog item 7 flagged as lost when the transcript
// moved onto shared chrome roles (`Source agent transcript colors from the
// theme`): the user message bubble's blue tint, and the tool-approval
// surfaces' amber tint. Restored here as their own roles (not reused from
// `accent`/`danger`, which are hue-specific to other UI already) so a
// `[theme]` override can retint them independently of the rest of chrome.

/// The user message bubble's background (`agent::view::style::block_colors`)
/// -- restores the blue tint backlog item 7 flagged as lost when the
/// transcript moved onto shared chrome roles.
pub(crate) fn user_message_surface() -> Color {
    resolve("user_message_surface", USER_MESSAGE_SURFACE_DEFAULT)
}

/// The user message bubble's border -- paired with
/// [`user_message_surface`].
pub(crate) fn user_message_border() -> Color {
    resolve("user_message_border", USER_MESSAGE_BORDER_DEFAULT)
}

/// The `Approval`-tone transcript block's background (`agent::view::style::
/// block_colors`) -- restores the amber tint backlog item 7 flagged as
/// lost.
pub(crate) fn approval_surface() -> Color {
    resolve("approval_surface", APPROVAL_SURFACE_DEFAULT)
}

/// The `Approval`-tone transcript block's border -- paired with
/// [`approval_surface`].
pub(crate) fn approval_border() -> Color {
    resolve("approval_border", APPROVAL_BORDER_DEFAULT)
}

/// The approval banner's Approve button background
/// (`workspace::view::agent_controls::agent_approval_banner`) -- its border
/// stays plain [`accent`], this only roles the fill.
pub(crate) fn approval_confirm_surface() -> Color {
    resolve("approval_confirm_surface", APPROVAL_CONFIRM_SURFACE_DEFAULT)
}

/// The approval banner's Deny button background -- its border stays plain
/// [`danger`], this only roles the fill.
pub(crate) fn approval_deny_surface() -> Color {
    resolve("approval_deny_surface", APPROVAL_DENY_SURFACE_DEFAULT)
}

// --- terminal roles ------------------------------------------------------
//
// The terminal is not a separate palette: its default foreground,
// background, and cursor project from the same three roles chrome already
// uses, so setting `[theme]` once recolors chrome AND the terminal
// consistently (`terminal::config::resolved_colors` is the consumer). Each
// also accepts its own explicit override name below, for a terminal look
// that diverges from chrome without touching the shared roles.

pub(crate) fn terminal_foreground() -> Color {
    resolve_or("terminal_foreground", text_primary)
}

pub(crate) fn terminal_background() -> Color {
    resolve_or("terminal_background", surface_base)
}

/// Cursor defaults to `accent()` — the two already share the same built-in
/// value (`#84dcc6`), so this is a pixel-identical default, not a new one.
pub(crate) fn terminal_cursor() -> Color {
    resolve_or("terminal_cursor", accent)
}

/// Converts a resolved theme color to the `[u8; 3]` RGB triple the terminal
/// renderer works in (`terminal::config::resolved_colors`) — the one
/// conversion point between `ui::theme`'s `floem::peniko::Color` and the
/// terminal's raw per-cell colors. Alpha is always opaque for every theme
/// color used here, so it's dropped.
pub(crate) fn to_rgb8(color: Color) -> [u8; 3] {
    let rgba = color.to_rgba8();
    [rgba.r, rgba.g, rgba.b]
}

// --- config-driven overrides --------------------------------------------
//
// `[theme]` in Horizon's config file (`crate::config`) maps one of the
// names below to a `#rrggbb`/`#rgb` hex string, overriding that accessor's
// built-in default above. An unrecognized name or an unparsable hex value
// is warned about on stderr and skipped — never a startup failure, matching
// the config file's overall "never crash on a bad file" policy
// (`crate::config`'s module doc). The nested `[theme.ansi]` table (the 16
// base ANSI slots) is handled the same way by the `ansi` submodule; a
// future named-scheme layer would nest alongside `ansi` rather than
// reshape either table's keys.

/// Every name `[theme]` may override, matching this module's accessor
/// functions above one-to-one. `terminal_foreground`/`terminal_background`/
/// `terminal_cursor` have no fixed built-in default of their own here (see
/// `resolve_or`) but still need to be recognized names rather than rejected
/// as unknown.
const THEME_NAMES: &[&str] = &[
    "text_primary",
    "text_muted",
    "text_subtle",
    "accent",
    "danger",
    "surface_base",
    "surface_panel",
    "surface_raised",
    "surface_chrome",
    "surface_selected",
    "border_default",
    "border_subtle",
    "cursor_accent",
    "diff_added_surface",
    "diff_added_text",
    "diff_removed_surface",
    "diff_removed_text",
    "user_message_surface",
    "user_message_border",
    "approval_surface",
    "approval_border",
    "approval_confirm_surface",
    "approval_deny_surface",
    "terminal_foreground",
    "terminal_background",
    "terminal_cursor",
];

/// All of Horizon's `Reload Config`-able theme state. Chrome overrides and
/// the ANSI palette swap atomically behind the one reactive signal
/// ([`THEME_STATE`]); the terminal's derived scheme is precomputed here at
/// swap time — rather than resolved from `chrome`/`ansi` on every
/// `terminal::config::resolved_colors()` call, which is read once per
/// rendered cell — and then published to the cross-thread
/// [`TERMINAL_COLORS`] store, since cell rendering happens off the UI
/// thread where the reactive signal doesn't reach (see that static's doc
/// comment).
#[derive(Clone)]
struct ThemeState {
    chrome: HashMap<&'static str, Color>,
    ansi: HashMap<&'static str, Color>,
    terminal: TerminalColors,
}

impl ThemeState {
    fn build(theme: &RawThemeConfig) -> Self {
        let chrome = build_overrides(&theme.colors);
        let ansi_overrides = ansi::build_overrides(&theme.ansi);
        let terminal = compute_terminal_colors(&chrome, &ansi_overrides);
        ThemeState {
            chrome,
            ansi: ansi_overrides,
            terminal,
        }
    }
}

/// Pure computation of the terminal's derived 19-color scheme from a chrome
/// override map and an ansi override map — the reload-safe replacement for
/// what used to be a startup-only `OnceLock<TerminalColors>` in
/// `terminal::config`. Kept here, not there: every default color it falls
/// back to (the chrome roles' `*_DEFAULT` constants above, `ansi`'s
/// `BLACK`/`RED`/...) already lives in this module and `ansi`, so computing
/// the derived scheme here is the only way to do it without duplicating
/// those defaults a second time.
fn compute_terminal_colors(
    chrome: &HashMap<&'static str, Color>,
    ansi_overrides: &HashMap<&'static str, Color>,
) -> TerminalColors {
    let text_primary = resolve_pure(chrome, "text_primary", TEXT_PRIMARY_DEFAULT);
    let surface_base = resolve_pure(chrome, "surface_base", SURFACE_BASE_DEFAULT);
    let accent = resolve_pure(chrome, "accent", ACCENT_DEFAULT);

    TerminalColors {
        foreground: to_rgb8(resolve_pure(chrome, "terminal_foreground", text_primary)),
        background: to_rgb8(resolve_pure(chrome, "terminal_background", surface_base)),
        cursor: to_rgb8(resolve_pure(chrome, "terminal_cursor", accent)),
        black: to_rgb8(resolve_pure(ansi_overrides, "black", ansi::BLACK)),
        red: to_rgb8(resolve_pure(ansi_overrides, "red", ansi::RED)),
        green: to_rgb8(resolve_pure(ansi_overrides, "green", ansi::GREEN)),
        yellow: to_rgb8(resolve_pure(ansi_overrides, "yellow", ansi::YELLOW)),
        blue: to_rgb8(resolve_pure(ansi_overrides, "blue", ansi::BLUE)),
        magenta: to_rgb8(resolve_pure(ansi_overrides, "magenta", ansi::MAGENTA)),
        cyan: to_rgb8(resolve_pure(ansi_overrides, "cyan", ansi::CYAN)),
        white: to_rgb8(resolve_pure(ansi_overrides, "white", ansi::WHITE)),
        bright_black: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_black",
            ansi::BRIGHT_BLACK,
        )),
        bright_red: to_rgb8(resolve_pure(ansi_overrides, "bright_red", ansi::BRIGHT_RED)),
        bright_green: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_green",
            ansi::BRIGHT_GREEN,
        )),
        bright_yellow: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_yellow",
            ansi::BRIGHT_YELLOW,
        )),
        bright_blue: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_blue",
            ansi::BRIGHT_BLUE,
        )),
        bright_magenta: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_magenta",
            ansi::BRIGHT_MAGENTA,
        )),
        bright_cyan: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_cyan",
            ansi::BRIGHT_CYAN,
        )),
        bright_white: to_rgb8(resolve_pure(
            ansi_overrides,
            "bright_white",
            ansi::BRIGHT_WHITE,
        )),
    }
}

thread_local! {
    /// The process-wide (per-thread, in practice: floem's own reactive
    /// runtime is thread-local too — `RwSignal` is deliberately `!Sync`/
    /// `!Send`, see `floem_reactive::signal::NotThreadSafe` — and Horizon
    /// only ever touches this from its single UI thread; each test that
    /// reaches this module gets its own independent copy on its own
    /// thread, which is exactly the isolation `crate::config::load`'s
    /// startup-only cache and `app::keymap::Keymap`'s lock already rely on
    /// nextest's one-process-per-test model for), reload-able theme state.
    ///
    /// `RwSignal` (rather than a plain thread-local `ThemeState`, this
    /// module's pre-reload shape) is what makes every accessor below
    /// reactive: floem re-runs any `.style(|s| ...)` closure that read this
    /// signal the moment [`apply_reload`] swaps it, so existing call sites
    /// (`tab_strip.rs`, `workspace::view::chrome`, ...) pick up a reload
    /// with no changes of their own. `Arc` keeps a swap O(1) (no cloning
    /// the override maps) and keeps `.with()` reads cheap (a pointer deref,
    /// not a clone) — see this module's doc comment on `ThemeState` for why
    /// chrome/ansi/terminal are bundled into one signal rather than three.
    ///
    /// Created under a detached root `Scope` — NOT the scope that happens
    /// to be current at first access. This thread-local initializes lazily,
    /// and in the running app that first access is inside a view's style
    /// effect; a bare `RwSignal::new` would make that effect's scope the
    /// signal's owner, so the first theme swap — which re-runs exactly that
    /// effect — would dispose the signal mid-propagation and every other
    /// style closure would then read a dangling signal id (observed as a
    /// `called Option::unwrap() on a None value` panic in
    /// `floem_reactive`'s read path on the first real `Reload Config`;
    /// unit tests never catch this because nothing there reads the signal
    /// from inside an effect). `Scope::new()` has no parent, so nothing
    /// ever disposes it — the right lifetime for process-wide theme state.
    static THEME_STATE: RwSignal<Arc<ThemeState>> =
        Scope::new().create_rw_signal(initial_state());
}

/// Reads the config file's `[theme]` table once, at first access from
/// whichever code path (production startup or a test) touches this module
/// first on its thread — mirrors the pre-reload behavior where
/// `overrides()`'s `OnceLock::get_or_init` read `crate::config::load()`
/// lazily rather than eagerly at process start.
fn initial_state() -> Arc<ThemeState> {
    Arc::new(ThemeState::build(&crate::config::load().theme))
}

/// The terminal scheme's cross-thread home. The reactive `THEME_STATE`
/// above is thread-local twice over (the `thread_local!` itself, and
/// floem's reactive runtime behind `RwSignal`), which is correct for the
/// chrome/ansi accessors — those are only ever read from UI style
/// closures — but WRONG for the terminal's colors: the per-cell RGB
/// resolution (`terminal::core::render`) runs on terminal session threads,
/// which would each lazily initialize their own startup-config copy and
/// never observe a UI-thread [`apply_reload`]. Observed in the plan-03 E2E
/// as "chrome recolors live, the terminal grid keeps the startup theme
/// forever". A process-wide `RwLock` fixes that: one copy, written by
/// `apply_reload` on the UI thread, read (uncontended, a `Copy` struct)
/// from any thread that paints cells.
static TERMINAL_COLORS: std::sync::OnceLock<std::sync::RwLock<TerminalColors>> =
    std::sync::OnceLock::new();

fn terminal_colors_store() -> &'static std::sync::RwLock<TerminalColors> {
    TERMINAL_COLORS.get_or_init(|| {
        std::sync::RwLock::new(ThemeState::build(&crate::config::load().theme).terminal)
    })
}

/// Swaps in a freshly parsed `[theme]` table (`Reload Config`'s theme half
/// — see `app::command_actions::reload_config`): chrome and the ansi
/// palette atomically through the reactive [`ThemeState`], then the
/// terminal's derived colors into the cross-thread [`TERMINAL_COLORS`]
/// store — see those items' doc comments for why the split exists.
///
/// Two signal writes, not one: phase 1 installs the new chrome/ansi
/// override maps immediately (with a placeholder `terminal` field, never
/// observed by any reader — nothing else runs between phase 1 and phase 2,
/// both synchronous on the one UI thread); phase 2 then derives the
/// terminal's scheme by calling the *ordinary* per-role/per-ansi-color
/// accessors below (`terminal_foreground`, `ansi::black`, ...), which all
/// read this same thread-local signal and therefore now resolve against the
/// config this reload just installed. This reuses the exact resolution
/// path every other reader of the theme already goes through, rather than
/// a second hand-rolled one — [`ThemeState::build`]'s
/// `compute_terminal_colors` exists only to bootstrap the very first
/// `ThemeState`, before this signal exists for phase 2's accessors to read
/// "live" from at all.
pub(crate) fn apply_reload(theme: &RawThemeConfig) {
    let chrome = build_overrides(&theme.colors);
    let ansi_overrides = ansi::build_overrides(&theme.ansi);
    THEME_STATE.with(|signal| {
        signal.set(Arc::new(ThemeState {
            chrome,
            ansi: ansi_overrides,
            terminal: TerminalColors::default(),
        }));
    });

    let terminal = TerminalColors {
        foreground: to_rgb8(terminal_foreground()),
        background: to_rgb8(terminal_background()),
        cursor: to_rgb8(terminal_cursor()),
        black: to_rgb8(ansi::black()),
        red: to_rgb8(ansi::red()),
        green: to_rgb8(ansi::green()),
        yellow: to_rgb8(ansi::yellow()),
        blue: to_rgb8(ansi::blue()),
        magenta: to_rgb8(ansi::magenta()),
        cyan: to_rgb8(ansi::cyan()),
        white: to_rgb8(ansi::white()),
        bright_black: to_rgb8(ansi::bright_black()),
        bright_red: to_rgb8(ansi::bright_red()),
        bright_green: to_rgb8(ansi::bright_green()),
        bright_yellow: to_rgb8(ansi::bright_yellow()),
        bright_blue: to_rgb8(ansi::bright_blue()),
        bright_magenta: to_rgb8(ansi::bright_magenta()),
        bright_cyan: to_rgb8(ansi::bright_cyan()),
        bright_white: to_rgb8(ansi::bright_white()),
    };
    THEME_STATE.with(|signal| {
        signal.update(|state| Arc::make_mut(state).terminal = terminal);
    });
    // The cross-thread copy the render paths actually read — see
    // [`TERMINAL_COLORS`]. Written last, after the reactive state is fully
    // consistent; terminal readers are repaint-driven (per frame), not
    // reactive, so there is no ordering hazard with the signal writes above.
    *terminal_colors_store()
        .write()
        .expect("terminal color store poisoned") = terminal;
}

fn resolve(name: &'static str, default: Color) -> Color {
    THEME_STATE.with(|signal| signal.with(|state| resolve_pure(&state.chrome, name, default)))
}

/// Like [`resolve`], but the fallback is another role's resolved color
/// (itself override-aware) rather than a fixed constant — how
/// `terminal_foreground`/`terminal_background`/`terminal_cursor` derive
/// from `text_primary`/`surface_base`/`accent` by default.
fn resolve_or(name: &'static str, fallback: fn() -> Color) -> Color {
    let direct = THEME_STATE.with(|signal| signal.with(|state| state.chrome.get(name).copied()));
    direct.unwrap_or_else(fallback)
}

/// Reads `[theme.ansi]`'s live override for `name`, if any — `ansi`'s own
/// `resolve` calls back into this rather than keeping its own signal, so
/// chrome and ansi always swap together through the one `ThemeState` above.
pub(super) fn resolve_ansi(name: &'static str, default: Color) -> Color {
    THEME_STATE.with(|signal| signal.with(|state| resolve_pure(&state.ansi, name, default)))
}

/// The terminal's live derived color scheme — a plain locked read of a
/// `Copy` struct of `[u8; 3]` triples, for `terminal::config::
/// resolved_colors` to expose to the per-cell render path. Reads
/// [`TERMINAL_COLORS`], not `THEME_STATE`: cell rendering runs on terminal
/// session threads, where the thread-local reactive state would be a
/// stale, never-reloaded copy — see [`TERMINAL_COLORS`]'s doc comment.
pub(crate) fn terminal_colors() -> TerminalColors {
    *terminal_colors_store()
        .read()
        .expect("terminal color store poisoned")
}

/// A map lookup with a fallback default — the pure core both the
/// signal-backed [`resolve`]/[`resolve_ansi`] and reload-time
/// [`compute_terminal_colors`] share, so "how an override map resolves a
/// name" is defined exactly once.
fn resolve_pure(
    overrides: &HashMap<&'static str, Color>,
    name: &'static str,
    default: Color,
) -> Color {
    overrides.get(name).copied().unwrap_or(default)
}

fn build_overrides(entries: &HashMap<String, String>) -> HashMap<&'static str, Color> {
    let mut overrides = HashMap::new();
    for (name, hex) in entries {
        let Some(key) = THEME_NAMES.iter().find(|candidate| **candidate == name) else {
            eprintln!("horizon config: skipping theme override `{name}`: unknown color name");
            continue;
        };
        match parse_hex_color(hex) {
            Ok(color) => {
                overrides.insert(*key, color);
            }
            Err(error) => {
                eprintln!("horizon config: skipping theme override `{name}`: {error}");
            }
        }
    }
    overrides
}

/// Parses a `#rrggbb` or `#rgb` hex color string (case-insensitive, leading
/// `#` optional). Returns an error message (never panics) for anything
/// else, so a malformed `[theme]` entry can be warned about and skipped
/// rather than crashing startup. Shared with the `ansi` submodule (private
/// items here are visible to descendant modules) so hex parsing has exactly
/// one implementation for the whole theme.
fn parse_hex_color(input: &str) -> Result<Color, String> {
    let trimmed = input.trim().trim_start_matches('#');

    let expand_nibble = |c: char| -> Option<u8> {
        let d = c.to_digit(16)? as u8;
        Some(d * 16 + d)
    };
    let byte_pair = |pair: &str| -> Option<u8> { u8::from_str_radix(pair, 16).ok() };

    let rgb = match trimmed.len() {
        3 => {
            let mut chars = trimmed.chars();
            let r = expand_nibble(chars.next().unwrap());
            let g = expand_nibble(chars.next().unwrap());
            let b = expand_nibble(chars.next().unwrap());
            r.zip(g).zip(b).map(|((r, g), b)| (r, g, b))
        }
        6 => {
            let r = byte_pair(&trimmed[0..2]);
            let g = byte_pair(&trimmed[2..4]);
            let b = byte_pair(&trimmed[4..6]);
            r.zip(g).zip(b).map(|((r, g), b)| (r, g, b))
        }
        _ => None,
    };

    rgb.map(|(r, g, b)| Color::from_rgb8(r, g, b))
        .ok_or_else(|| format!("invalid hex color `{input}`: expected `#rgb` or `#rrggbb`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_digit_hex_with_hash() {
        assert_eq!(
            parse_hex_color("#84dcc6"),
            Ok(Color::from_rgb8(132, 220, 198))
        );
    }

    #[test]
    fn parses_six_digit_hex_without_hash() {
        assert_eq!(
            parse_hex_color("84DCC6"),
            Ok(Color::from_rgb8(132, 220, 198))
        );
    }

    #[test]
    fn parses_three_digit_shorthand_hex() {
        assert_eq!(parse_hex_color("#0f0"), Ok(Color::from_rgb8(0, 255, 0)));
    }

    #[test]
    fn rejects_wrong_length_hex() {
        assert!(parse_hex_color("#1234").is_err());
    }

    #[test]
    fn rejects_non_hex_characters() {
        assert!(parse_hex_color("#gggggg").is_err());
    }

    #[test]
    fn build_overrides_applies_a_valid_entry() {
        let mut entries = HashMap::new();
        entries.insert("accent".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(
            overrides.get("accent"),
            Some(&Color::from_rgb8(255, 0, 255))
        );
    }

    #[test]
    fn build_overrides_accepts_cursor_accent_override_name() {
        let mut entries = HashMap::new();
        entries.insert("cursor_accent".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("cursor_accent"));
    }

    #[test]
    fn cursor_accent_defaults_to_a_color_distinct_from_the_focus_accent() {
        assert_ne!(
            cursor_accent(),
            accent(),
            "the cursor frame must be visually distinct from the focus border"
        );
    }

    #[test]
    fn build_overrides_accepts_agent_transcript_role_override_names() {
        let mut entries = HashMap::new();
        entries.insert("user_message_surface".to_string(), "#ff00ff".to_string());
        entries.insert(
            "approval_confirm_surface".to_string(),
            "#00ff00".to_string(),
        );

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 2);
        assert!(overrides.contains_key("user_message_surface"));
        assert!(overrides.contains_key("approval_confirm_surface"));
    }

    #[test]
    fn build_overrides_accepts_terminal_role_override_names() {
        let mut entries = HashMap::new();
        entries.insert("terminal_cursor".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("terminal_cursor"));
    }

    #[test]
    fn build_overrides_skips_unknown_name_without_dropping_others() {
        let mut entries = HashMap::new();
        entries.insert("not_a_real_color".to_string(), "#ff00ff".to_string());
        entries.insert("accent".to_string(), "#ff00ff".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("accent"));
    }

    #[test]
    fn build_overrides_skips_invalid_hex_without_dropping_others() {
        let mut entries = HashMap::new();
        entries.insert("accent".to_string(), "not-a-color".to_string());
        entries.insert("danger".to_string(), "#ff0000".to_string());

        let overrides = build_overrides(&entries);

        assert_eq!(overrides.len(), 1);
        assert!(overrides.contains_key("danger"));
    }

    #[test]
    fn build_overrides_is_empty_for_an_empty_config() {
        assert!(build_overrides(&HashMap::new()).is_empty());
    }

    #[test]
    fn to_rgb8_drops_alpha_and_keeps_components() {
        assert_eq!(to_rgb8(Color::from_rgb8(1, 2, 3)), [1, 2, 3]);
    }

    // --- terminal roles: unset falls back to the paired chrome role -----
    //
    // These call the live (cache-backed) accessors rather than a pure
    // helper: whatever the process's real config resolves `text_primary`/
    // `surface_base`/`accent` to, `terminal_foreground`/
    // `terminal_background`/`terminal_cursor` must equal it exactly when
    // left unset, because the fallback *is* that same call — this holds
    // regardless of which config, if any, is active in the test process.

    #[test]
    fn terminal_foreground_falls_back_to_text_primary() {
        assert_eq!(terminal_foreground(), text_primary());
    }

    #[test]
    fn terminal_background_falls_back_to_surface_base() {
        assert_eq!(terminal_background(), surface_base());
    }

    #[test]
    fn terminal_cursor_falls_back_to_accent() {
        assert_eq!(terminal_cursor(), accent());
    }

    // --- Reload Config: theme swap ---------------------------------------

    #[test]
    fn apply_reload_updates_a_chrome_role() {
        let before = accent();

        let mut theme = RawThemeConfig::default();
        theme
            .colors
            .insert("accent".to_string(), "#123456".to_string());
        apply_reload(&theme);

        assert_eq!(accent(), Color::from_rgb8(0x12, 0x34, 0x56));
        assert_ne!(accent(), before);

        // Restore, so other tests in this process see the built-in default
        // again (theme state is a process-wide global, like `config::load`
        // and `Keymap::global`).
        apply_reload(&RawThemeConfig::default());
    }

    #[test]
    fn apply_reload_updates_the_ansi_palette_and_terminal_colors_together() {
        let mut theme = RawThemeConfig::default();
        theme.ansi.red = Some("#abcdef".to_string());
        apply_reload(&theme);

        assert_eq!(ansi::red(), Color::from_rgb8(0xab, 0xcd, 0xef));
        assert_eq!(terminal_colors().red, [0xab, 0xcd, 0xef]);

        apply_reload(&RawThemeConfig::default());
    }

    #[test]
    fn apply_reload_recomputes_derived_terminal_roles() {
        let mut theme = RawThemeConfig::default();
        theme
            .colors
            .insert("surface_base".to_string(), "#0a0b0c".to_string());
        apply_reload(&theme);

        // `terminal_background` has no override of its own here, so it
        // still derives from `surface_base` -- proving the derived roles
        // recompute from the *new* chrome map, not a stale one.
        assert_eq!(terminal_colors().background, [0x0a, 0x0b, 0x0c]);

        apply_reload(&RawThemeConfig::default());
    }
}
