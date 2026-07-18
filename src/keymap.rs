//! Translates `[keybindings]` config entries (`horizon_config::load()
//! .keybindings`, a chord string -> command id map) into gpui's own
//! keystroke string format and `CommandId`s, mirroring the semantics of
//! the Floem shell's `src/app/keymap.rs` (chord syntax, command id names,
//! reserved pseudo-commands). Only the target format differs: gpui
//! keystrokes join modifiers with `-` (lowercase) instead of `+`, and
//! bindings are wired up in `workspace::init` instead of through a
//! process-wide `Keymap` type.
//!
//! An entry that doesn't parse (bad chord syntax, unknown command id) is
//! the caller's job to warn about and skip — these functions just return
//! `None`, never panic.

use horizon_config::RawConfig;
use horizon_workspace::commands::CommandId;

/// The reserved `[keybindings]` value that binds a chord to the existing
/// `OpenPalette` action globally, rather than to a `CommandId` — opening
/// the palette isn't an operation the palette itself can list or run.
/// Resolved by `workspace::init` directly, not through [`command_for`].
pub(crate) const OPEN_PALETTE_PSEUDO_COMMAND: &str = "open-palette";

/// The reserved `[keybindings]` value that overrides the chord bound to
/// the existing `ToggleWorkspaceMode` action, in place of the built-in
/// default. Resolved by `workspace::init` directly, not through
/// [`command_for`].
pub(crate) const WORKSPACE_MODE_PSEUDO_COMMAND: &str = "workspace-mode";

/// Parses a `[keybindings]` chord string (modifiers joined by `+`, ending
/// in the key, case-insensitive -- e.g. `"Ctrl+Shift+T"`) into gpui's own
/// keystroke string format (modifiers joined by `-`, all lowercase --
/// e.g. `"ctrl-shift-t"`, suitable for `KeyBinding::new`). Returns `None`
/// for anything unparsable: an empty chord, an unknown modifier name, a
/// chord with no key, or a key token that's neither a single character
/// nor a recognized named key.
pub(crate) fn gpui_keystroke(chord: &str) -> Option<String> {
    let parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }

    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut cmd = false;
    let mut key_token = None;

    for (index, part) in parts.iter().enumerate() {
        let is_last = index == parts.len() - 1;
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "meta" | "cmd" | "command" | "super" | "win" => cmd = true,
            _ if is_last => key_token = Some(*part),
            _ => return None,
        }
    }

    let key = gpui_key_name(key_token?)?;

    let mut keystroke = String::new();
    if ctrl {
        keystroke.push_str("ctrl-");
    }
    if shift {
        keystroke.push_str("shift-");
    }
    if alt {
        keystroke.push_str("alt-");
    }
    if cmd {
        keystroke.push_str("cmd-");
    }
    keystroke.push_str(&key);
    Some(keystroke)
}

/// Maps a chord's key token to gpui's own key name: either one of its
/// named keys (`enter`, `escape`, `tab`, `space`, `backspace`, `delete`,
/// `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown` --
/// see `gpui_macos::events`'s keycode tables) or a single character passed
/// through as-is. Named-key aliases mirror the Floem shell's
/// `app::keymap::NamedChordKey::parse` (`esc`, `return`, `spacebar`,
/// `del`, `arrowup`/.../`arrowright`).
fn gpui_key_name(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => "enter",
        "escape" | "esc" => "escape",
        "tab" => "tab",
        "space" | "spacebar" => "space",
        "backspace" => "backspace",
        "delete" | "del" => "delete",
        "up" | "arrowup" => "up",
        "down" | "arrowdown" => "down",
        "left" | "arrowleft" => "left",
        "right" | "arrowright" => "right",
        "home" => "home",
        "end" => "end",
        "pageup" => "pageup",
        "pagedown" => "pagedown",
        _ => {
            let mut chars = lower.chars();
            let first = chars.next()?;
            return if chars.next().is_none() {
                Some(first.to_string())
            } else {
                None
            };
        }
    };
    Some(named.to_string())
}

/// Resolves a `[keybindings]` command id (kebab-case, e.g.
/// `"split-right"`) to a `CommandId` -- the GPUI shell's counterpart of
/// the Floem shell's `app::keymap::command_id_from_str`. The reserved
/// pseudo-commands ([`OPEN_PALETTE_PSEUDO_COMMAND`],
/// [`WORKSPACE_MODE_PSEUDO_COMMAND`]) are not real commands and must be
/// handled by the caller before falling back to this.
pub(crate) fn command_for(id: &str) -> Option<CommandId> {
    match id {
        "split-right" => Some(CommandId::SplitRight),
        "split-down" => Some(CommandId::SplitDown),
        "new-tab" => Some(CommandId::NewTab),
        "focus-next-pane" => Some(CommandId::FocusNextPane),
        "close-active-pane" => Some(CommandId::CloseActivePane),
        "close-active-tab" => Some(CommandId::CloseActiveTab),
        "terminate-active-session" => Some(CommandId::TerminateActiveSession),
        "approve-tool-call" => Some(CommandId::ApproveToolCall),
        "deny-tool-call" => Some(CommandId::DenyToolCall),
        "cancel-agent-turn" => Some(CommandId::CancelAgentTurn),
        "reload-session-runtime" => Some(CommandId::ReloadSessionRuntime),
        "reload-config" => Some(CommandId::ReloadConfig),
        "manage-sessions" => Some(CommandId::OpenSessionManager),
        _ => None,
    }
}

/// What a `[keybindings]` chord dispatches to once bound — either the
/// reserved "open the command palette" pseudo-command or a real
/// `CommandId` run through `RunCommand`. The workspace-mode toggle chord
/// isn't covered here: it replaces a fixed built-in action rather than
/// adding a new dispatch target, so it's resolved separately by
/// [`workspace_mode_keystroke`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeybindingTarget {
    OpenPalette,
    Command(CommandId),
}

/// One `[keybindings]` entry, successfully parsed into a gpui keystroke
/// string and its dispatch target — see [`resolve_keybindings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedKeybinding {
    pub(crate) keystroke: String,
    pub(crate) target: KeybindingTarget,
}

/// Parses every `[keybindings]` entry other than the workspace-mode
/// pseudo-command (owned by [`workspace_mode_keystroke`]) into gpui
/// keystroke strings and dispatch targets. An entry with an unparsable
/// chord or an unknown command id is skipped with a stderr warning, same
/// policy as at startup. Pure and config-only (no `gpui::App` needed), so
/// `workspace::derive_bindings` can share this between startup and
/// `Reload Config` and this module can unit-test the parsing/warning
/// behavior directly.
pub(crate) fn resolve_keybindings(config: &RawConfig) -> Vec<ResolvedKeybinding> {
    let mut resolved = Vec::new();
    for (chord, command) in &config.keybindings {
        if command == WORKSPACE_MODE_PSEUDO_COMMAND {
            continue;
        }
        let Some(keystroke) = gpui_keystroke(chord) else {
            eprintln!(
                "horizon config: skipping keybinding `{chord}` = `{command}`: unrecognized chord"
            );
            continue;
        };
        let target = if command == OPEN_PALETTE_PSEUDO_COMMAND {
            KeybindingTarget::OpenPalette
        } else if let Some(id) = command_for(command) {
            KeybindingTarget::Command(id)
        } else {
            eprintln!(
                "horizon config: skipping keybinding `{chord}` = `{command}`: unknown command id"
            );
            continue;
        };
        resolved.push(ResolvedKeybinding { keystroke, target });
    }
    resolved
}

/// Resolves the workspace-mode toggle's chord: the `[keybindings]`
/// override if present and parseable, else `default_keystroke`. A
/// present-but-unparsable override warns to stderr and falls back to the
/// default, same policy as [`resolve_keybindings`].
pub(crate) fn workspace_mode_keystroke(config: &RawConfig, default_keystroke: &str) -> String {
    let Some((chord, _)) = config
        .keybindings
        .iter()
        .find(|(_, command)| command.as_str() == WORKSPACE_MODE_PSEUDO_COMMAND)
    else {
        return default_keystroke.to_string();
    };
    gpui_keystroke(chord).unwrap_or_else(|| {
        eprintln!(
            "horizon config: skipping keybinding `{chord}` = \
             `{WORKSPACE_MODE_PSEUDO_COMMAND}`: unrecognized chord"
        );
        default_keystroke.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_a_mixed_case_multi_modifier_chord() {
        assert_eq!(
            gpui_keystroke("Ctrl+Shift+T"),
            Some("ctrl-shift-t".to_string())
        );
    }

    #[test]
    fn translates_alias_modifiers() {
        assert_eq!(
            gpui_keystroke("control+option+command+a"),
            Some("ctrl-alt-cmd-a".to_string())
        );
        assert_eq!(gpui_keystroke("super+a"), Some("cmd-a".to_string()));
        assert_eq!(gpui_keystroke("win+a"), Some("cmd-a".to_string()));
        assert_eq!(gpui_keystroke("meta+a"), Some("cmd-a".to_string()));
    }

    #[test]
    fn translates_named_keys_to_gpui_names() {
        assert_eq!(gpui_keystroke("ctrl+enter"), Some("ctrl-enter".to_string()));
        assert_eq!(gpui_keystroke("ctrl+arrowup"), Some("ctrl-up".to_string()));
        assert_eq!(gpui_keystroke("ctrl+esc"), Some("ctrl-escape".to_string()));
        assert_eq!(
            gpui_keystroke("ctrl+pagedown"),
            Some("ctrl-pagedown".to_string())
        );
    }

    #[test]
    fn keeps_a_single_character_key_as_is() {
        assert_eq!(gpui_keystroke("ctrl+'"), Some("ctrl-'".to_string()));
    }

    #[test]
    fn rejects_an_empty_chord() {
        assert_eq!(gpui_keystroke(""), None);
    }

    #[test]
    fn rejects_a_chord_with_no_key() {
        assert_eq!(gpui_keystroke("ctrl+shift"), None);
    }

    #[test]
    fn rejects_an_unknown_modifier() {
        assert_eq!(gpui_keystroke("hyper+t"), None);
    }

    #[test]
    fn rejects_a_multi_character_unnamed_key() {
        assert_eq!(gpui_keystroke("ctrl+th"), None);
    }

    #[test]
    fn resolves_known_command_ids() {
        assert_eq!(command_for("split-right"), Some(CommandId::SplitRight));
        assert_eq!(command_for("new-tab"), Some(CommandId::NewTab));
        assert_eq!(
            command_for("manage-sessions"),
            Some(CommandId::OpenSessionManager)
        );
    }

    #[test]
    fn rejects_an_unknown_command_id() {
        assert_eq!(command_for("not-a-real-command"), None);
        assert_eq!(command_for(OPEN_PALETTE_PSEUDO_COMMAND), None);
        assert_eq!(command_for(WORKSPACE_MODE_PSEUDO_COMMAND), None);
    }

    fn config_with_keybindings(entries: &[(&str, &str)]) -> RawConfig {
        RawConfig {
            keybindings: entries
                .iter()
                .map(|(chord, command)| (chord.to_string(), command.to_string()))
                .collect(),
            ..RawConfig::default()
        }
    }

    #[test]
    fn resolve_keybindings_translates_a_command_entry() {
        let config = config_with_keybindings(&[("ctrl+shift+t", "new-tab")]);
        assert_eq!(
            resolve_keybindings(&config),
            vec![ResolvedKeybinding {
                keystroke: "ctrl-shift-t".to_string(),
                target: KeybindingTarget::Command(CommandId::NewTab),
            }]
        );
    }

    #[test]
    fn resolve_keybindings_translates_the_open_palette_pseudo_command() {
        let config = config_with_keybindings(&[("ctrl+p", OPEN_PALETTE_PSEUDO_COMMAND)]);
        assert_eq!(
            resolve_keybindings(&config),
            vec![ResolvedKeybinding {
                keystroke: "ctrl-p".to_string(),
                target: KeybindingTarget::OpenPalette,
            }]
        );
    }

    #[test]
    fn resolve_keybindings_skips_the_workspace_mode_pseudo_command() {
        let config = config_with_keybindings(&[("ctrl+'", WORKSPACE_MODE_PSEUDO_COMMAND)]);
        assert_eq!(resolve_keybindings(&config), vec![]);
    }

    #[test]
    fn resolve_keybindings_skips_an_unparsable_chord() {
        let config = config_with_keybindings(&[("hyper+t", "new-tab")]);
        assert_eq!(resolve_keybindings(&config), vec![]);
    }

    #[test]
    fn resolve_keybindings_skips_an_unknown_command_id() {
        let config = config_with_keybindings(&[("ctrl+t", "not-a-real-command")]);
        assert_eq!(resolve_keybindings(&config), vec![]);
    }

    #[test]
    fn workspace_mode_keystroke_falls_back_to_the_default_when_unset() {
        let config = RawConfig::default();
        assert_eq!(workspace_mode_keystroke(&config, "ctrl-'"), "ctrl-'");
    }

    #[test]
    fn workspace_mode_keystroke_uses_the_override_when_present() {
        let config = config_with_keybindings(&[("ctrl+space", WORKSPACE_MODE_PSEUDO_COMMAND)]);
        assert_eq!(workspace_mode_keystroke(&config, "ctrl-'"), "ctrl-space");
    }

    #[test]
    fn workspace_mode_keystroke_falls_back_on_an_unparsable_override() {
        let config = config_with_keybindings(&[("hyper+x", WORKSPACE_MODE_PSEUDO_COMMAND)]);
        assert_eq!(workspace_mode_keystroke(&config, "ctrl-'"), "ctrl-'");
    }
}
