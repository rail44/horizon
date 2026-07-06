use std::collections::HashMap;
use std::sync::OnceLock;

use floem::keyboard::{Key, KeyEvent, Modifiers, NamedKey};
use termwiz::input::{KeyCode as TermKeyCode, Modifiers as TermModifiers};

use crate::app::commands::CommandId;
use crate::terminal::KeyEventKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AgentDraftAction {
    Insert(String),
    Backspace,
    Submit,
}

pub(crate) fn pop_last_grapheme_approx(text: &mut String) {
    while let Some(ch) = text.pop() {
        if !is_combining_mark(ch) {
            break;
        }
    }
}

pub(crate) fn agent_draft_action(key: &Key, modifiers: Modifiers) -> Option<AgentDraftAction> {
    match key {
        Key::Named(NamedKey::Enter) => Some(AgentDraftAction::Submit),
        Key::Named(NamedKey::Backspace) => Some(AgentDraftAction::Backspace),
        Key::Named(NamedKey::Space) if agent_accepts_text_input(modifiers) => {
            Some(AgentDraftAction::Insert(" ".to_string()))
        }
        Key::Character(text) if agent_accepts_text_input(modifiers) => {
            Some(AgentDraftAction::Insert(text.to_string()))
        }
        _ => None,
    }
}

pub(crate) fn terminal_input_from_key(event: &KeyEvent) -> Option<Vec<u8>> {
    match &event.key.logical_key {
        Key::Character(text) => character_input(text.as_str(), event.modifiers),
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(vec![0x7f]),
        Key::Named(NamedKey::Escape) => Some(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        _ => None,
    }
}

pub(crate) fn terminal_key_from_key(event: &KeyEvent) -> Option<TermKeyCode> {
    if let Key::Character(text) = &event.key.logical_key {
        return terminal_key_from_character(text.as_str(), event.modifiers);
    }
    terminal_key_from_input(&event.key.logical_key)
}

/// Maps a single-keystroke printable character to `TermKeyCode::Char` so it
/// routes through `TerminalCommand::Key` — and from there, `TerminalCore`'s
/// live Kitty flags (`terminal::protocol::kitty_keyboard::encode_text_key`)
/// — instead of `terminal_input_from_key`'s raw-bytes `character_input`
/// path, which never consulted the terminal's negotiated Kitty state at all
/// (see `KITTY_COMPLIANCE`'s former "Report all keys as escape codes (text
/// keys)" BYPASSED row).
///
/// Two cases deliberately still fall through to the old path (return
/// `None` here, exactly as `handle_terminal_key`'s call order already
/// expects — see its comment): multi-character `text` (not IME — a
/// composed/committed string arrives through `Event::ImeCommit`, handled
/// entirely separately in `app::input::handle_ime_commit` — but the rare
/// non-IME case of a single physical keystroke producing more than one
/// `char`, e.g. some ligature-producing layouts, isn't a single keystroke
/// `TermKeyCode::Char` can represent), and a Super/Cmd-held character with
/// no Ctrl also held (preserving `character_input`'s existing "meta
/// swallows the keystroke" behavior, which `terminal_input_from_key`'s
/// `character_input` call still implements for whatever reaches it).
///
/// `first` is passed through as termwiz's own `KeyCode::Char` convention
/// expects: the *base* (unshifted) character, undoing the ASCII case fold
/// winit already applied to `text` for a Shift-held letter (`"A"` for
/// Shift+A) — see `kitty_keyboard::encode_text_key`'s doc comment. Not
/// possible for a shifted non-letter (Shift+1 -> `'!'` on a US layout, with
/// no algorithmic inverse available here), so those pass through unchanged;
/// see `KITTY_COMPLIANCE`.
fn terminal_key_from_character(text: &str, modifiers: Modifiers) -> Option<TermKeyCode> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if !modifiers.control() && modifiers.meta() {
        return None;
    }

    let base = if modifiers.shift() && first.is_ascii_uppercase() {
        first.to_ascii_lowercase()
    } else {
        first
    };
    Some(TermKeyCode::Char(base))
}

pub(crate) fn terminal_key_from_input(key: &Key) -> Option<TermKeyCode> {
    match key {
        Key::Named(NamedKey::Enter) => Some(TermKeyCode::Enter),
        Key::Named(NamedKey::Tab) => Some(TermKeyCode::Tab),
        Key::Named(NamedKey::Backspace) => Some(TermKeyCode::Backspace),
        Key::Named(NamedKey::Escape) => Some(TermKeyCode::Escape),
        Key::Named(NamedKey::ArrowUp) => Some(TermKeyCode::UpArrow),
        Key::Named(NamedKey::ArrowDown) => Some(TermKeyCode::DownArrow),
        Key::Named(NamedKey::ArrowRight) => Some(TermKeyCode::RightArrow),
        Key::Named(NamedKey::ArrowLeft) => Some(TermKeyCode::LeftArrow),
        Key::Named(NamedKey::Home) => Some(TermKeyCode::Home),
        Key::Named(NamedKey::End) => Some(TermKeyCode::End),
        Key::Named(NamedKey::PageUp) => Some(TermKeyCode::PageUp),
        Key::Named(NamedKey::PageDown) => Some(TermKeyCode::PageDown),
        Key::Named(NamedKey::Delete) => Some(TermKeyCode::Delete),
        _ => None,
    }
}

/// Classifies an `Event::KeyDown` as an initial press or an OS/winit-
/// synthesized repeat (a held key), for `TerminalCommand::Key`'s Kitty
/// "event type" subfield (`terminal::protocol::kitty_keyboard`).
/// `Event::KeyUp` never goes through this function — a key-up is always
/// `KeyEventKind::Release` outright, classified directly at its call site
/// (see `workspace::input::handle_active_pane_key_release`) since winit
/// never sets `repeat` on a release.
pub(crate) fn terminal_key_event_kind(event: &KeyEvent) -> KeyEventKind {
    key_event_kind_from_repeat(event.key.repeat)
}

fn key_event_kind_from_repeat(repeat: bool) -> KeyEventKind {
    if repeat {
        KeyEventKind::Repeat
    } else {
        KeyEventKind::Press
    }
}

pub(crate) fn termwiz_modifiers(modifiers: Modifiers) -> TermModifiers {
    let mut term_modifiers = TermModifiers::NONE;
    if modifiers.shift() {
        term_modifiers |= TermModifiers::SHIFT;
    }
    if modifiers.control() {
        term_modifiers |= TermModifiers::CTRL;
    }
    if modifiers.alt() {
        term_modifiers |= TermModifiers::ALT;
    }
    if modifiers.meta() {
        term_modifiers |= TermModifiers::SUPER;
    }
    term_modifiers
}

pub(crate) fn is_terminal_paste_key(event: &KeyEvent) -> bool {
    is_terminal_paste_input(&event.key.logical_key, event.modifiers)
}

/// Whether `event` matches the chord that opens the command palette — the
/// `[keybindings]` entry for the reserved `"open-palette"` pseudo-command
/// (see [`Keymap::from_entries`]). Unbound by default: the palette's global
/// chord retired once workspace mode shipped `:` as its one-key resident
/// (`docs/tasks/backlog.md` item 1, resolved) — set an explicit entry here
/// to restore a global shortcut.
pub(crate) fn is_palette_open_key(event: &KeyEvent) -> bool {
    Keymap::global().is_palette_open(event)
}

/// Whether `event` matches the chord that enters workspace mode
/// (`docs/workspace-mode-design.md`) — the `[keybindings]` entry for the
/// reserved `"workspace-mode"` pseudo-command (see
/// [`Keymap::from_entries`]), falling back to `ctrl+'` when unset. An
/// agent pane's message box also accepts a bare `Esc` as a second entry
/// path regardless of this chord — see
/// `workspace::agent_escape_requests_workspace_mode`.
pub(crate) fn is_workspace_mode_enter_key(event: &KeyEvent) -> bool {
    Keymap::global().is_workspace_mode_enter(event)
}

pub(crate) fn palette_accepts_text_input(modifiers: Modifiers) -> bool {
    !modifiers.control() && !modifiers.alt() && !modifiers.meta()
}

pub(crate) fn is_terminal_copy_key(event: &KeyEvent) -> bool {
    is_terminal_copy_input(&event.key.logical_key, event.modifiers)
}

fn agent_accepts_text_input(modifiers: Modifiers) -> bool {
    !modifiers.control() && !modifiers.alt() && !modifiers.meta()
}

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe20..=0xfe2f
    )
}

fn is_terminal_paste_input(key: &Key, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(NamedKey::Paste) => true,
        Key::Character(text) => {
            modifiers.control() && modifiers.shift() && text.eq_ignore_ascii_case("v")
        }
        _ => false,
    }
}

fn is_terminal_copy_input(key: &Key, modifiers: Modifiers) -> bool {
    match key {
        Key::Named(NamedKey::Copy) => true,
        Key::Character(text) => {
            modifiers.control() && modifiers.shift() && text.eq_ignore_ascii_case("c")
        }
        _ => false,
    }
}

fn character_input(text: &str, modifiers: Modifiers) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let single_char = chars.next().is_none();

    if modifiers.control() && single_char {
        return control_input(first);
    }

    if modifiers.meta() {
        return None;
    }

    let mut bytes = Vec::new();
    if modifiers.alt() {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

fn control_input(c: char) -> Option<Vec<u8>> {
    let c = c.to_ascii_lowercase();
    let byte = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    };
    Some(vec![byte])
}

// --- command keybindings ----------------------------------------------
//
// `[keybindings]` in Horizon's config file (`crate::config`) maps a key
// chord string (e.g. `"ctrl+shift+t"`) to a `CommandId` string (e.g.
// `"new-terminal"`) for the *simple* commands — the ones `app::
// command_actions::CommandInvocation::Simple` can run without an
// explicit target. Entries layer on top of `default_bindings` below,
// overriding a default bound to the same chord or adding a new one.
//
// The reserved value `"open-palette"` is special: it isn't a `CommandId`
// (the palette can't list or run "open the palette" as an operation on
// itself), so it overrides `Keymap::palette_chord` instead of adding to
// `Keymap::bindings` — see `Keymap::from_entries`.
//
// An entry that doesn't parse (bad chord syntax, unknown command id) is
// warned about on stderr and skipped — never a startup failure, matching
// the config file's overall "never crash on a bad file" policy
// (`crate::config`'s module doc).

/// Horizon's built-in keyboard shortcuts. Deliberately small: creation and
/// basic layout operations that are safe to fire globally. Destructive
/// (`TerminateActiveSession`) and contextual (approve/deny/cancel, which
/// already have pane-local buttons) commands are left unbound by default —
/// add a `[keybindings]` entry for them if wanted.
fn default_bindings() -> &'static [(&'static str, CommandId)] {
    &[
        ("ctrl+shift+t", CommandId::NewTerminal),
        ("ctrl+shift+a", CommandId::NewAgent),
        ("ctrl+shift+d", CommandId::SplitActivePane),
        ("ctrl+shift+w", CommandId::CloseActivePane),
        ("ctrl+shift+x", CommandId::CloseActiveTab),
    ]
}

/// The reserved `[keybindings]` value that overrides the command-palette
/// open chord — not a real `CommandId` (opening the palette isn't an
/// operation the palette itself can list or run), so it's resolved
/// separately from `command_id_from_str`/`Keymap::bindings` below. See
/// [`Keymap::from_entries`].
///
/// Unlike [`WORKSPACE_MODE_PSEUDO_COMMAND`], this one has no built-in
/// default chord any more (`docs/tasks/backlog.md` item 1, resolved):
/// opening the palette is a workspace-mode resident now (`:` inside the
/// mode, see `workspace::mode_input`), not a global shortcut. The
/// mechanism itself is kept — an explicit entry here still works — only
/// the shipped default is gone.
const OPEN_PALETTE_PSEUDO_COMMAND: &str = "open-palette";

/// The reserved `[keybindings]` value that overrides the workspace-mode
/// entry chord — mirrors [`OPEN_PALETTE_PSEUDO_COMMAND`] exactly (not a
/// real `CommandId`, resolved separately from `command_id_from_str`). See
/// [`Keymap::from_entries`].
const WORKSPACE_MODE_PSEUDO_COMMAND: &str = "workspace-mode";
/// Built-in default chord for [`WORKSPACE_MODE_PSEUDO_COMMAND`] —
/// `docs/workspace-mode-design.md`'s shipped pick. `super+esc` was tried
/// first, but on the owner's real GNOME session the shell intercepts it
/// before it ever reaches a client window (Horizon's own handling path
/// proved healthy headless, so the loss is GNOME's, not Horizon's).
/// `ctrl+'` replaces it: apostrophe has no legacy terminal encoding, so
/// essentially no in-pane TUI can already claim it, and it sits under a
/// comfortable finger for the owner's (Dvorak) layout.
const DEFAULT_WORKSPACE_MODE_CHORD: &str = "ctrl+'";

fn command_id_from_str(id: &str) -> Option<CommandId> {
    match id {
        "new-terminal" => Some(CommandId::NewTerminal),
        "new-agent" => Some(CommandId::NewAgent),
        "split-active-pane" => Some(CommandId::SplitActivePane),
        "focus-next-pane" => Some(CommandId::FocusNextPane),
        "close-active-pane" => Some(CommandId::CloseActivePane),
        "close-active-tab" => Some(CommandId::CloseActiveTab),
        "terminate-active-session" => Some(CommandId::TerminateActiveSession),
        "approve-tool-call" => Some(CommandId::ApproveToolCall),
        "deny-tool-call" => Some(CommandId::DenyToolCall),
        "cancel-agent-turn" => Some(CommandId::CancelAgentTurn),
        "reload-agent-runtime" => Some(CommandId::ReloadAgentRuntime),
        _ => None,
    }
}

/// A parsed key chord: an exact modifier set plus a key. Matching is exact
/// on modifiers (not "at least") — the same convention most desktop apps
/// use, and the simplest one to reason about when defaults and config
/// entries interact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Chord {
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
    key: ChordKey,
}

impl Chord {
    /// Takes `modifiers`/`key` rather than a whole `KeyEvent` so this stays
    /// testable without constructing one — floem's `KeyEvent` wraps a winit
    /// type with a private platform-specific field, so it can't be built
    /// from a plain struct literal outside that crate.
    fn matches(&self, modifiers: Modifiers, key: &Key) -> bool {
        self.ctrl == modifiers.control()
            && self.shift == modifiers.shift()
            && self.alt == modifiers.alt()
            && self.meta == modifiers.meta()
            && self.key.matches(key)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ChordKey {
    Char(char),
    Named(NamedChordKey),
}

impl ChordKey {
    fn parse(token: &str) -> Option<Self> {
        let lower = token.to_ascii_lowercase();
        if let Some(named) = NamedChordKey::parse(&lower) {
            return Some(ChordKey::Named(named));
        }
        let mut chars = lower.chars();
        let first = chars.next()?;
        if chars.next().is_some() {
            return None; // not a single character, and not a recognized named key
        }
        Some(ChordKey::Char(first))
    }

    fn matches(self, key: &Key) -> bool {
        match (self, key) {
            (ChordKey::Char(expected), Key::Character(text)) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(only), None) => only.to_ascii_lowercase() == expected,
                    _ => false,
                }
            }
            (ChordKey::Named(expected), Key::Named(named)) => expected.matches(*named),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum NamedChordKey {
    Enter,
    Escape,
    Tab,
    Space,
    Backspace,
    Delete,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
}

impl NamedChordKey {
    fn parse(lower: &str) -> Option<Self> {
        Some(match lower {
            "enter" | "return" => Self::Enter,
            "escape" | "esc" => Self::Escape,
            "tab" => Self::Tab,
            "space" | "spacebar" => Self::Space,
            "backspace" => Self::Backspace,
            "delete" | "del" => Self::Delete,
            "up" | "arrowup" => Self::ArrowUp,
            "down" | "arrowdown" => Self::ArrowDown,
            "left" | "arrowleft" => Self::ArrowLeft,
            "right" | "arrowright" => Self::ArrowRight,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" => Self::PageUp,
            "pagedown" => Self::PageDown,
            _ => return None,
        })
    }

    fn matches(self, named: NamedKey) -> bool {
        matches!(
            (self, named),
            (Self::Enter, NamedKey::Enter)
                | (Self::Escape, NamedKey::Escape)
                | (Self::Tab, NamedKey::Tab)
                | (Self::Space, NamedKey::Space)
                | (Self::Backspace, NamedKey::Backspace)
                | (Self::Delete, NamedKey::Delete)
                | (Self::ArrowUp, NamedKey::ArrowUp)
                | (Self::ArrowDown, NamedKey::ArrowDown)
                | (Self::ArrowLeft, NamedKey::ArrowLeft)
                | (Self::ArrowRight, NamedKey::ArrowRight)
                | (Self::Home, NamedKey::Home)
                | (Self::End, NamedKey::End)
                | (Self::PageUp, NamedKey::PageUp)
                | (Self::PageDown, NamedKey::PageDown)
        )
    }
}

/// Parses a key chord string like `"ctrl+shift+t"`: modifiers joined by
/// `+`, ending in the key itself. Modifier names are case-insensitive and
/// accept a couple of aliases (`control`/`ctrl`, `option`/`alt`,
/// `cmd`/`command`/`super`/`win`/`meta`). Returns an error message (never
/// panics) for anything unparsable, so a bad `[keybindings]` entry can be
/// warned about and skipped rather than crashing startup.
fn parse_chord(spec: &str) -> Result<Chord, String> {
    let parts: Vec<&str> = spec
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(format!("empty key chord `{spec}`"));
    }

    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut meta = false;
    let mut key_token = None;

    for (index, part) in parts.iter().enumerate() {
        let is_last = index == parts.len() - 1;
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "meta" | "cmd" | "command" | "super" | "win" => meta = true,
            _ if is_last => key_token = Some(*part),
            other => return Err(format!("unknown modifier `{other}` in key chord `{spec}`")),
        }
    }

    let key_token = key_token.ok_or_else(|| format!("key chord `{spec}` has no key"))?;
    let key = ChordKey::parse(key_token)
        .ok_or_else(|| format!("unrecognized key `{key_token}` in key chord `{spec}`"))?;

    Ok(Chord {
        ctrl,
        shift,
        alt,
        meta,
        key,
    })
}

/// The resolved set of key chord -> command bindings: Horizon's built-in
/// defaults with the config file's `[keybindings]` table layered on top.
/// The command-palette open chord is tracked separately (`palette_chord`)
/// rather than through `bindings`, since it isn't a `CommandId` — see
/// [`Keymap::from_entries`]. `palette_chord` is `None` unless a config
/// entry sets one (see [`OPEN_PALETTE_PSEUDO_COMMAND`]'s doc comment).
pub(crate) struct Keymap {
    bindings: Vec<(Chord, CommandId)>,
    palette_chord: Option<Chord>,
    workspace_mode_chord: Chord,
    /// The chord spec string that produced `workspace_mode_chord` (the
    /// built-in default, or whatever a config entry set) — kept alongside
    /// the parsed `Chord` purely for display, so the status bar can render
    /// the actual configured chord (`app::status_bar`) instead of a
    /// hardcoded string that would drift the moment someone rebinds it.
    workspace_mode_chord_label: String,
}

impl Keymap {
    /// The process-wide keymap, built once from Horizon's config file
    /// (applied at startup only — see `AGENTS.md`) and cached for the rest
    /// of the run.
    pub(crate) fn global() -> &'static Keymap {
        static KEYMAP: OnceLock<Keymap> = OnceLock::new();
        KEYMAP.get_or_init(|| Keymap::from_entries(&crate::config::load().keybindings))
    }

    fn from_entries(entries: &HashMap<String, String>) -> Self {
        let mut bindings: Vec<(Chord, CommandId)> = default_bindings()
            .iter()
            .map(|(spec, command_id)| {
                (
                    parse_chord(spec).expect("built-in default key chord must parse"),
                    *command_id,
                )
            })
            .collect();
        let mut palette_chord: Option<Chord> = None;
        let mut workspace_mode_chord = parse_chord(DEFAULT_WORKSPACE_MODE_CHORD)
            .expect("built-in default key chord must parse");
        let mut workspace_mode_chord_label = DEFAULT_WORKSPACE_MODE_CHORD.to_string();

        for (chord_spec, command_spec) in entries {
            let chord = match parse_chord(chord_spec) {
                Ok(chord) => chord,
                Err(error) => {
                    eprintln!(
                        "horizon config: skipping keybinding `{chord_spec}` = \
                         `{command_spec}`: {error}"
                    );
                    continue;
                }
            };
            if command_spec == OPEN_PALETTE_PSEUDO_COMMAND {
                palette_chord = Some(chord);
                continue;
            }
            if command_spec == WORKSPACE_MODE_PSEUDO_COMMAND {
                workspace_mode_chord = chord;
                workspace_mode_chord_label = chord_spec.clone();
                continue;
            }
            let Some(command_id) = command_id_from_str(command_spec) else {
                eprintln!(
                    "horizon config: skipping keybinding `{chord_spec}` = `{command_spec}`: \
                     unknown command id `{command_spec}`"
                );
                continue;
            };
            // A config entry for a chord already bound (by a default, or by
            // an earlier entry) replaces it rather than adding a second,
            // unreachable binding for the same chord.
            bindings.retain(|(existing, _)| existing != &chord);
            bindings.push((chord, command_id));
        }

        Keymap {
            bindings,
            palette_chord,
            workspace_mode_chord,
            workspace_mode_chord_label,
        }
    }

    /// The `CommandId` bound to `event`'s chord, if any.
    pub(crate) fn command_for(&self, event: &KeyEvent) -> Option<CommandId> {
        self.bindings
            .iter()
            .find(|(chord, _)| chord.matches(event.modifiers, &event.key.logical_key))
            .map(|(_, command_id)| *command_id)
    }

    /// Whether `event` matches the command-palette open chord. Always
    /// `false` unless a config entry set one — see
    /// [`OPEN_PALETTE_PSEUDO_COMMAND`]'s doc comment.
    pub(crate) fn is_palette_open(&self, event: &KeyEvent) -> bool {
        self.palette_chord
            .is_some_and(|chord| chord.matches(event.modifiers, &event.key.logical_key))
    }

    /// Whether `event` matches the workspace-mode entry chord.
    pub(crate) fn is_workspace_mode_enter(&self, event: &KeyEvent) -> bool {
        self.workspace_mode_chord
            .matches(event.modifiers, &event.key.logical_key)
    }

    /// The chord spec string (e.g. `"ctrl+'"`) for the workspace-mode entry
    /// chord currently in effect -- the built-in default, or whatever a
    /// `[keybindings]` entry overrode it to. Purely for display -- see
    /// `app::status_bar`, which builds its "how to enter workspace mode"
    /// hint from this instead of a hardcoded string.
    pub(crate) fn workspace_mode_chord_label(&self) -> &str {
        &self.workspace_mode_chord_label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_input_keeps_space() {
        assert_eq!(
            character_input(" ", Modifiers::default()),
            Some(b" ".to_vec())
        );
    }

    #[test]
    fn character_input_keeps_utf8_text() {
        assert_eq!(
            character_input("日本語", Modifiers::default()),
            Some("日本語".as_bytes().to_vec())
        );
    }

    #[test]
    fn control_space_input_is_nul() {
        assert_eq!(control_input(' '), None);
    }

    #[test]
    fn agent_draft_accepts_plain_text() {
        assert_eq!(
            agent_draft_action(&Key::Character("hello".into()), Modifiers::default()),
            Some(AgentDraftAction::Insert("hello".to_string()))
        );
    }

    #[test]
    fn agent_draft_accepts_submit_and_backspace() {
        assert_eq!(
            agent_draft_action(&Key::Named(NamedKey::Enter), Modifiers::default()),
            Some(AgentDraftAction::Submit)
        );
        assert_eq!(
            agent_draft_action(&Key::Named(NamedKey::Backspace), Modifiers::default()),
            Some(AgentDraftAction::Backspace)
        );
    }

    #[test]
    fn agent_draft_keeps_control_shortcuts_available() {
        assert_eq!(
            agent_draft_action(&Key::Character("p".into()), Modifiers::CONTROL),
            None
        );
    }

    #[test]
    fn ctrl_shift_v_is_terminal_paste() {
        assert!(is_terminal_paste_input(
            &Key::Character("v".into()),
            Modifiers::CONTROL | Modifiers::SHIFT
        ));
    }

    #[test]
    fn ctrl_v_remains_terminal_control_input() {
        assert!(!is_terminal_paste_input(
            &Key::Character("v".into()),
            Modifiers::CONTROL
        ));
        assert_eq!(character_input("v", Modifiers::CONTROL), Some(vec![0x16]));
    }

    #[test]
    fn ctrl_shift_c_is_terminal_copy() {
        assert!(is_terminal_copy_input(
            &Key::Character("c".into()),
            Modifiers::CONTROL | Modifiers::SHIFT
        ));
    }

    #[test]
    fn ctrl_c_remains_terminal_control_input() {
        assert!(!is_terminal_copy_input(
            &Key::Character("c".into()),
            Modifiers::CONTROL
        ));
        assert_eq!(character_input("c", Modifiers::CONTROL), Some(vec![0x03]));
    }

    #[test]
    fn named_arrow_uses_termwiz_key_path() {
        assert_eq!(
            terminal_key_from_input(&Key::Named(NamedKey::ArrowUp)),
            Some(TermKeyCode::UpArrow)
        );
    }

    // --- text-key routing (`terminal_key_from_character`) ----------------
    //
    // `terminal_key_from_key` now claims single-character keystrokes for
    // `TerminalCommand::Key` (routing them through the terminal's live
    // Kitty state — see `terminal::protocol::kitty_keyboard::encode_text_key`)
    // instead of leaving them to `terminal_input_from_key`'s
    // `character_input` bypass. These tests cover the routing decision
    // itself; `terminal::tests`' `legacy_text_key_matches_pre_existing_
    // bytes_over_printable_range_and_ctrl_table` and `csi_u_text_key_*`
    // cover the resulting bytes.

    #[test]
    fn terminal_key_from_character_unshifts_ascii_letters() {
        // winit folds Shift into the text itself ("A" for a Shift+A press);
        // termwiz's own `KeyCode::Char` convention expects the base
        // (unshifted) character instead, with Shift carried in `Modifiers`
        // — see `kitty_keyboard::encode_text_key`'s doc comment.
        assert_eq!(
            terminal_key_from_character("A", Modifiers::SHIFT),
            Some(TermKeyCode::Char('a'))
        );
    }

    #[test]
    fn terminal_key_from_character_cannot_unshift_punctuation() {
        // Shift+1 -> '!' on a US layout has no algorithmic base-codepoint
        // inverse without OS keyboard-layout data, so it passes through as
        // received — a documented deviation (see `KITTY_COMPLIANCE`).
        assert_eq!(
            terminal_key_from_character("!", Modifiers::SHIFT),
            Some(TermKeyCode::Char('!'))
        );
    }

    #[test]
    fn terminal_key_from_character_falls_through_for_multi_char_text() {
        // Not a single keystroke `TermKeyCode::Char` can represent —
        // `handle_terminal_key` falls back to `terminal_input_from_key`/
        // `character_input` for these, unchanged.
        assert_eq!(
            terminal_key_from_character("ab", Modifiers::default()),
            None
        );
    }

    #[test]
    fn terminal_key_from_character_falls_through_for_meta_without_ctrl() {
        // Preserves `character_input`'s existing "Super/Cmd swallows the
        // keystroke" behavior for the case that still reaches it.
        assert_eq!(terminal_key_from_character("a", Modifiers::META), None);
    }

    #[test]
    fn terminal_key_from_character_claims_ctrl_meta_combo() {
        // Ctrl takes priority over Meta, matching `character_input`'s Ctrl
        // branch (which returns before ever checking Meta).
        assert_eq!(
            terminal_key_from_character("a", Modifiers::CONTROL | Modifiers::META),
            Some(TermKeyCode::Char('a'))
        );
    }

    #[test]
    fn key_event_kind_reflects_the_repeat_flag() {
        assert_eq!(key_event_kind_from_repeat(false), KeyEventKind::Press);
        assert_eq!(key_event_kind_from_repeat(true), KeyEventKind::Repeat);
    }

    #[test]
    fn modifiers_convert_to_termwiz() {
        assert_eq!(
            termwiz_modifiers(Modifiers::CONTROL | Modifiers::SHIFT),
            TermModifiers::CTRL | TermModifiers::SHIFT
        );
    }

    // --- key chord parsing --------------------------------------------

    #[test]
    fn parses_a_multi_modifier_chord_case_insensitively() {
        let chord = parse_chord("Ctrl+Shift+T").expect("valid chord");
        assert_eq!(
            chord,
            Chord {
                ctrl: true,
                shift: true,
                alt: false,
                meta: false,
                key: ChordKey::Char('t'),
            }
        );
    }

    #[test]
    fn parses_a_named_key_chord() {
        let chord = parse_chord("alt+enter").expect("valid chord");
        assert_eq!(chord.key, ChordKey::Named(NamedChordKey::Enter));
        assert!(chord.alt);
    }

    #[test]
    fn rejects_a_chord_with_no_key() {
        assert!(parse_chord("ctrl+shift").is_err());
    }

    #[test]
    fn rejects_an_empty_chord() {
        assert!(parse_chord("").is_err());
    }

    #[test]
    fn rejects_an_unknown_modifier() {
        assert!(parse_chord("hyper+t").is_err());
    }

    #[test]
    fn chord_matches_exact_modifiers_only() {
        let chord = parse_chord("ctrl+shift+t").expect("valid chord");
        let key = Key::Character("t".into());

        assert!(chord.matches(Modifiers::CONTROL | Modifiers::SHIFT, &key));
        assert!(!chord.matches(Modifiers::CONTROL, &key));
        assert!(!chord.matches(Modifiers::CONTROL | Modifiers::SHIFT | Modifiers::ALT, &key));
    }

    #[test]
    fn chord_key_matching_is_case_insensitive_and_single_char_only() {
        let key_lower = Key::Character("t".into());
        let key_upper = Key::Character("T".into());
        let multi_char = Key::Character("th".into());

        assert!(ChordKey::Char('t').matches(&key_lower));
        assert!(ChordKey::Char('t').matches(&key_upper));
        assert!(!ChordKey::Char('t').matches(&multi_char));
    }

    // --- keymap resolution: defaults + config precedence ----------------

    #[test]
    fn config_entry_overrides_a_default_bound_to_the_same_chord() {
        let mut entries = HashMap::new();
        entries.insert("ctrl+shift+t".to_string(), "new-agent".to_string());

        let keymap = Keymap::from_entries(&entries);
        let chord = parse_chord("ctrl+shift+t").unwrap();

        assert_eq!(
            keymap
                .bindings
                .iter()
                .find(|(bound, _)| *bound == chord)
                .map(|(_, id)| *id),
            Some(CommandId::NewAgent)
        );
    }

    #[test]
    fn config_entry_adds_a_new_binding() {
        let mut entries = HashMap::new();
        entries.insert(
            "ctrl+shift+q".to_string(),
            "terminate-active-session".to_string(),
        );

        let keymap = Keymap::from_entries(&entries);
        let chord = parse_chord("ctrl+shift+q").unwrap();

        assert_eq!(
            keymap
                .bindings
                .iter()
                .find(|(bound, _)| *bound == chord)
                .map(|(_, id)| *id),
            Some(CommandId::TerminateActiveSession)
        );
    }

    #[test]
    fn invalid_chord_is_skipped_without_dropping_other_entries() {
        let mut entries = HashMap::new();
        entries.insert("not a chord".to_string(), "new-agent".to_string());
        entries.insert(
            "ctrl+shift+q".to_string(),
            "terminate-active-session".to_string(),
        );

        let keymap = Keymap::from_entries(&entries);

        assert!(command_id_from_str("new-agent").is_some());
        assert_eq!(keymap.bindings.len(), default_bindings().len() + 1);
    }

    #[test]
    fn unknown_command_id_is_skipped_without_dropping_other_entries() {
        let mut entries = HashMap::new();
        entries.insert("ctrl+shift+z".to_string(), "not-a-real-command".to_string());

        let keymap = Keymap::from_entries(&entries);

        assert_eq!(keymap.bindings.len(), default_bindings().len());
    }

    #[test]
    fn default_bindings_are_present_when_config_is_empty() {
        let keymap = Keymap::from_entries(&HashMap::new());
        assert_eq!(keymap.bindings.len(), default_bindings().len());
    }

    // --- palette-open pseudo-command ------------------------------------

    #[test]
    fn palette_chord_is_unbound_when_config_is_empty() {
        // No built-in default any more (`docs/tasks/backlog.md` item 1,
        // resolved): opening the palette is a workspace-mode resident
        // (`:`), not a global shortcut.
        let keymap = Keymap::from_entries(&HashMap::new());
        assert_eq!(keymap.palette_chord, None);
    }

    #[test]
    fn open_palette_entry_sets_the_palette_chord() {
        let mut entries = HashMap::new();
        entries.insert("ctrl+shift+p".to_string(), "open-palette".to_string());

        let keymap = Keymap::from_entries(&entries);

        assert_eq!(
            keymap.palette_chord,
            Some(parse_chord("ctrl+shift+p").unwrap())
        );
        // Not a real command id, so it never lands in `bindings`.
        assert_eq!(keymap.bindings.len(), default_bindings().len());
    }

    // --- workspace-mode-entry pseudo-command ----------------------------

    #[test]
    fn workspace_mode_chord_defaults_to_ctrl_quote_when_config_is_empty() {
        let keymap = Keymap::from_entries(&HashMap::new());
        assert_eq!(keymap.workspace_mode_chord, parse_chord("ctrl+'").unwrap());
        assert_eq!(keymap.workspace_mode_chord_label(), "ctrl+'");
    }

    #[test]
    fn workspace_mode_entry_overrides_the_default_chord() {
        let mut entries = HashMap::new();
        entries.insert("ctrl+space".to_string(), "workspace-mode".to_string());

        let keymap = Keymap::from_entries(&entries);

        assert_eq!(
            keymap.workspace_mode_chord,
            parse_chord("ctrl+space").unwrap()
        );
        assert_eq!(keymap.workspace_mode_chord_label(), "ctrl+space");
        // Not a real command id, so it never lands in `bindings`.
        assert_eq!(keymap.bindings.len(), default_bindings().len());
    }

    #[test]
    fn parses_a_chord_with_an_apostrophe_key() {
        // The shipped workspace-mode default (`DEFAULT_WORKSPACE_MODE_CHORD`)
        // -- apostrophe is an ordinary single-character key, not a named one,
        // but exercised explicitly here since it's now load-bearing.
        let chord = parse_chord("ctrl+'").expect("valid chord");
        assert_eq!(
            chord,
            Chord {
                ctrl: true,
                shift: false,
                alt: false,
                meta: false,
                key: ChordKey::Char('\''),
            }
        );
        assert!(chord.matches(Modifiers::CONTROL, &Key::Character("'".into())));
    }
}
