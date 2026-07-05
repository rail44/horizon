use crate::agent::contract::Command;
use crate::app::keymap::{
    agent_draft_action, is_terminal_copy_key, is_terminal_paste_key, pop_last_grapheme_approx,
    terminal_input_from_key, terminal_key_event_kind, terminal_key_from_key, termwiz_modifiers,
    AgentDraftAction,
};
use crate::session::Registry;
use crate::terminal::{KeyEventKind, TerminalCommand};
use crate::workspace::{PaneKind, Workspace};
use floem::action::set_ime_allowed;
use floem::keyboard::{Key, KeyEvent, Modifiers, NamedKey};
use floem::prelude::*;
use floem::Clipboard;

pub(crate) const MAX_VISIBLE_PANES: usize = 4;

pub(crate) type AgentDrafts = [RwSignal<String>; MAX_VISIBLE_PANES];
pub(crate) type PaneFocusRequests = [RwSignal<u64>; MAX_VISIBLE_PANES];

pub(crate) fn active_agent(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_is(PaneKind::Agent))
}

pub(crate) fn active_text_input_pane(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_accepts_text_input())
}

pub(crate) fn request_active_pane_focus(
    workspace: RwSignal<Workspace>,
    pane_focus_requests: PaneFocusRequests,
) {
    let index = workspace.with_untracked(|ws| ws.active_visible_index());
    if let Some(focus_request) = pane_focus_requests.get(index) {
        focus_request.update(|request| *request += 1);
    }
    set_ime_allowed(active_text_input_pane(workspace));
}

pub(crate) fn active_agent_draft(
    workspace: RwSignal<Workspace>,
    agent_drafts: AgentDrafts,
) -> Option<RwSignal<String>> {
    if !active_agent(workspace) {
        return None;
    }

    let index = workspace.with_untracked(|ws| ws.active_visible_index());
    agent_drafts.get(index).copied()
}

pub(crate) fn active_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.active_terminal_session_id())?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub(crate) fn visible_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.visible_terminal_session_id(index))?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub(crate) fn visible_agent_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
) -> Option<crossbeam_channel::Sender<Command>> {
    let session_id = workspace.with_untracked(|ws| ws.visible_agent_session_id(index))?;
    sessions.with_untracked(|registry| registry.agent_sender(session_id))
}

fn handle_terminal_key(
    key_event: &KeyEvent,
    terminal_tx: Option<crossbeam_channel::Sender<TerminalCommand>>,
    event: KeyEventKind,
) -> bool {
    let Some(tx) = terminal_tx else {
        return false;
    };

    if is_terminal_paste_key(key_event) {
        if let Ok(text) = Clipboard::get_contents() {
            let _ = tx.send(TerminalCommand::Paste(text));
            return true;
        }
    }

    if is_terminal_copy_key(key_event) {
        let _ = tx.send(TerminalCommand::CopySelection);
        return true;
    }

    if let Some(key) = terminal_key_from_key(key_event) {
        let _ = tx.send(TerminalCommand::Key {
            key,
            modifiers: termwiz_modifiers(key_event.modifiers),
            event,
        });
        return true;
    }

    if let Some(bytes) = terminal_input_from_key(key_event) {
        let _ = tx.send(TerminalCommand::Input(bytes));
        return true;
    }

    false
}

/// Key-release counterpart to `handle_terminal_key`, called from
/// `Event::KeyUp` (see `handle_active_pane_key_release`). Deliberately
/// narrower: a release only ever means anything for a key that round-trips
/// through `terminal_key_from_key`/`TerminalCommand::Key` — the
/// paste/copy chords and `terminal_input_from_key`'s raw-bytes fallback
/// (multi-character text, a Meta-held character, ...) have no "release"
/// counterpart to send, so they're not checked here at all.
fn handle_terminal_key_release(
    key_event: &KeyEvent,
    terminal_tx: Option<crossbeam_channel::Sender<TerminalCommand>>,
) -> bool {
    let Some(tx) = terminal_tx else {
        return false;
    };

    let Some(key) = terminal_key_from_key(key_event) else {
        return false;
    };

    let _ = tx.send(TerminalCommand::Key {
        key,
        modifiers: termwiz_modifiers(key_event.modifiers),
        event: KeyEventKind::Release,
    });
    true
}

fn handle_agent_key(
    key_event: &KeyEvent,
    draft: RwSignal<String>,
    agent_tx: Option<crossbeam_channel::Sender<Command>>,
) -> bool {
    if is_terminal_paste_key(key_event) {
        if let Ok(text) = Clipboard::get_contents() {
            draft.update(|draft| draft.push_str(&text));
            return true;
        }
    }

    match agent_draft_action(&key_event.key.logical_key, key_event.modifiers) {
        Some(AgentDraftAction::Insert(text)) => {
            draft.update(|draft| draft.push_str(&text));
            true
        }
        Some(AgentDraftAction::Backspace) => {
            draft.update(|draft| {
                pop_last_grapheme_approx(draft);
            });
            true
        }
        Some(AgentDraftAction::Submit) => {
            let text = draft.with_untracked(|draft| draft.trim().to_string());
            if text.is_empty() {
                return true;
            }
            if let Some(tx) = agent_tx {
                let command = Command::UserMessage { text };
                let _ = tx.send(command);
                draft.set(String::new());
            }
            true
        }
        None => false,
    }
}

/// Whether the IME composing guard should still swallow `key` here.
///
/// `ime_composing` and `ime_preedit` are always set together (see
/// `app::input::AppInput`'s `handle_ime_preedit`/`handle_ime_commit`/
/// `handle_ime_disabled`, which clear both on every path that ends
/// composition). This is deliberate belt-and-braces on top of that: if
/// `ime_composing` ever claims an active composition with no preedit text
/// backing it -- a stuck flag desynced from its own preedit, from some
/// future code path or platform quirk neither of us has seen yet -- the
/// guard fails open right here by clearing the flag and letting the key
/// through, instead of swallowing every Character key forever with no way
/// back short of a pane focus change.
fn composing_guard_swallows(
    key: &Key,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
) -> bool {
    if !ime_composing.get_untracked() {
        return false;
    }
    if ime_preedit.with_untracked(Option::is_none) {
        ime_composing.set(false);
        return false;
    }
    matches!(key, Key::Character(_))
}

/// A banner-focused agent pane's response to one keystroke -- see
/// `workspace::view::agent_controls::AgentPaneFocus` and `pane_view`'s
/// `KeyDown` handler, which calls `handle_agent_banner_key` before falling
/// through to `handle_active_pane_key` whenever the pane's approval banner
/// currently holds pane-internal focus.
///
/// Mirrors the crush-inspired (charmbracelet's TUI) design: `y` approves,
/// `n` denies, `Esc` backs out to the message box without answering, and any
/// other printable character is delivered to the message box instead of
/// being swallowed -- the banner must never be a modal trap for ordinary
/// typing. Everything else (Enter, Backspace, arrows, a held modifier
/// chord, ...) is swallowed outright: while the banner holds focus, keys
/// must not leak to the terminal/message box except through that one
/// soft-redirect path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BannerKeyAction {
    Approve,
    Deny,
    /// Leave the banner without answering: pane-internal focus moves back
    /// to the message box, the pending call is left untouched.
    ReleaseFocus,
    /// Deliver `.0` to the message box as if it had been typed there, and
    /// move focus there too.
    Redirect(String),
    /// Consumed with no effect.
    Swallow,
}

fn banner_key_action(key: &Key, modifiers: Modifiers) -> BannerKeyAction {
    if modifiers.control() || modifiers.alt() || modifiers.meta() {
        return BannerKeyAction::Swallow;
    }

    match key {
        Key::Named(NamedKey::Escape) => BannerKeyAction::ReleaseFocus,
        Key::Named(NamedKey::Space) => BannerKeyAction::Redirect(" ".to_string()),
        Key::Character(text) => match text.as_str() {
            "y" | "Y" => BannerKeyAction::Approve,
            "n" | "N" => BannerKeyAction::Deny,
            _ => BannerKeyAction::Redirect(text.to_string()),
        },
        _ => BannerKeyAction::Swallow,
    }
}

/// `Event::KeyDown` entry point for a banner-focused agent pane. Applies the
/// same IME composing guard as the message box/terminal
/// (`composing_guard_swallows`) before considering the key at all, so a
/// composing IME's own keystrokes never reach `y`/`n`/`Esc` handling
/// half-composed.
pub(crate) fn handle_agent_banner_key(
    key_event: &KeyEvent,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
) -> BannerKeyAction {
    if composing_guard_swallows(&key_event.key.logical_key, ime_composing, ime_preedit) {
        return BannerKeyAction::Swallow;
    }
    banner_key_action(&key_event.key.logical_key, key_event.modifiers)
}

pub(crate) fn handle_active_pane_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    agent_draft: RwSignal<String>,
) -> bool {
    if composing_guard_swallows(&key_event.key.logical_key, ime_composing, ime_preedit) {
        return true;
    }

    if workspace.with(|ws| ws.active_visible_pane_is(index, PaneKind::Agent)) {
        return handle_agent_key(
            key_event,
            agent_draft,
            visible_agent_sender(workspace, sessions, index),
        );
    }

    if workspace.with(|ws| ws.active_visible_pane_is(index, PaneKind::Terminal)) {
        return handle_terminal_key(
            key_event,
            visible_terminal_sender(workspace, sessions, index),
            terminal_key_event_kind(key_event),
        );
    }

    false
}

/// `Event::KeyUp` counterpart to `handle_active_pane_key`. Deliberately much
/// narrower than the press side: only the active pane's *terminal* — the
/// one pane kind a key release means anything for at the protocol level
/// (see `KITTY_COMPLIANCE`'s "Report event types" rows) — ever sees a
/// release, and only through `handle_terminal_key_release`. Agent panes,
/// the command palette, global chords (`app::input::AppInput::
/// handle_key_down`) and `app::keymap::Keymap`'s config-driven bindings are
/// never wired to `Event::KeyUp` at all, by construction — a chord already
/// ran its command on the matching `KeyDown`, so there is nothing left for
/// its release to (re-)trigger.
pub(crate) fn handle_active_pane_key_release(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    palette_open: RwSignal<bool>,
) -> bool {
    if composing_guard_swallows(&key_event.key.logical_key, ime_composing, ime_preedit) {
        return true;
    }

    if palette_open.get_untracked() {
        // The palette intercepts most keys before `handle_active_pane_key`
        // ever sees their *press* (see `pane_view`'s `KeyDown` handler,
        // `handle_control_key` and `is_palette_open_key`) — most commonly
        // its own open chord (`ctrl+p` by default), whose press the
        // terminal never received. Forwarding that key's later release
        // anyway would hand the PTY an orphan release with no matching
        // press. This is coarser than mirroring the press side's exact
        // dispatch (`control_surface::handle_palette_key`'s match doesn't
        // claim every key — e.g. a held Ctrl-combo already falls through
        // to the terminal even while the palette is open, so strictly it
        // loses its matching release under this blanket check), but doing
        // better would mean re-running the palette's own key dispatch here
        // just to check whether it *would* have claimed the press, without
        // triggering its side effects twice.
        return true;
    }

    if !workspace.with(|ws| ws.active_visible_pane_is(index, PaneKind::Terminal)) {
        return false;
    }

    handle_terminal_key_release(
        key_event,
        visible_terminal_sender(workspace, sessions, index),
    )
}

pub(crate) fn trace_ime(message: &str) {
    if std::env::var_os("HORIZON_IME_TRACE").is_some() {
        eprintln!("horizon ime: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use floem::keyboard::NamedKey;

    fn composing(preedit: Option<&str>) -> (RwSignal<bool>, RwSignal<Option<String>>) {
        (
            RwSignal::new(true),
            RwSignal::new(preedit.map(str::to_string)),
        )
    }

    #[test]
    fn composing_guard_swallows_character_keys_during_active_composition() {
        let (ime_composing, ime_preedit) = composing(Some("か"));

        assert!(composing_guard_swallows(
            &Key::Character("a".into()),
            ime_composing,
            ime_preedit
        ));
        // A genuinely active composition (preedit text still backing the
        // flag) must not be cleared just because one key was swallowed.
        assert!(ime_composing.get_untracked());
    }

    #[test]
    fn composing_guard_lets_named_keys_through_even_while_composing() {
        // Backspace, Enter, arrows, ... are never routed through the IME's
        // own text -- an IME edits its own preedit directly, so a Named
        // key reaching the app at all means the IME didn't want it. This
        // also documents that a stuck `ime_composing` alone was never
        // capable of blocking Backspace specifically through this guard:
        // whatever a report describes as "Backspace stops working" has to
        // be explained elsewhere (a frozen preedit overlay drawn over the
        // terminal is the leading candidate -- see `ime_preedit`'s use in
        // `workspace::view::pane`/`terminal_output`).
        let (ime_composing, ime_preedit) = composing(Some("か"));

        assert!(!composing_guard_swallows(
            &Key::Named(NamedKey::Backspace),
            ime_composing,
            ime_preedit
        ));
        assert!(ime_composing.get_untracked());
    }

    #[test]
    fn composing_guard_self_heals_a_stuck_flag_with_no_preedit_text() {
        // `ime_composing` claiming an active composition with no preedit
        // text behind it is exactly the stuck state this guard is meant to
        // never produce (see `app::input::AppInput`'s Ime handlers, which
        // always clear both together) -- but if it ever happens anyway,
        // the very next KeyDown/KeyUp must self-heal instead of eating
        // input forever.
        let (ime_composing, ime_preedit) = composing(None);

        assert!(!composing_guard_swallows(
            &Key::Character("a".into()),
            ime_composing,
            ime_preedit
        ));
        assert!(!ime_composing.get_untracked(), "the stuck flag must clear");
    }

    #[test]
    fn composing_guard_is_inactive_when_not_composing() {
        let ime_composing = RwSignal::new(false);
        let ime_preedit = RwSignal::new(None::<String>);

        assert!(!composing_guard_swallows(
            &Key::Character("a".into()),
            ime_composing,
            ime_preedit
        ));
    }

    // --- approval banner key routing (`banner_key_action`) ----------------

    #[test]
    fn banner_key_y_approves_case_insensitively() {
        assert_eq!(
            banner_key_action(&Key::Character("y".into()), Modifiers::default()),
            BannerKeyAction::Approve
        );
        assert_eq!(
            banner_key_action(&Key::Character("Y".into()), Modifiers::SHIFT),
            BannerKeyAction::Approve
        );
    }

    #[test]
    fn banner_key_n_denies_case_insensitively() {
        assert_eq!(
            banner_key_action(&Key::Character("n".into()), Modifiers::default()),
            BannerKeyAction::Deny
        );
        assert_eq!(
            banner_key_action(&Key::Character("N".into()), Modifiers::SHIFT),
            BannerKeyAction::Deny
        );
    }

    #[test]
    fn banner_key_escape_releases_focus_without_answering() {
        assert_eq!(
            banner_key_action(&Key::Named(NamedKey::Escape), Modifiers::default()),
            BannerKeyAction::ReleaseFocus
        );
    }

    #[test]
    fn banner_key_other_printable_chars_redirect_to_the_message_box() {
        assert_eq!(
            banner_key_action(&Key::Character("h".into()), Modifiers::default()),
            BannerKeyAction::Redirect("h".to_string())
        );
        assert_eq!(
            banner_key_action(&Key::Named(NamedKey::Space), Modifiers::default()),
            BannerKeyAction::Redirect(" ".to_string())
        );
    }

    #[test]
    fn banner_key_swallows_keys_bound_to_nothing() {
        assert_eq!(
            banner_key_action(&Key::Named(NamedKey::Enter), Modifiers::default()),
            BannerKeyAction::Swallow
        );
        assert_eq!(
            banner_key_action(&Key::Named(NamedKey::Backspace), Modifiers::default()),
            BannerKeyAction::Swallow
        );
        assert_eq!(
            banner_key_action(&Key::Named(NamedKey::ArrowLeft), Modifiers::default()),
            BannerKeyAction::Swallow
        );
    }

    #[test]
    fn banner_key_swallows_modifier_held_chords_instead_of_redirecting() {
        // A held Ctrl/Alt/Meta chord isn't ordinary typing -- and per the
        // banner's "must not leak to the terminal/message box" rule it still
        // must not fall through, so it's swallowed rather than redirected.
        assert_eq!(
            banner_key_action(&Key::Character("y".into()), Modifiers::CONTROL),
            BannerKeyAction::Swallow
        );
        assert_eq!(
            banner_key_action(&Key::Character("h".into()), Modifiers::ALT),
            BannerKeyAction::Swallow
        );
    }
}
