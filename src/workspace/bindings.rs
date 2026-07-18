//! `[keybindings]` chord/command translation into gpui `KeyBinding`s:
//! [`derive_bindings`] builds the full set (fixed workspace-mode
//! navigation, the workspace-mode toggle chord, and every config entry),
//! and [`apply_bindings`] installs it -- unbinding whatever the previous
//! apply's config-driven chords were first, so `Reload Config` can swap
//! the set live (see its own doc comment for the unbind mechanism).

use gpui::*;

use super::{RunCommand, MODE_CONTEXT};
use crate::keymap;

/// Built-in default chord for [`super::ToggleWorkspaceMode`] — mirrors the
/// Floem shell's `DEFAULT_WORKSPACE_MODE_CHORD`. Not bound when a
/// `[keybindings]` entry overrides it via the reserved
/// `keymap::WORKSPACE_MODE_PSEUDO_COMMAND` (see [`super::init`]).
const DEFAULT_WORKSPACE_MODE_KEYSTROKE: &str = "ctrl-'";

/// gpui-component's `List` (shared by the command palette, the view
/// chooser, and the session manager modal) binds arrow-key selection
/// movement to its own `ui::SelectUp`/`ui::SelectDown` actions in key
/// context `"List"` — see gpui-component's `crates/ui/src/list/list.rs`.
/// That `actions` module is crate-private, so the action types can't be
/// named from here; `cx.build_action` (gpui's mechanism for resolving an
/// action by its registered namespaced name, the same path a JSON keymap
/// file would use) builds an instance dynamically instead. Binding Tab and
/// Shift+Tab to these same actions in the "List" context is the intended
/// way to extend a third-party action gpui-component doesn't expose a
/// public Rust path to, and it lands the behavior on every List-backed
/// modal for free since they all share this widget.
fn list_select_binding(cx: &App, keystroke: &str, action_name: &str) -> KeyBinding {
    let action = cx.build_action(action_name, None).unwrap_or_else(|err| {
        panic!("gpui-component action `{action_name}` not registered: {err}")
    });
    let context_predicate = KeyBindingContextPredicate::parse("List")
        .expect("`List` is a valid key context predicate")
        .into();
    KeyBinding::load(
        keystroke,
        action,
        Some(context_predicate),
        false,
        None,
        cx.keyboard_mapper().as_ref(),
    )
    .unwrap_or_else(|err| panic!("invalid keystroke `{keystroke}`: {err}"))
}

/// Builds the full key binding set from `config`: the fixed
/// workspace-mode navigation/action chords (hardcoded, never
/// configurable), the workspace-mode toggle chord (default or a
/// `[keybindings]` override), and each `[keybindings]` entry. Returns the
/// bindings alongside the gpui keystroke strings of the config-driven
/// subset (`dynamic_keystrokes` — everything but the fixed navigation
/// chords and the two List-context ones) — [`apply_bindings`] needs that
/// subset to know what a *later* reload must be able to unbind.
fn derive_bindings(cx: &App, config: &horizon_config::RawConfig) -> (Vec<KeyBinding>, Vec<String>) {
    let mut bindings = vec![
        KeyBinding::new("h", super::ModeMoveLeft, Some(MODE_CONTEXT)),
        KeyBinding::new("j", super::ModeMoveDown, Some(MODE_CONTEXT)),
        KeyBinding::new("k", super::ModeMoveUp, Some(MODE_CONTEXT)),
        KeyBinding::new("l", super::ModeMoveRight, Some(MODE_CONTEXT)),
        KeyBinding::new("left", super::ModeMoveLeft, Some(MODE_CONTEXT)),
        KeyBinding::new("down", super::ModeMoveDown, Some(MODE_CONTEXT)),
        KeyBinding::new("up", super::ModeMoveUp, Some(MODE_CONTEXT)),
        KeyBinding::new("right", super::ModeMoveRight, Some(MODE_CONTEXT)),
        KeyBinding::new("enter", super::ModeCommit, Some(MODE_CONTEXT)),
        KeyBinding::new("escape", super::ModeCancel, Some(MODE_CONTEXT)),
        KeyBinding::new("t", super::NewTab, Some(MODE_CONTEXT)),
        KeyBinding::new("a", super::NewAgentTab, Some(MODE_CONTEXT)),
        KeyBinding::new("s", super::SplitPane, Some(MODE_CONTEXT)),
        KeyBinding::new("x", super::ClosePane, Some(MODE_CONTEXT)),
        KeyBinding::new("tab", super::NextTab, Some(MODE_CONTEXT)),
        KeyBinding::new(":", super::OpenPalette, Some(MODE_CONTEXT)),
    ];

    let mut dynamic_keystrokes = Vec::new();

    let workspace_mode_keystroke =
        keymap::workspace_mode_keystroke(config, DEFAULT_WORKSPACE_MODE_KEYSTROKE);
    bindings.push(KeyBinding::new(
        &workspace_mode_keystroke,
        super::ToggleWorkspaceMode,
        None,
    ));
    dynamic_keystrokes.push(workspace_mode_keystroke);

    // `[keybindings]` config entries layer on top of the built-ins above:
    // later-registered bindings take precedence in gpui at the same
    // context depth (`Keymap::bindings_for_input`'s doc comment — "the
    // ones added to the keymap later take precedence"), so pushing these
    // after the built-ins is enough for a config entry to override one
    // bound to the same chord.
    for resolved in keymap::resolve_keybindings(config) {
        match resolved.target {
            keymap::KeybindingTarget::OpenPalette => {
                bindings.push(KeyBinding::new(
                    &resolved.keystroke,
                    super::OpenPalette,
                    None,
                ));
            }
            keymap::KeybindingTarget::Command(id) => {
                bindings.push(KeyBinding::new(
                    &resolved.keystroke,
                    RunCommand { id },
                    None,
                ));
            }
        }
        dynamic_keystrokes.push(resolved.keystroke);
    }

    // Tab / Shift+Tab move the selection in every List-backed modal
    // (command palette, view chooser, session manager) the same way
    // Up/Down already do. gpui-component's `Input` binds "tab" to an
    // inline-indent action in its own (more specific) "Input" context,
    // but the List's query input is single-line, so that handler finds
    // nothing to indent and propagates — letting these "List"-context
    // bindings fire next even while the query input has focus. See
    // `list_select_binding`'s doc comment for why the actions are built
    // dynamically instead of bound by type.
    bindings.push(list_select_binding(cx, "tab", "ui::SelectDown"));
    bindings.push(list_select_binding(cx, "shift-tab", "ui::SelectUp"));

    (bindings, dynamic_keystrokes)
}

/// The gpui keystroke strings most recently bound by the config-driven
/// portion of [`derive_bindings`] (the workspace-mode toggle chord plus
/// each `[keybindings]` entry) — everything [`apply_bindings`] needs to
/// unbind before installing a freshly reloaded config's set. Lazily
/// initialized empty (nothing applied yet); mirrors `theme::scheme_store`'s
/// shape for the same "live app-wide state `Reload Config` mutates" need.
fn dynamic_keystroke_store() -> &'static std::sync::RwLock<Vec<String>> {
    static STORE: std::sync::OnceLock<std::sync::RwLock<Vec<String>>> = std::sync::OnceLock::new();
    STORE.get_or_init(|| std::sync::RwLock::new(Vec::new()))
}

/// Applies [`derive_bindings`]'s output for `config`: explicitly unbinds
/// every keystroke the config-driven subset used on the *previous* apply,
/// then binds the freshly derived set. Called by both [`super::init`]
/// (previous set is empty, so the unbind step is a no-op) and the
/// `ReloadConfig` command.
///
/// gpui's `Keymap` is append-only (`Keymap::add_bindings`/`Keymap::clear`
/// — see the pinned checkout's `crates/gpui/src/keymap.rs`): binding the
/// same chord twice doesn't replace the old entry, it just makes the
/// newer one win precedence (`Keymap::bindings_for_input`'s doc comment:
/// "the ones added to the keymap later take precedence"). That's enough
/// when a chord's *target* changes, but not when a chord disappears from
/// config entirely — nothing "later" exists to out-rank the stale entry,
/// so it would keep firing after a reload that removed it.
/// `App::clear_key_bindings` does exist, but it resets gpui's *entire*
/// process-wide keymap, which would also wipe gpui-component's own
/// internal bindings (`gpui_component::init`, called once at startup) and
/// `main.rs`'s `cmd-q` — unacceptable collateral damage for a config
/// reload, so it's not used here.
///
/// The precise tool is a `KeyBinding` bound to gpui's `NoAction`
/// pseudo-action at the stale keystroke, global (`None`) context — the
/// same context every dynamic binding here uses, so it competes at the
/// same precedence tier. Reading `Keymap::bindings_for_input`'s match loop
/// directly: a `NoAction` binding with no `meta` set (true of every
/// binding built via `KeyBinding::new`, since nothing here calls
/// `with_meta`/`set_meta`) is unconditionally treated as a user override
/// and `break`s resolution for that keystroke — so pushing these unbind
/// markers *before* the freshly derived bindings guarantees: a keystroke
/// no longer in the new set resolves to nothing (the marker is the last,
/// i.e. highest-precedence, entry for it); a keystroke still in the new
/// set resolves to the new target (the freshly-bound entry, pushed after
/// the marker, outranks it).
pub(super) fn apply_bindings(cx: &mut App, config: &horizon_config::RawConfig) {
    let (bindings, dynamic_keystrokes) = derive_bindings(cx, config);

    let previous = std::mem::replace(
        &mut *dynamic_keystroke_store().write().unwrap(),
        dynamic_keystrokes,
    );
    if !previous.is_empty() {
        let unbinds: Vec<KeyBinding> = previous
            .into_iter()
            .map(|keystroke| KeyBinding::new(&keystroke, NoAction, None))
            .collect();
        cx.bind_keys(unbinds);
    }
    cx.bind_keys(bindings);
}
