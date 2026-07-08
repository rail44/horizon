use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::agent::contract::Command;
use crate::app::keymap::{
    agent_draft_action, is_terminal_copy_key, is_terminal_paste_key, next_grapheme_boundary_approx,
    prev_grapheme_boundary_approx, terminal_input_from_key, terminal_key_event_kind,
    terminal_key_from_key, termwiz_modifiers, AgentDraftAction,
};
use crate::session::Registry;
use crate::terminal::{KeyEventKind, TerminalCommand};
use crate::workspace::{PaneId, PaneKind, Workspace};
use floem::action::set_ime_allowed;
use floem::keyboard::{Key, KeyEvent, Modifiers, NamedKey};
use floem::prelude::*;
use floem::Clipboard;

/// A dynamically-sized, `PaneId`-keyed table of per-pane UI signals --
/// replaces the old fixed `[RwSignal<T>; MAX_VISIBLE_PANES]` arrays
/// (`docs/recursive-layout-design.md`'s slice 2 de-caps the pane count).
/// Entries are created lazily the first time a pane's view actually needs
/// one (`register`, called from `workspace::view::pane`'s
/// `pane_view`) and pruned once the pane no longer exists anywhere in the
/// workspace (`retain`, driven reactively by `workspace::view::
/// workspace_view`'s cleanup effect) -- so a pane that's never rendered
/// (e.g. one only ever touched through the CLI control plane) never
/// allocates an entry, and a closed pane's entry doesn't outlive it.
#[derive(Clone)]
pub(crate) struct PaneKeyedSignals<T: 'static> {
    inner: Rc<RefCell<HashMap<PaneId, RwSignal<T>>>>,
}

impl<T: Default + 'static> PaneKeyedSignals<T> {
    pub(crate) fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    /// Registers a fresh signal (seeded with `T::default()`) for `pane_id`,
    /// replacing any existing entry -- called once per `pane_view`
    /// construction, never from inside a reactive closure that might re-run
    /// for an already-live view. This *must* overwrite rather than reuse an
    /// existing entry: `workspace::view::layout_tree`'s leaf/split
    /// `dyn_container` (e.g. a tab's very first split, which wraps its
    /// sole existing pane) disposes and rebuilds that pane's own
    /// `pane_view` in place, which disposes every signal created while
    /// that view was mounted -- reusing the stale entry here would hand
    /// back a signal from a disposed reactive scope, panicking on the next
    /// read. Minting a new one instead keeps this always valid, at the
    /// cost of losing that one pane's draft/focus-request state across
    /// exactly that transition (acceptable -- see the design's slice 2
    /// report).
    pub(crate) fn register(&self, pane_id: PaneId) -> RwSignal<T> {
        let signal = RwSignal::new(T::default());
        self.inner.borrow_mut().insert(pane_id, signal);
        signal
    }

    /// The signal for `pane_id`, if one has already been created -- for
    /// call sites (focus-follow, the active agent draft) that must act on
    /// an already-rendered pane and never conjure a signal into existence
    /// for one that isn't.
    pub(crate) fn get(&self, pane_id: PaneId) -> Option<RwSignal<T>> {
        self.inner.borrow().get(&pane_id).copied()
    }

    /// Drops every entry whose pane isn't in `live` -- the counterpart to
    /// `register`'s lazy allocation.
    pub(crate) fn retain(&self, live: &HashSet<PaneId>) {
        self.inner.borrow_mut().retain(|id, _| live.contains(id));
    }
}

/// A composer's in-progress message plus its cursor position -- `cursor` is
/// a byte offset into `text`, always kept on a UTF-8 char boundary by every
/// mutation path (`apply_agent_draft_action`, `insert_agent_draft_text`).
/// Backs `agent_controls::agent_composer`'s cursor rendering and the
/// arrow-key/insert/backspace editing below; before this, drafts were plain
/// `String`s with an implicit cursor that was always at the end (append-only
/// typing, pop-from-the-end Backspace), which is what made the cursor
/// neither visible nor movable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AgentDraft {
    pub(crate) text: String,
    pub(crate) cursor: usize,
}

pub(crate) type AgentDrafts = PaneKeyedSignals<AgentDraft>;
pub(crate) type PaneFocusRequests = PaneKeyedSignals<u64>;

pub(crate) fn active_agent(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_is(PaneKind::Agent))
}

pub(crate) fn active_text_input_pane(workspace: RwSignal<Workspace>) -> bool {
    workspace.with(|ws| ws.active_pane_accepts_text_input())
}

fn active_pane_id(workspace: RwSignal<Workspace>) -> Option<PaneId> {
    workspace.with_untracked(|ws| ws.active_tab().map(|tab| tab.active))
}

pub(crate) fn request_active_pane_focus(
    workspace: RwSignal<Workspace>,
    pane_focus_requests: PaneFocusRequests,
) {
    if let Some(pane_id) = active_pane_id(workspace) {
        if let Some(focus_request) = pane_focus_requests.get(pane_id) {
            focus_request.update(|request| *request += 1);
        }
    }
    set_ime_allowed(active_text_input_pane(workspace));
}

pub(crate) fn active_agent_draft(
    workspace: RwSignal<Workspace>,
    agent_drafts: AgentDrafts,
) -> Option<RwSignal<AgentDraft>> {
    if !active_agent(workspace) {
        return None;
    }

    agent_drafts.get(active_pane_id(workspace)?)
}

/// Inserts `text` at the current cursor and advances the cursor past it --
/// shared by ordinary character insertion/paste (`handle_agent_key` below),
/// IME commit (`app::input::AppInput::handle_ime_commit`), and the approval
/// row's soft-redirect path (`workspace::view::pane`'s
/// `ApprovalKeyAction::Redirect`), so every write path keeps the cursor
/// in sync with the text it just inserted instead of always appending at
/// the tail regardless of where the cursor actually is.
pub(crate) fn insert_agent_draft_text(draft: RwSignal<AgentDraft>, text: &str) {
    draft.update(|draft| {
        draft.text.insert_str(draft.cursor, text);
        draft.cursor += text.len();
    });
}

pub(crate) fn active_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.active_terminal_session_id())?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub(crate) fn pane_terminal_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    pane_id: PaneId,
) -> Option<crossbeam_channel::Sender<TerminalCommand>> {
    let session_id = workspace.with_untracked(|ws| ws.terminal_session_id(pane_id))?;
    sessions.with_untracked(|registry| registry.terminal_sender(session_id))
}

pub(crate) fn pane_agent_sender(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    pane_id: PaneId,
) -> Option<crossbeam_channel::Sender<Command>> {
    let session_id = workspace.with_untracked(|ws| ws.agent_session_id(pane_id))?;
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
    draft: RwSignal<AgentDraft>,
    agent_tx: Option<crossbeam_channel::Sender<Command>>,
) -> bool {
    if is_terminal_paste_key(key_event) {
        if let Ok(text) = Clipboard::get_contents() {
            insert_agent_draft_text(draft, &text);
            return true;
        }
    }

    let Some(action) = agent_draft_action(&key_event.key.logical_key, key_event.modifiers) else {
        return false;
    };
    apply_agent_draft_action(action, draft, agent_tx);
    true
}

/// Applies one editing action to `draft` -- pulled out of `handle_agent_key`
/// so it's directly testable without constructing a floem `KeyEvent` (which
/// wraps a private winit type -- see `app::keymap::Chord::matches`'s doc
/// comment for why the rest of this file's tests stick to raw `Key`/
/// `Modifiers` instead).
fn apply_agent_draft_action(
    action: AgentDraftAction,
    draft: RwSignal<AgentDraft>,
    agent_tx: Option<crossbeam_channel::Sender<Command>>,
) {
    match action {
        AgentDraftAction::Insert(text) => insert_agent_draft_text(draft, &text),
        AgentDraftAction::Backspace => draft.update(|draft| {
            let boundary = prev_grapheme_boundary_approx(&draft.text, draft.cursor);
            draft.text.replace_range(boundary..draft.cursor, "");
            draft.cursor = boundary;
        }),
        AgentDraftAction::MoveLeft => draft.update(|draft| {
            draft.cursor = prev_grapheme_boundary_approx(&draft.text, draft.cursor);
        }),
        AgentDraftAction::MoveRight => draft.update(|draft| {
            draft.cursor = next_grapheme_boundary_approx(&draft.text, draft.cursor);
        }),
        AgentDraftAction::Submit => {
            let text = draft.with_untracked(|draft| draft.text.trim().to_string());
            if text.is_empty() {
                return;
            }
            if let Some(tx) = agent_tx {
                let _ = tx.send(Command::UserMessage { text });
                draft.set(AgentDraft::default());
            }
        }
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
pub(super) fn composing_guard_swallows(
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

/// An approval-focused agent pane's response to one keystroke -- see
/// `agent::view::AgentPaneFocus` and `pane_view`'s `KeyDown` handler, which
/// calls `handle_agent_approval_key` before falling through to
/// `handle_active_pane_key` whenever the pane's inline approval control row
/// (`docs/agent-output-ui-design.md` decision 8) currently holds
/// pane-internal focus. Named for the pre-slice-4 approval banner this
/// key routing was originally built for; the routing itself is unchanged by
/// the banner's move inline.
///
/// Mirrors the crush-inspired (charmbracelet's TUI) design: `y` approves,
/// `n` denies, `Esc` backs out to the message box without answering, and any
/// other printable character is delivered to the message box instead of
/// being swallowed -- the approval row must never be a modal trap for
/// ordinary typing. Everything else (Enter, Backspace, arrows, a held
/// modifier chord, ...) is swallowed outright: while the row holds focus,
/// keys must not leak to the terminal/message box except through that one
/// soft-redirect path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalKeyAction {
    Approve,
    Deny,
    /// Leave the approval row without answering: pane-internal focus moves
    /// back to the message box, the pending call is left untouched.
    ReleaseFocus,
    /// Deliver `.0` to the message box as if it had been typed there, and
    /// move focus there too.
    Redirect(String),
    /// Consumed with no effect.
    Swallow,
}

fn approval_key_action(key: &Key, modifiers: Modifiers) -> ApprovalKeyAction {
    if modifiers.control() || modifiers.alt() || modifiers.meta() {
        return ApprovalKeyAction::Swallow;
    }

    match key {
        Key::Named(NamedKey::Escape) => ApprovalKeyAction::ReleaseFocus,
        Key::Named(NamedKey::Space) => ApprovalKeyAction::Redirect(" ".to_string()),
        Key::Character(text) => match text.as_str() {
            "y" | "Y" => ApprovalKeyAction::Approve,
            "n" | "N" => ApprovalKeyAction::Deny,
            _ => ApprovalKeyAction::Redirect(text.to_string()),
        },
        _ => ApprovalKeyAction::Swallow,
    }
}

/// `Event::KeyDown` entry point for an approval-focused agent pane. Applies
/// the same IME composing guard as the message box/terminal
/// (`composing_guard_swallows`) before considering the key at all, so a
/// composing IME's own keystrokes never reach `y`/`n`/`Esc` handling
/// half-composed.
pub(crate) fn handle_agent_approval_key(
    key_event: &KeyEvent,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
) -> ApprovalKeyAction {
    if composing_guard_swallows(&key_event.key.logical_key, ime_composing, ime_preedit) {
        return ApprovalKeyAction::Swallow;
    }
    approval_key_action(&key_event.key.logical_key, key_event.modifiers)
}

pub(crate) fn handle_active_pane_key(
    key_event: &KeyEvent,
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    pane_id: PaneId,
    ime_composing: RwSignal<bool>,
    ime_preedit: RwSignal<Option<String>>,
    agent_draft: RwSignal<AgentDraft>,
) -> bool {
    if composing_guard_swallows(&key_event.key.logical_key, ime_composing, ime_preedit) {
        return true;
    }

    if workspace.with(|ws| ws.is_active_pane_of_kind(pane_id, PaneKind::Agent)) {
        return handle_agent_key(
            key_event,
            agent_draft,
            pane_agent_sender(workspace, sessions, pane_id),
        );
    }

    if workspace.with(|ws| ws.is_active_pane_of_kind(pane_id, PaneKind::Terminal)) {
        return handle_terminal_key(
            key_event,
            pane_terminal_sender(workspace, sessions, pane_id),
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
    pane_id: PaneId,
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

    if !workspace.with(|ws| ws.is_active_pane_of_kind(pane_id, PaneKind::Terminal)) {
        return false;
    }

    handle_terminal_key_release(
        key_event,
        pane_terminal_sender(workspace, sessions, pane_id),
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

    // --- approval key routing (`approval_key_action`) ---------------------

    #[test]
    fn approval_key_y_approves_case_insensitively() {
        assert_eq!(
            approval_key_action(&Key::Character("y".into()), Modifiers::default()),
            ApprovalKeyAction::Approve
        );
        assert_eq!(
            approval_key_action(&Key::Character("Y".into()), Modifiers::SHIFT),
            ApprovalKeyAction::Approve
        );
    }

    #[test]
    fn approval_key_n_denies_case_insensitively() {
        assert_eq!(
            approval_key_action(&Key::Character("n".into()), Modifiers::default()),
            ApprovalKeyAction::Deny
        );
        assert_eq!(
            approval_key_action(&Key::Character("N".into()), Modifiers::SHIFT),
            ApprovalKeyAction::Deny
        );
    }

    #[test]
    fn approval_key_escape_releases_focus_without_answering() {
        assert_eq!(
            approval_key_action(&Key::Named(NamedKey::Escape), Modifiers::default()),
            ApprovalKeyAction::ReleaseFocus
        );
    }

    #[test]
    fn approval_key_other_printable_chars_redirect_to_the_message_box() {
        assert_eq!(
            approval_key_action(&Key::Character("h".into()), Modifiers::default()),
            ApprovalKeyAction::Redirect("h".to_string())
        );
        assert_eq!(
            approval_key_action(&Key::Named(NamedKey::Space), Modifiers::default()),
            ApprovalKeyAction::Redirect(" ".to_string())
        );
    }

    #[test]
    fn approval_key_swallows_keys_bound_to_nothing() {
        assert_eq!(
            approval_key_action(&Key::Named(NamedKey::Enter), Modifiers::default()),
            ApprovalKeyAction::Swallow
        );
        assert_eq!(
            approval_key_action(&Key::Named(NamedKey::Backspace), Modifiers::default()),
            ApprovalKeyAction::Swallow
        );
        assert_eq!(
            approval_key_action(&Key::Named(NamedKey::ArrowLeft), Modifiers::default()),
            ApprovalKeyAction::Swallow
        );
    }

    #[test]
    fn approval_key_swallows_modifier_held_chords_instead_of_redirecting() {
        // A held Ctrl/Alt/Meta chord isn't ordinary typing -- and per the
        // row's "must not leak to the terminal/message box" rule it still
        // must not fall through, so it's swallowed rather than redirected.
        assert_eq!(
            approval_key_action(&Key::Character("y".into()), Modifiers::CONTROL),
            ApprovalKeyAction::Swallow
        );
        assert_eq!(
            approval_key_action(&Key::Character("h".into()), Modifiers::ALT),
            ApprovalKeyAction::Swallow
        );
    }

    // --- agent draft editing (`apply_agent_draft_action`) -----------------
    //
    // Exercises the action-application core directly rather than through
    // `handle_agent_key`'s full `&KeyEvent`, which can't be constructed in a
    // test (see `apply_agent_draft_action`'s doc comment).

    fn apply_all(draft: RwSignal<AgentDraft>, actions: impl IntoIterator<Item = AgentDraftAction>) {
        for action in actions {
            apply_agent_draft_action(action, draft, None);
        }
    }

    #[test]
    fn cursor_left_then_insert_lands_mid_string_not_at_the_tail() {
        // The regression this whole fix is for: typing "abc", moving the
        // cursor left twice, then typing "X" must insert *before* "bc"
        // ("aXbc"), not append at the end ("abcX") the way the old
        // always-push_str editing did.
        let draft = RwSignal::new(AgentDraft::default());
        apply_all(
            draft,
            [
                AgentDraftAction::Insert("a".to_string()),
                AgentDraftAction::Insert("b".to_string()),
                AgentDraftAction::Insert("c".to_string()),
                AgentDraftAction::MoveLeft,
                AgentDraftAction::MoveLeft,
                AgentDraftAction::Insert("X".to_string()),
            ],
        );

        draft.with_untracked(|draft| {
            assert_eq!(draft.text, "aXbc");
            assert_eq!(draft.cursor, 2);
        });
    }

    #[test]
    fn backspace_removes_the_grapheme_before_the_cursor_not_the_tail() {
        let draft = RwSignal::new(AgentDraft::default());
        apply_all(
            draft,
            [
                AgentDraftAction::Insert("abc".to_string()),
                AgentDraftAction::MoveLeft,
                AgentDraftAction::Backspace,
            ],
        );

        draft.with_untracked(|draft| {
            assert_eq!(draft.text, "ac");
            assert_eq!(draft.cursor, 1);
        });
    }

    #[test]
    fn move_right_past_the_end_of_text_is_a_no_op() {
        let draft = RwSignal::new(AgentDraft::default());
        apply_all(
            draft,
            [
                AgentDraftAction::Insert("ab".to_string()),
                AgentDraftAction::MoveRight,
                AgentDraftAction::MoveRight,
            ],
        );

        draft.with_untracked(|draft| {
            assert_eq!(draft.text, "ab");
            assert_eq!(draft.cursor, 2);
        });
    }

    #[test]
    fn cursor_editing_handles_multibyte_japanese_text() {
        // Test data as multibyte/IME-relevant Japanese, per this repo's own
        // convention for exercising char-boundary correctness.
        let draft = RwSignal::new(AgentDraft::default());
        apply_all(
            draft,
            [
                AgentDraftAction::Insert("こんにちは".to_string()),
                AgentDraftAction::MoveLeft,
                AgentDraftAction::MoveLeft,
                AgentDraftAction::Insert("!".to_string()),
            ],
        );

        draft.with_untracked(|draft| {
            assert_eq!(draft.text, "こんに!ちは");
        });
    }

    #[test]
    fn submit_sends_trimmed_text_and_resets_cursor() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let draft = RwSignal::new(AgentDraft::default());
        apply_agent_draft_action(AgentDraftAction::Insert("  hi  ".to_string()), draft, None);
        apply_agent_draft_action(AgentDraftAction::Submit, draft, Some(tx));

        draft.with_untracked(|draft| {
            assert_eq!(draft.text, "");
            assert_eq!(draft.cursor, 0);
        });
        assert_eq!(
            rx.try_recv(),
            Ok(Command::UserMessage {
                text: "hi".to_string()
            })
        );
    }

    #[test]
    fn submit_of_blank_text_does_not_send_or_clear() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let draft = RwSignal::new(AgentDraft::default());
        apply_agent_draft_action(AgentDraftAction::Insert("   ".to_string()), draft, None);
        apply_agent_draft_action(AgentDraftAction::Submit, draft, Some(tx));

        draft.with_untracked(|draft| assert_eq!(draft.text, "   "));
        assert!(rx.try_recv().is_err());
    }
}
