//! The GPUI projection of the shared workspace model
//! (`crates/horizon-workspace`): tab strip, recursive split rendering on
//! gpui-component's resizable primitives, pane focus, and workspace mode
//! with spatial navigation. The model owns all layout truth; this module
//! only renders it and translates GPUI actions into model operations.
//!
//! The key bindings registered in [`init`] are M2 stand-ins wired
//! straight to model calls — M3 replaces them with the command model
//! (`CommandId` + keymap config), at which point every handler here
//! becomes a binding to a command instead. [`init`]'s `[keybindings]`
//! layer (parsed via `keymap::gpui_keystroke`/`keymap::command_for`) is
//! the first piece of that: config-bound chords dispatch through
//! [`RunCommand`] to [`WorkspaceShell::execute`] instead of a
//! model-call handler.

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::{List, ListDelegate, ListEvent, ListState};
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizablePanelGroup};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::IndexPath;
use horizon_workspace::commands::{command_entries, CommandId, CommandState};
use horizon_workspace::types::{LayoutNode, TabId};
use horizon_workspace::{
    Direction, PaneId, PaneKind, SessionInventory, SplitAxis, ViewKind, Workspace,
    WORKSPACE_STATE_VERSION,
};

use crate::agent::{AgentSession, AgentView};
use crate::keymap;
use crate::palette::PaletteDelegate;
use crate::session_manager::SessionManagerDelegate;
use crate::sessiond::{wait_for_drain, SessiondHandle, SessiondResponder};
use crate::terminal::{TerminalSession, TerminalView};
use crate::terminal_focus::focus_transition;
use crate::theme;
use crate::theme_settings::ThemeSettingsView;
use crate::view_chooser::{Placement, ViewChooserDelegate};
use crate::workspace_state::{InvalidState, LoadResult, WorkspaceStateStore};
use horizon_terminal_core::{TerminalCoreOptions, TerminalSize, TerminalSpawnSpec};
use horizon_workspace::types::SessionKind;
use horizon_workspace::SessionId;
use uuid::Uuid;

type AgentSessionId = horizon_agent::contract::SessionId;

fn agent_session_id(id: SessionId) -> AgentSessionId {
    AgentSessionId::from_uuid(id.as_uuid())
}

#[derive(Clone)]
struct PendingTerminalSpawn {
    source_session_id: Option<SessionId>,
    fallback_cwd: std::path::PathBuf,
}

fn prepare_workspace_for_runtime_reload(workspace: &mut Workspace) {
    let terminals = workspace
        .session_summaries()
        .into_iter()
        .filter(|summary| summary.kind == SessionKind::Terminal)
        .map(|summary| summary.id)
        .collect::<Vec<_>>();
    for session_id in terminals {
        workspace.terminate_session(session_id);
    }
}

fn ensure_workspace_has_pane(workspace: &mut Workspace) -> Option<SessionId> {
    (workspace.tab_count() == 0)
        .then(|| workspace.open_tab_with_new_session_activated(PaneKind::Terminal, true))
}

fn command_blocked_by_restore(restoring: bool, failed: bool, id: CommandId) -> bool {
    restoring && !(failed && id == CommandId::ReloadSessionRuntime)
}

/// The first row is selectable exactly when the list isn't empty — the
/// pure predicate behind [`select_first_row_on_open`], kept free of
/// `ListState`/`App` so it's unit-testable without a GPUI window.
fn first_row_to_select(items_count: usize) -> Option<IndexPath> {
    (items_count > 0).then(IndexPath::default)
}

/// Selects the first row right after a searchable `List` is constructed,
/// so a bare Enter on open runs it without arrowing down first
/// (owner report, 2026-07-13). gpui-component's `ListState` starts with
/// no selection and only re-selects a candidate in response to a query
/// change (its own `on_query_input_event`), never on construction — so
/// every palette/session-manager/view-chooser open required an arrow key
/// before Enter did anything. A no-op when the delegate starts empty:
/// `ListState::on_action_confirm` already guards Enter on an empty list.
fn select_first_row_on_open<D: ListDelegate>(
    list: &mut ListState<D>,
    window: &mut Window,
    cx: &mut Context<ListState<D>>,
) {
    if let Some(ix) = first_row_to_select(list.delegate().items_count(0, cx)) {
        list.set_selected_index(Some(ix), window, cx);
    }
}

fn workspace_mode_blocked_by_restore(restoring: bool, failed: bool) -> bool {
    restoring && !failed
}

fn terminal_spawn_source(
    explicit_source: Option<SessionId>,
    active_session: Option<SessionId>,
) -> Option<SessionId> {
    explicit_source.or(active_session)
}

fn terminal_fallback_cwd(
    current_dir: Option<std::path::PathBuf>,
    home: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    current_dir
        .or(home)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn terminal_resume_candidates(
    summaries: Vec<horizon_terminal_core::TerminalSummary>,
    known: &std::collections::HashSet<SessionId>,
) -> Vec<Uuid> {
    let mut seen = std::collections::HashSet::new();
    summaries
        .into_iter()
        .filter_map(|summary| {
            let id = SessionId::from_uuid(summary.session_id);
            (!known.contains(&id) && seen.insert(id)).then_some(summary.session_id)
        })
        .collect()
}

fn load_workspace_state(store: &mut WorkspaceStateStore) -> (Workspace, bool, bool) {
    match store.load(u64::from(WORKSPACE_STATE_VERSION)) {
        Ok(LoadResult::Valid(json)) => match Workspace::from_persisted_json(&json) {
            Ok(workspace) => (workspace, true, false),
            Err(horizon_workspace::WorkspaceStateError::UnsupportedVersion {
                found,
                supported,
            }) => {
                eprintln!(
                    "workspace state version {found} is unsupported (expected {supported}); preserving the file"
                );
                (Workspace::mvp(), false, false)
            }
            Err(error) => {
                eprintln!("ignoring invalid workspace state: {error}");
                (Workspace::mvp(), false, true)
            }
        },
        Ok(LoadResult::Missing) => (Workspace::mvp(), false, true),
        Ok(LoadResult::Invalid(InvalidState::UnsupportedVersion { found, supported })) => {
            eprintln!(
                "workspace state version {found} is unsupported (expected {supported}); preserving the file"
            );
            (Workspace::mvp(), false, false)
        }
        Ok(LoadResult::Invalid(InvalidState::Corrupt(error))) => {
            eprintln!("ignoring corrupt workspace state: {error}");
            (Workspace::mvp(), false, true)
        }
        Err(error) => {
            eprintln!("failed to load workspace state: {error}");
            (Workspace::mvp(), false, false)
        }
    }
}

/// One pane's view, by session kind -- plus one variant per first-party
/// [`ViewKind`] (`docs/theme-settings-view-design.md`), which has no
/// session at all.
#[derive(Clone)]
enum PaneView {
    Terminal(Entity<TerminalView>),
    Agent(Entity<AgentView>),
    ThemeSettings(Entity<ThemeSettingsView>),
}

impl PaneView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneView::Terminal(view) => view.focus_handle(cx),
            PaneView::Agent(view) => view.focus_handle(cx),
            PaneView::ThemeSettings(view) => view.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            PaneView::Terminal(view) => view.clone().into_any_element(),
            PaneView::Agent(view) => view.clone().into_any_element(),
            PaneView::ThemeSettings(view) => view.clone().into_any_element(),
        }
    }
}

actions!(
    workspace,
    [
        ToggleWorkspaceMode,
        ModeMoveLeft,
        ModeMoveDown,
        ModeMoveUp,
        ModeMoveRight,
        ModeCommit,
        ModeCancel,
        NewTab,
        NewAgentTab,
        SplitPane,
        ClosePane,
        NextTab,
        OpenPalette
    ]
);

/// A `[keybindings]`-config-driven binding to a `CommandId` — gpui actions
/// used with `KeyBinding` are compile-time types, so a config chord can't
/// bind directly to one of the many `CommandId` variants the way a unit
/// action binds to one fixed handler. `RunCommand` carries the resolved
/// id as data instead, so a single action type covers every simple
/// command a `[keybindings]` entry can name (see `keymap::command_for`).
/// `no_json`: never built from a JSON keymap (only ever constructed
/// directly in [`init`]), so it skips gpui's `Deserialize`/`JsonSchema`
/// requirements for action fields.
///
/// `pub(crate)` (stage F, `docs/agent-output-ui-amendment.md`): the agent
/// pane's stop button/status-line stop affordance dispatch this same
/// action from `src/agent/view.rs`, rather than reaching into
/// `AgentSession::cancel` directly, so a pointer-driven cancel goes
/// through the same command-model path as the palette and `[keybindings]`
/// chords (AGENTS.md's "operations go through the command model").
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub(crate) struct RunCommand {
    pub(crate) id: CommandId,
}

const MODE_CONTEXT: &str = "WorkspaceMode";

/// Alpha applied to `theme::scrim_color()` for every pane while workspace
/// mode's dim pattern is active -- ported from the retired Floem shell's
/// `WORKSPACE_MODE_DIM_ALPHA` (`docs/workspace-mode-design.md`'s "pane
/// dimming" visualization signal, the accident-killer: a stray keystroke
/// should read as "nothing here types normally right now" at a glance). A
/// numeric opacity factor, not a color, so the color itself stays
/// theme-driven.
///
/// Lowered from the original 0.55 on 2026-07-15 dogfooding feedback
/// (round 1): 0.55 was carried over unchanged from the old bg-colored
/// veil, but a *polarity-flipped pole* scrim color (tried this round,
/// see below) reads noticeably heavier than the veil did at the same
/// alpha.
///
/// Round 2 (also 2026-07-15) gave the cursor pane its own lighter alpha
/// (`SCRIM_CURSOR_DIM_ALPHA = 0.12`) instead of the base dim. Round 3
/// (same day) dropped that distinction entirely: the cursor signal now
/// lives solely in the pane border (see `render_node`'s now-more-prominent
/// `border_2()` and [`pane_border_role`]), so every pane -- cursor
/// included -- gets the *same* alpha whenever the dim pattern is active,
/// and the alpha itself dropped to 0.10 (light, since the scrim no
/// longer needed to carry any cursor/non-cursor contrast on its own).
///
/// Round 4 (2026-07-16) reset the alpha's *meaning*: the owner tried the
/// polarity-flipped pole color from rounds 1-3 in real use and withdrew
/// it (see `theme::scrim_color`'s doc comment) in favor of the original
/// pre-2026-07-15 approach -- compositing the *background* color over a
/// pane, which reduces its contrast rather than shifting its lightness.
/// 0.10 was calibrated for the (much higher-contrast) black/white pole
/// color and would be nearly invisible against a same-hued background
/// veil, so the alpha jumped back up to 0.5 -- the historical veil lived
/// at 0.55, kept as the reference starting point. Still every pane
/// uniformly, cursor included; still explicitly feel-tunable, not
/// derived.
const SCRIM_DIM_ALPHA: f32 = 0.5;

/// Whether workspace mode's dim pattern applies a scrim, given whether the
/// pattern is active. Trivial since round 3 (2026-07-15): every pane dims
/// uniformly at [`SCRIM_DIM_ALPHA`] when the pattern is active, cursor
/// pane included -- see [`SCRIM_DIM_ALPHA`]'s doc comment. Kept as a
/// small named, tested seam rather than inlined at the one call site, so
/// "is a scrim applied at all" stays independently documented and
/// testable if the composition ever grows again.
///
/// `mode_active` describes the dim *pattern*, not necessarily workspace
/// mode's own live state at render time: while a control-surface modal
/// (palette / view chooser / session manager) is open, the caller passes
/// the pattern frozen at modal-open time (see [`effective_scrim_pattern`])
/// instead of the live `is_workspace_mode_active()`.
fn pane_scrim_alpha(mode_active: bool) -> Option<f32> {
    mode_active.then_some(SCRIM_DIM_ALPHA)
}

/// The workspace-mode dim/cursor pattern currently in effect, for both the
/// scrim and the cursor-pane border: the live pattern normally, or --
/// while a control-surface modal is open -- the pattern frozen at the
/// moment the modal opened (`scrim_freeze`) instead. Pure and
/// unit-tested so the freeze substitution itself is covered without a
/// GPUI render.
///
/// Freezing is necessary because every modal-opening handler
/// (`open_palette`/`open_view_chooser`/`open_session_manager`) calls
/// `Workspace::exit_workspace_mode` immediately -- so the mode's own
/// hjkl/Enter/Escape key bindings don't hijack the modal's typed
/// search/confirm keys -- which would otherwise erase
/// `is_workspace_mode_active()`/`cursor_pane_id()`'s live values the
/// instant the modal renders. Substituting the frozen pattern instead
/// keeps opening a modal fully chrome-neutral: neither the scrim nor the
/// cursor-pane accent border changes when it opens (2026-07-15 round-3
/// feedback, `docs/theme-design.md`'s scrim section) -- direct `ctrl+p`
/// from outside workspace mode freezes "inactive", so nothing dims and no
/// pane gets the cursor border.
fn effective_scrim_pattern(
    modal_open: bool,
    scrim_freeze: Option<PaneId>,
    live_mode_active: bool,
    live_cursor_pane: Option<PaneId>,
) -> (bool, Option<PaneId>) {
    if modal_open {
        (scrim_freeze.is_some(), scrim_freeze)
    } else {
        (live_mode_active, live_cursor_pane)
    }
}

/// Which color role a pane's border resolves to. Pure and unit-tested so
/// the state selection is covered without a GPUI render; `render_node`
/// maps the resolved role onto the actual `theme::` color and paints it
/// at a uniform 2px (`border_2()`) for every role -- widened from 1px
/// (2026-07-15 round-3 feedback: the 1px accent border was nearly
/// invisible against the panes). Width stays identical across roles
/// deliberately, so a pane's layout never shifts as the cursor moves;
/// only the color changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneBorderRole {
    /// The workspace-mode cursor pane (or its frozen equivalent while a
    /// modal is open, see [`effective_scrim_pattern`]) -- full-strength
    /// `theme::accent()`, deliberately left at full saturation (not
    /// subdued/blended) so it stays the clearly most prominent of the
    /// three roles even at the same 2px width.
    Cursor,
    /// The tab's focused pane, when it isn't also the cursor pane --
    /// `theme::border()`, an intentionally subdued separator color.
    Active,
    /// Neither -- `theme::background()`, i.e. no visible border at all.
    Inactive,
}

fn pane_border_role(is_cursor: bool, is_active: bool) -> PaneBorderRole {
    if is_cursor {
        PaneBorderRole::Cursor
    } else if is_active {
        PaneBorderRole::Active
    } else {
        PaneBorderRole::Inactive
    }
}

/// Blur radius for the cursor pane's inner-glow inset shadow (round 5,
/// 2026-07-16 owner ask: "can a blur-like effect be applied inside the
/// border"). Layered with, not a replacement for, the existing 2px accent
/// border (`PaneBorderRole::Cursor`) -- a soft accent-colored wash just
/// inside the border, reinforcing the same cursor signal. `inset: true`
/// `BoxShadow`s are genuinely honored by gpui's renderer (verified against
/// the pinned rev, not assumed -- see `render_node`'s call site comment
/// for the trace), and shadows never participate in layout (`box_shadow`
/// is read only from `Style::paint`, never from any layout/taffy
/// conversion -- crates/gpui/src/style.rs in the pinned checkout), so this
/// cannot shift a pane's size the way widening the border itself would
/// have. Feel-tunable, not derived. Lowered from 8.0 on round-6
/// (2026-07-16) owner feedback -- the glow felt too soft/wide at 8.
const CURSOR_GLOW_BLUR_PX: f32 = 4.0;

/// Alpha for the cursor pane's inner-glow inset shadow, applied to
/// `theme::accent()`. Feel-tunable, same as [`CURSOR_GLOW_BLUR_PX`].
const CURSOR_GLOW_ALPHA: f32 = 0.35;

/// How far `render_node`'s `LayoutNode::Split` branch pulls each pane's
/// border back from a split boundary it's adjacent to (round 5,
/// 2026-07-16 owner report: pane borders relate to the split boundary
/// asymmetrically -- one side's border reads as overlapping the
/// boundary, the other as sitting cleanly inside it).
///
/// Root cause, traced into gpui-component's vendored `resizable` module
/// (pinned rev, `crates/ui/src/resizable/{panel,resize_handle}.rs`): the
/// draggable divider between two adjacent panes is *not* a shared,
/// neutral element both panes contribute to. `ResizablePanel::render`
/// (`panel.rs`) attaches a `resize_handle` only as a child of "each panel
/// after the first" (the file's own doc comment) -- i.e. solely owned by
/// the *trailing* pane of a pair. That handle is absolutely positioned at
/// `left`/`top: -HANDLE_PADDING` (`resize_handle.rs`, `HANDLE_PADDING =
/// 4px`) relative to the trailing pane's own origin, reaching *backward*
/// into the leading pane -- and deliberately left unclipped
/// (`ResizablePanel`'s own doc comment forbids `.overflow_hidden()` on
/// the panel specifically because it would clip this handle). Since
/// flex siblings paint in child order, the trailing pane's whole subtree
/// (its own content *and* this handle) paints *after* the leading pane's,
/// so the handle -- reaching back into the leading pane's screen region
/// -- paints over part of the leading pane's border from *outside* the
/// leading pane's own render tree, while it paints over part of the
/// trailing pane's border from *inside* the trailing pane's own render
/// tree (it's the trailing pane's own child). Same visual effect
/// (something else drawn over the border near the boundary), opposite
/// compositional relationship -- which is exactly the reported "overlaps
/// vs. sits inside" asymmetry once phrased pane-by-pane.
///
/// Horizon's own pane content (`render_node`'s `LayoutNode::Pane` arm) is
/// `.size_full()` inside its `resizable_panel()`, so nothing in
/// Horizon's *own* code currently keeps either border clear of that
/// reach. Rather than fighting the stock resizable component (reserved
/// styles, see `panel.rs`'s own warning), the fix lives entirely on
/// Horizon's side: `LayoutNode::Split` wraps each child in a thin
/// padding div before handing it to `resizable_panel()`, pulling the
/// child's own rendered content back by this exact amount on every edge
/// that faces a split boundary (every edge except a group's own outer
/// edges) -- see `split_child_insets`. `HANDLE_PADDING` is
/// `pub(crate)` inside gpui-component's own crate (not part of its public
/// API), so it can't be imported; this mirrors its value directly,
/// matching the handle's own documented reach depth exactly rather than
/// guessing a smaller number tuned to its 1px visible line specifically
/// (`HANDLE_SIZE`) -- deliberately the more conservative of the two, so
/// the fix holds regardless of exactly how gpui/Taffy's border-box
/// sizing resolves that inner line's precise sub-pixel position (verified
/// Taffy's box-sizing default is `BorderBox`,
/// `taffy-0.10.1/src/style/mod.rs`, but did not attempt to re-derive the
/// handle's own padding-vs-explicit-width resolution from that alone).
///
/// Round 6 (2026-07-16) asked for the handle's own resting divider LINE
/// to be removed too, keeping this 4px gutter. Investigated but left
/// unimplemented: the 1px line is `resize_handle.rs`'s inner
/// `div().bg(bg_color)...w(HANDLE_SIZE)` child, where `bg_color =
/// cx.theme().border` at rest (`cx.theme().drag_border` while actively
/// dragging -- a real, distinct, and still-working highlight; hover
/// itself reapplies the identical resting color via `group_hover`, a
/// no-op in the vendored source, not something this investigation
/// touched). Neither `ResizablePanelGroup`/`ResizablePanel` nor
/// `resize_handle` (itself `pub(crate)` inside gpui-component, not
/// public API) expose any builder to recolor or hide it. `cx.theme()`
/// resolves through a single gpui `Global` (`ActiveTheme for App`,
/// `theme/mod.rs`) with no subtree-scoped override mechanism, and the
/// `border` token it reads is the same one `gpui_component_theme_config`
/// (`src/theme.rs`) already projects Horizon's own `theme::border()`
/// onto for every other stock surface -- 43 files across the vendored
/// `ui` crate read `cx.theme().border` (buttons, inputs, tabs, dialogs,
/// tables, scrollbars, ...), so blanking that token to transparent to
/// silence this one line would blank borders everywhere else in the
/// app. No narrower token exists to isolate just this line. Per the
/// task's own instruction, left unimplemented rather than papering over
/// it with a Horizon-side overlay -- see the round-6 report for the
/// full trail.
const SPLIT_BOUNDARY_INSET_PX: f32 = 4.0;

/// Which of a split child's own edges (leading = left/top, trailing =
/// right/bottom, along the split's axis) face another pane across a
/// boundary and so need `SPLIT_BOUNDARY_INSET_PX`'s pullback -- see its
/// doc comment for why. `index` is this child's position among
/// `sibling_count` total children of the same split; every child except
/// the first is reached into on its own leading edge by its *own*
/// resize handle (owned because it isn't the first), and every child
/// except the last is reached into on its trailing edge by the *next*
/// child's handle. Pure and unit-tested so the edge selection is covered
/// without a GPUI render.
fn split_child_insets(index: usize, sibling_count: usize) -> (bool, bool) {
    let leading = index > 0;
    let trailing = index + 1 < sibling_count;
    (leading, trailing)
}

/// Built-in default chord for [`ToggleWorkspaceMode`] — mirrors the Floem
/// shell's `DEFAULT_WORKSPACE_MODE_CHORD`. Not bound when a
/// `[keybindings]` entry overrides it via the reserved
/// `keymap::WORKSPACE_MODE_PSEUDO_COMMAND` (see [`init`]).
const DEFAULT_WORKSPACE_MODE_KEYSTROKE: &str = "ctrl-'";

/// Equal-width distribution for the segmented tab strip: `true` gives
/// every tab an identical share of the track (each `Tab` gets an explicit
/// pixel width from [`equal_tab_width`]); `false` sizes each tab to its
/// own label instead, as before. The owner is comparing both in-session
/// (2026-07-14 GO on trying `Segmented`) -- flip this one constant to
/// switch, no other code changes needed.
const EQUAL_WIDTH_TABS: bool = true;

/// Fixed allowance for the segmented track's own non-tab chrome:
/// `TabBar`'s outer `px_2()` padding (8px each side) plus the `Segmented`
/// variant's inner `padding_x` for `XSmall` (2px each side -- see the
/// vendored `tab_bar.rs`'s `Segmented` branch of `RenderOnce::render`).
/// That's 20px; rounded up to 24px for slack. Deliberately a slight
/// overestimate of the real chrome, so computed tab widths lean a few
/// pixels narrow rather than push the track past `tabs-inner`'s
/// `overflow_x_scroll()` edge.
const EQUAL_WIDTH_CHROME_ALLOWANCE_PX: f32 = 24.0;

/// The `Segmented` variant's own inter-tab `gap` (2px at `XSmall`/`Small`),
/// counted once per boundary between tabs.
const EQUAL_WIDTH_GAP_PX: f32 = 2.0;

/// Never size a tab below this, however many are open or however narrow
/// the window gets -- `tabs-inner`'s `overflow_x_scroll()` (already
/// gpui-component's own default) takes over once tabs stop fitting, the
/// same fallback content-sized tabs already rely on today.
const EQUAL_WIDTH_MIN_TAB_PX: f32 = 40.0;

/// One equal-width tab's share of `strip_width` (the tab strip's measured
/// viewport width -- it spans the window edge to edge, see
/// [`WorkspaceShell::render_tab_strip`]'s `.w_full()`), after subtracting
/// the track's own fixed chrome and the gaps between tabs. Pure so it's
/// unit-testable without a window; kept in lockstep with the constants
/// above rather than reading gpui-component's private layout directly.
fn equal_tab_width(strip_width: Pixels, tab_count: usize) -> Pixels {
    if tab_count == 0 {
        return px(0.0);
    }
    let gaps = EQUAL_WIDTH_GAP_PX * tab_count.saturating_sub(1) as f32;
    let usable = (f32::from(strip_width) - EQUAL_WIDTH_CHROME_ALLOWANCE_PX - gaps).max(0.0);
    px((usable / tab_count as f32).max(EQUAL_WIDTH_MIN_TAB_PX))
}

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

pub fn init(cx: &mut App) {
    let config = horizon_config::load();

    let workspace_mode_override = config
        .keybindings
        .iter()
        .find(|(_, command)| command.as_str() == keymap::WORKSPACE_MODE_PSEUDO_COMMAND)
        .map(|(chord, _)| chord.as_str());

    let mut bindings = vec![
        KeyBinding::new("h", ModeMoveLeft, Some(MODE_CONTEXT)),
        KeyBinding::new("j", ModeMoveDown, Some(MODE_CONTEXT)),
        KeyBinding::new("k", ModeMoveUp, Some(MODE_CONTEXT)),
        KeyBinding::new("l", ModeMoveRight, Some(MODE_CONTEXT)),
        KeyBinding::new("left", ModeMoveLeft, Some(MODE_CONTEXT)),
        KeyBinding::new("down", ModeMoveDown, Some(MODE_CONTEXT)),
        KeyBinding::new("up", ModeMoveUp, Some(MODE_CONTEXT)),
        KeyBinding::new("right", ModeMoveRight, Some(MODE_CONTEXT)),
        KeyBinding::new("enter", ModeCommit, Some(MODE_CONTEXT)),
        KeyBinding::new("escape", ModeCancel, Some(MODE_CONTEXT)),
        KeyBinding::new("t", NewTab, Some(MODE_CONTEXT)),
        KeyBinding::new("a", NewAgentTab, Some(MODE_CONTEXT)),
        KeyBinding::new("s", SplitPane, Some(MODE_CONTEXT)),
        KeyBinding::new("x", ClosePane, Some(MODE_CONTEXT)),
        KeyBinding::new("tab", NextTab, Some(MODE_CONTEXT)),
        KeyBinding::new(":", OpenPalette, Some(MODE_CONTEXT)),
    ];

    match workspace_mode_override {
        Some(chord) => match keymap::gpui_keystroke(chord) {
            Some(keystroke) => {
                bindings.push(KeyBinding::new(&keystroke, ToggleWorkspaceMode, None))
            }
            None => {
                eprintln!(
                    "horizon config: skipping keybinding `{chord}` = \
                     `{}`: unrecognized chord",
                    keymap::WORKSPACE_MODE_PSEUDO_COMMAND
                );
                bindings.push(KeyBinding::new(
                    DEFAULT_WORKSPACE_MODE_KEYSTROKE,
                    ToggleWorkspaceMode,
                    None,
                ));
            }
        },
        None => bindings.push(KeyBinding::new(
            DEFAULT_WORKSPACE_MODE_KEYSTROKE,
            ToggleWorkspaceMode,
            None,
        )),
    }

    // `[keybindings]` config entries layer on top of the built-ins above:
    // later-registered bindings take precedence in gpui at the same
    // context depth (`Keymap::bindings_for_input`'s doc comment — "the
    // ones added to the keymap later take precedence"), so pushing these
    // after the built-ins is enough for a config entry to override one
    // bound to the same chord.
    for (chord, command) in &config.keybindings {
        if command == keymap::WORKSPACE_MODE_PSEUDO_COMMAND {
            continue; // handled above
        }
        let Some(keystroke) = keymap::gpui_keystroke(chord) else {
            eprintln!(
                "horizon config: skipping keybinding `{chord}` = `{command}`: unrecognized chord"
            );
            continue;
        };
        if command == keymap::OPEN_PALETTE_PSEUDO_COMMAND {
            bindings.push(KeyBinding::new(&keystroke, OpenPalette, None));
            continue;
        }
        let Some(id) = keymap::command_for(command) else {
            eprintln!(
                "horizon config: skipping keybinding `{chord}` = `{command}`: unknown command id"
            );
            continue;
        };
        bindings.push(KeyBinding::new(&keystroke, RunCommand { id }, None));
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

    cx.bind_keys(bindings);
}

pub struct WorkspaceShell {
    workspace: Workspace,
    workspace_state: WorkspaceStateStore,
    persistence_ready: bool,
    restoring_workspace: bool,
    workspace_restore_failed: bool,
    // This instance's control socket — every spawned pane gets it as
    // HORIZON_SOCKET so CLIs invoked inside reach back here.
    socket_path: std::path::PathBuf,
    // The session store — the GPUI shell's Registry counterpart: PTY
    // sessions live here keyed by SessionId, independent of pane views,
    // so closing a pane detaches (session survives, scrollback intact)
    // and terminating is the explicit destructive path.
    sessions: HashMap<SessionId, Entity<TerminalSession>>,
    agent_sessions: HashMap<SessionId, Entity<AgentSession>>,
    // Staged by `external_new_session` (a role-tagged create, e.g.
    // `new-config-agent`) and consumed by `reconcile` when it actually
    // starts the session — the model's `open_tab_with_new_session_*`
    // call only yields a `SessionId`, so the role has nowhere else to
    // ride until reconcile turns that id into a live agent session.
    pending_roles: HashMap<SessionId, horizon_agent::roles::RoleId>,
    // Staged before session-creating workspace mutations and consumed by
    // reconcile. Sessiond resolves the source session's live cwd; Horizon
    // carries only the source id and fallback spawn input.
    pending_terminal_spawns: HashMap<SessionId, PendingTerminalSpawn>,
    // Created eagerly before the first reconcile. Its raw FIFO accepts
    // terminal and agent requests while connect/Hello proceeds in the
    // background.
    sessiond: Option<SessiondHandle>,
    reload_in_progress: bool,
    panes: HashMap<PaneId, PaneView>,
    // This window — needed by `Reload Session Runtime`'s post-resume step,
    // which rebuilds pane views from a background thread's async
    // continuation (no `&mut Window` of its own to reuse).
    window: AnyWindowHandle,
    // Focused while workspace mode is active, so mode keys dispatch here
    // instead of reaching the terminal.
    focus_handle: FocusHandle,
    palette: Option<Entity<ListState<PaletteDelegate>>>,
    _palette_subscription: Option<Subscription>,
    session_manager: Option<Entity<ListState<SessionManagerDelegate>>>,
    _session_manager_subscription: Option<Subscription>,
    view_chooser: Option<Entity<ListState<ViewChooserDelegate>>>,
    _view_chooser_subscription: Option<Subscription>,
    // The placement the open view chooser will apply on confirm.
    pending_placement: Option<Placement>,
    // Snapshot of workspace mode's dim/cursor pattern (the cursor pane, if
    // the mode was active), taken by `freeze_scrim_before_modal_exit`
    // right before a modal-opening handler calls
    // `Workspace::exit_workspace_mode`. `render_node`
    // (`effective_scrim_pattern`) substitutes this for the (now-inactive)
    // live mode state while any control-surface modal is open, for both
    // the scrim and the cursor-pane border (2026-07-15 round 3: modal-open
    // is fully chrome-neutral, not just scrim-neutral) -- see
    // `effective_scrim_pattern`'s doc comment and `docs/theme-design.md`'s
    // scrim section. `None` means the mode wasn't active when the modal
    // opened (or no modal is open).
    scrim_freeze: Option<PaneId>,
    // The terminal session `sync_terminal_focus` last sent `Focus(true)`
    // to, so a transition can send `Focus(false)` to the one it's about
    // to stop being true for. See `focus_transition`.
    last_focused_terminal: Option<SessionId>,
    // Handed to every `TerminalSession::spawn` (cloned per session) so a
    // PTY-side shell exit can notify the shell to terminate that workspace
    // session -- see the `terminal_exit_rx` pump spawned in `new`.
    terminal_exit_tx: futures::channel::mpsc::UnboundedSender<SessionId>,
}

impl WorkspaceShell {
    pub fn new(
        socket_path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workspace_state = WorkspaceStateStore::from_environment();
        let (workspace, restoring_workspace, persistence_ready) =
            load_workspace_state(&mut workspace_state);
        let (sessiond, host_tool_rx) =
            SessiondHandle::start(&horizon_agent::socket::default_socket_path(), &socket_path);
        let (terminal_exit_tx, terminal_exit_rx) = futures::channel::mpsc::unbounded();
        let mut shell = Self {
            workspace,
            workspace_state,
            persistence_ready,
            restoring_workspace,
            workspace_restore_failed: false,
            socket_path,
            sessions: HashMap::new(),
            agent_sessions: HashMap::new(),
            pending_roles: HashMap::new(),
            pending_terminal_spawns: HashMap::new(),
            sessiond: Some(sessiond.clone()),
            reload_in_progress: false,
            panes: HashMap::new(),
            window: window.window_handle(),
            focus_handle: cx.focus_handle(),
            palette: None,
            _palette_subscription: None,
            session_manager: None,
            _session_manager_subscription: None,
            view_chooser: None,
            _view_chooser_subscription: None,
            pending_placement: None,
            scrim_freeze: None,
            last_focused_terminal: None,
            terminal_exit_tx,
        };
        // Window activation/deactivation doesn't otherwise touch the
        // model, so it needs its own observer alongside `focus_active`'s
        // call to `sync_terminal_focus` (every model mutation that can
        // change the active pane).
        cx.observe_window_activation(window, |shell, window, cx| {
            shell.sync_terminal_focus(window, cx);
        })
        .detach();
        shell.wire_host_tools(sessiond.responder(), host_tool_rx, cx);
        shell.wire_terminal_exit(terminal_exit_rx, cx);
        if shell.restoring_workspace {
            shell.spawn_workspace_restore(sessiond, cx);
        } else {
            shell.reconcile(window, cx);
            shell.focus_active(window, cx);
            shell.spawn_terminal_resume(sessiond.clone(), cx);
            shell.spawn_agent_resume(sessiond, cx);
        }
        shell
    }

    fn persist_workspace(&mut self) {
        if !self.persistence_ready || self.restoring_workspace {
            return;
        }
        let json = match self.workspace.to_persisted_json() {
            Ok(json) => json,
            Err(error) => {
                eprintln!("workspace state is not persistable: {error}");
                return;
            }
        };
        if let Err(error) = self.workspace_state.save(&json) {
            eprintln!(
                "failed to save workspace state to {}: {error}",
                self.workspace_state.path().display()
            );
        }
    }

    fn fail_workspace_restore(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        if !self.restoring_workspace {
            return;
        }
        eprintln!("workspace restore failed: {error}");
        self.workspace_restore_failed = true;
        cx.notify();
    }

    /// Bring the session store and the PaneId → view map in line with
    /// the model. Sessions the model no longer knows (terminated) are
    /// shut down and dropped; sessions without panes stay alive
    /// (detached); every pane gets a view bound to its session's entity,
    /// so a reattached pane resumes with scrollback intact.
    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let summaries = self.workspace.session_summaries();
        let known: std::collections::HashSet<SessionId> =
            summaries.iter().map(|summary| summary.id).collect();
        self.sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        self.agent_sessions.retain(|id, session| {
            let keep = known.contains(id);
            if !keep {
                session.read(cx).shutdown();
            }
            keep
        });
        for summary in summaries {
            match summary.kind {
                SessionKind::Terminal => {
                    let id = summary.id;
                    if !self.sessions.contains_key(&id) {
                        let pending =
                            self.pending_terminal_spawns.remove(&id).unwrap_or_else(|| {
                                PendingTerminalSpawn {
                                    source_session_id: None,
                                    fallback_cwd: Self::default_terminal_cwd(),
                                }
                            });
                        let Some(sessiond) = self.sessiond.as_ref() else {
                            continue;
                        };
                        let wire = sessiond
                            .start_terminal(id.as_uuid(), self.terminal_spawn_spec(pending));
                        let exit_tx = self.terminal_exit_tx.clone();
                        self.sessions.insert(
                            id,
                            cx.new(|cx| TerminalSession::spawn(wire, id, exit_tx, cx)),
                        );
                    }
                }
                SessionKind::Agent => {
                    if self.agent_sessions.contains_key(&summary.id) {
                        continue;
                    }
                    let Some(handle) = self.sessiond.clone() else {
                        continue;
                    };
                    let provider_id =
                        horizon_agent::contract::ProviderRegistry::default().default_provider_id();
                    let role_id = self.pending_roles.remove(&summary.id);
                    let session_handle =
                        handle.start_session(agent_session_id(summary.id), provider_id, role_id);
                    self.agent_sessions.insert(
                        summary.id,
                        cx.new(|cx| AgentSession::new(session_handle, cx)),
                    );
                }
            }
        }

        let pane_ids = self.workspace.all_pane_ids();
        self.panes.retain(|id, _| pane_ids.contains(id));
        for pane_id in pane_ids {
            if self.panes.contains_key(&pane_id) {
                continue;
            }
            if let Some(session) = self
                .workspace
                .terminal_session_id(pane_id)
                .and_then(|id| self.sessions.get(&id).cloned())
            {
                self.panes.insert(
                    pane_id,
                    PaneView::Terminal(cx.new(|cx| TerminalView::new(session.clone(), window, cx))),
                );
            } else if let Some(session) = self
                .workspace
                .agent_session_id(pane_id)
                .and_then(|id| self.agent_sessions.get(&id).cloned())
            {
                self.panes.insert(
                    pane_id,
                    PaneView::Agent(cx.new(|cx| AgentView::new(session.clone(), window, cx))),
                );
            } else if matches!(
                self.workspace.pane_kind(pane_id),
                Some(PaneKind::View(ViewKind::ThemeSettings))
            ) {
                self.panes.insert(
                    pane_id,
                    PaneView::ThemeSettings(cx.new(|cx| ThemeSettingsView::new(window, cx))),
                );
            }
        }
        self.persist_workspace();
        cx.notify();
    }

    /// Wires the host-tool responder for the already-adopted runtime:
    /// `workspace.snapshot` requests are answered on the UI thread from
    /// the live model, mirroring the Floem shell's
    /// `wire_host_tool_responder`.
    fn wire_host_tools(
        &mut self,
        responder: SessiondResponder,
        host_tool_rx: crossbeam_channel::Receiver<horizon_agent::wire::HostToolRequest>,
        cx: &mut Context<Self>,
    ) {
        let (async_tx, mut async_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            while let Ok(request) = host_tool_rx.recv() {
                if async_tx.unbounded_send(request).is_err() {
                    return;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(request) = async_rx.next().await {
                let output = this
                    .update(cx, |shell, _| match request.tool_id.as_str() {
                        "workspace.snapshot" => {
                            horizon_workspace::snapshot::workspace_snapshot(&shell.workspace)
                        }
                        other => serde_json::json!({
                            "error": format!("unknown host tool `{other}`")
                        }),
                    })
                    .unwrap_or_else(
                        |_| serde_json::json!({ "error": "the workspace shell is gone" }),
                    );
                responder.respond_host_tool(horizon_agent::wire::HostToolResponse {
                    request_id: request.request_id,
                    output,
                });
            }
        })
        .detach();
    }

    /// Wires the receiving end of every `TerminalSession`'s `exit_tx`: a PTY
    /// child exiting (e.g. the user typing `exit`) notifies the shell with
    /// its session id, and the shell terminates that workspace session --
    /// "shell exit terminates the session" (decision 1). Already async
    /// (`TerminalSession::spawn` hands out a `futures` unbounded sender), so
    /// unlike `wire_host_tools` this needs no blocking-to-async bridge
    /// thread, just the pump.
    fn wire_terminal_exit(
        &self,
        mut exit_rx: futures::channel::mpsc::UnboundedReceiver<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while let Some(session_id) = exit_rx.next().await {
                let _ = window_handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |shell, cx| {
                        shell.handle_terminal_exited(session_id, window, cx);
                    });
                });
            }
        })
        .detach();
    }

    /// Terminates the workspace session whose shell just exited -- whether
    /// it was attached to a pane or sitting detached (session-manager
    /// entry), `terminate_session` handles both uniformly. Reseeds a fresh
    /// terminal pane if this emptied the workspace (decision 2: see
    /// `ensure_workspace_has_pane`'s doc for why a zero-tab workspace must
    /// never be reached). Ignored while a restore is in progress: the
    /// session store isn't reconciled with the model yet, so there is
    /// nothing meaningful to terminate.
    fn handle_terminal_exited(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        if !self.workspace.terminate_session(session_id) {
            return;
        }
        ensure_workspace_has_pane(&mut self.workspace);
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// Lists the agent sessions hosted by the already-adopted runtime on a
    /// background thread, then adopts each as a detached
    /// session: registered in the model (so the session manager shows it)
    /// and attached over the wire (so its replayed transcript is ready
    /// when a pane picks it up). Shared by two callers: startup
    /// ([`Self::new`], against a freshly opened window with no agent
    /// panes yet) and `Reload Session Runtime`
    /// ([`Self::reload_session_runtime`], after the old connection has
    /// drained — see that function's doc comment for why its
    /// `agent_sessions`/agent-pane views are already cleared by the time
    /// this runs). Either way, the post-adopt `reconcile`/`focus_active`
    /// pass rebuilds any agent pane view whose session id this resume
    /// just reattached — a no-op at startup (no agent panes exist yet)
    /// and the reload's actual pane-rebuild step.
    fn spawn_agent_resume(&self, handle: SessiondHandle, cx: &mut Context<Self>) {
        let window_handle = self.window;
        let (startup_tx, mut startup_rx) = futures::channel::mpsc::unbounded();
        let list_handle = handle.clone();
        std::thread::spawn(move || {
            let summaries = list_handle.session_list();
            let _ = startup_tx.unbounded_send(summaries);
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let summaries = match startup_rx.next().await {
                Some(Ok(summaries)) => summaries,
                Some(Err(error)) => {
                    eprintln!("failed to list agent sessions: {error}");
                    Vec::new()
                }
                None => Vec::new(),
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    let Some(adopted) = shell.sessiond.clone() else {
                        return;
                    };
                    if !adopted.same_runtime(&handle) {
                        return;
                    }
                    for summary in summaries {
                        let session_id = SessionId::from_uuid(summary.session_id.as_uuid());
                        if shell.agent_sessions.contains_key(&session_id) {
                            continue;
                        }
                        if shell
                            .workspace
                            .session_pane_kind(session_id)
                            .is_some_and(|kind| kind != PaneKind::Agent)
                        {
                            eprintln!(
                                "ignoring agent session {}: its id is already used by a terminal",
                                session_id.as_uuid()
                            );
                            continue;
                        }
                        shell
                            .workspace
                            .register_detached_session(PaneKind::Agent, session_id);
                        let session_handle = adopted.attach_session(summary.session_id);
                        shell.agent_sessions.insert(
                            session_id,
                            cx.new(|cx| AgentSession::new(session_handle, cx)),
                        );
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// Restores a persisted workspace only after both domain inventories are
    /// authoritative and every retained terminal has acknowledged Attach.
    /// Until this barrier opens, normal reconcile must not see the saved ids:
    /// it would interpret a missing entity as a request to create a new
    /// process with that id.
    fn spawn_workspace_restore(&self, handle: SessiondHandle, cx: &mut Context<Self>) {
        let window_handle = self.window;
        let (list_tx, mut list_rx) = futures::channel::mpsc::unbounded();
        let list_handle = handle.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let terminals = list_handle.terminal_list()?;
                let agents = list_handle.session_list()?;
                Ok::<_, String>((terminals, agents))
            })();
            let _ = list_tx.unbounded_send(result);
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let (terminal_summaries, agent_summaries) = match list_rx.next().await {
                Some(Ok(summaries)) => summaries,
                Some(Err(error)) => {
                    let _ = this.update(cx, |shell, cx| {
                        shell.fail_workspace_restore(error, cx);
                    });
                    return;
                }
                None => {
                    let _ = this.update(cx, |shell, cx| {
                        shell.fail_workspace_restore("inventory worker stopped", cx);
                    });
                    return;
                }
            };

            let candidates = this
                .update(cx, |shell, _| {
                    let adopted = shell.sessiond.as_ref()?;
                    if !adopted.same_runtime(&handle) {
                        return None;
                    }

                    let expected: HashMap<_, _> = shell
                        .workspace
                        .session_summaries()
                        .into_iter()
                        .map(|summary| (summary.id.as_uuid(), summary.kind))
                        .collect();
                    let terminal_ids: HashSet<_> = terminal_summaries
                        .into_iter()
                        .map(|summary| summary.session_id)
                        .collect();
                    let agent_ids: HashSet<_> = agent_summaries
                        .into_iter()
                        .map(|summary| summary.session_id.as_uuid())
                        .collect();
                    let conflicts: HashSet<_> =
                        terminal_ids.intersection(&agent_ids).copied().collect();
                    for id in &conflicts {
                        eprintln!(
                            "ignoring session {id}: it appears in both terminal and agent inventories"
                        );
                    }

                    let terminals = terminal_ids
                        .into_iter()
                        .filter(|id| !conflicts.contains(id))
                        .filter(|id| {
                            let matches = expected
                                .get(id)
                                .is_none_or(|kind| *kind == SessionKind::Terminal);
                            if !matches {
                                eprintln!(
                                    "ignoring terminal session {id}: persisted kind is agent"
                                );
                            }
                            matches
                        })
                        .collect::<Vec<_>>();
                    let agents = agent_ids
                        .into_iter()
                        .filter(|id| !conflicts.contains(id))
                        .filter(|id| {
                            let matches = expected
                                .get(id)
                                .is_none_or(|kind| *kind == SessionKind::Agent);
                            if !matches {
                                eprintln!(
                                    "ignoring agent session {id}: persisted kind is terminal"
                                );
                            }
                            matches
                        })
                        .collect::<Vec<_>>();
                    Some((terminals, agents))
                })
                .ok()
                .flatten();
            let Some((terminal_ids, agent_ids)) = candidates else {
                return;
            };

            let (attach_tx, mut attach_rx) = futures::channel::mpsc::unbounded();
            let attach_handle = handle.clone();
            std::thread::spawn(move || {
                let terminals = attach_handle.attach_terminals(terminal_ids);
                let agents = agent_ids
                    .into_iter()
                    .map(|id| {
                        let session_id = AgentSessionId::from_uuid(id);
                        (id, attach_handle.attach_session(session_id))
                    })
                    .collect::<Vec<_>>();
                let _ = attach_tx.unbounded_send((terminals, agents));
            });
            let Some((terminals, agents)) = attach_rx.next().await else {
                let _ = this.update(cx, |shell, cx| {
                    shell.fail_workspace_restore("attach worker stopped", cx);
                });
                return;
            };

            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    let Some(adopted) = shell.sessiond.as_ref() else {
                        return;
                    };
                    if !adopted.same_runtime(&handle) {
                        return;
                    }

                    let inventory = SessionInventory::new(
                        terminals
                            .iter()
                            .map(|(id, _)| SessionId::from_uuid(*id))
                            .collect(),
                        agents
                            .iter()
                            .map(|(id, _)| SessionId::from_uuid(*id))
                            .collect(),
                    );
                    if let Err(error) = shell.workspace.reconcile_session_inventory(&inventory) {
                        shell.fail_workspace_restore(
                            format_args!("inventory is invalid: {error}"),
                            cx,
                        );
                        return;
                    }

                    for (id, wire) in terminals {
                        let session_id = SessionId::from_uuid(id);
                        if shell.workspace.session_pane_kind(session_id)
                            == Some(PaneKind::Terminal)
                        {
                            let exit_tx = shell.terminal_exit_tx.clone();
                            shell.sessions.insert(
                                session_id,
                                cx.new(|cx| TerminalSession::spawn(wire, session_id, exit_tx, cx)),
                            );
                        }
                    }
                    for (id, wire) in agents {
                        let session_id = SessionId::from_uuid(id);
                        if shell.workspace.session_pane_kind(session_id) == Some(PaneKind::Agent) {
                            shell.agent_sessions.insert(
                                session_id,
                                cx.new(|cx| AgentSession::new(wire, cx)),
                            );
                        }
                    }

                    shell.restoring_workspace = false;
                    shell.workspace_restore_failed = false;
                    shell.persistence_ready = true;
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// Discovers terminal sessions left alive by an earlier UI process and
    /// adopts them as detached sessions without delaying the fresh startup
    /// terminal. Listing and attaching are split by a UI-thread comparison:
    /// the just-created terminal (and any session created while List is in
    /// flight) must not have its existing route replaced by an Attach.
    fn spawn_terminal_resume(&self, handle: SessiondHandle, cx: &mut Context<Self>) {
        let window_handle = self.window;
        let (list_tx, mut list_rx) = futures::channel::mpsc::unbounded();
        let list_handle = handle.clone();
        std::thread::spawn(move || {
            let _ = list_tx.unbounded_send(list_handle.terminal_list());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            let Some(Ok(summaries)) = list_rx.next().await else {
                return;
            };
            let candidates = this
                .update(cx, |shell, _| {
                    let Some(adopted) = shell.sessiond.as_ref() else {
                        return Vec::new();
                    };
                    if !adopted.same_runtime(&handle) {
                        return Vec::new();
                    }
                    let known = shell
                        .workspace
                        .session_summaries()
                        .into_iter()
                        .map(|summary| summary.id)
                        .collect();
                    terminal_resume_candidates(summaries, &known)
                })
                .unwrap_or_default();
            if candidates.is_empty() {
                return;
            }

            let (attach_tx, mut attach_rx) = futures::channel::mpsc::unbounded();
            let attach_handle = handle.clone();
            std::thread::spawn(move || {
                let attached = attach_handle.attach_terminals(candidates);
                let _ = attach_tx.unbounded_send(attached);
            });
            let Some(attached) = attach_rx.next().await else {
                return;
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |shell, cx| {
                    let Some(adopted) = shell.sessiond.as_ref() else {
                        return;
                    };
                    if !adopted.same_runtime(&handle) {
                        return;
                    }
                    for (wire_id, wire) in attached {
                        let session_id = SessionId::from_uuid(wire_id);
                        if shell
                            .workspace
                            .session_summaries()
                            .iter()
                            .any(|summary| summary.id == session_id)
                        {
                            continue;
                        }
                        shell
                            .workspace
                            .register_detached_session(PaneKind::Terminal, session_id);
                        let exit_tx = shell.terminal_exit_tx.clone();
                        shell.sessions.insert(
                            session_id,
                            cx.new(|cx| TerminalSession::spawn(wire, session_id, exit_tx, cx)),
                        );
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                });
            });
        })
        .detach();
    }

    /// Drains the explicit old runtime on a background thread, then creates
    /// exactly one fresh eager runtime and lists/loads persisted agents. The
    /// caller has already removed terminal model sessions and dropped every
    /// stale entity/view without sending semantic agent shutdown commands.
    fn reload_session_runtime(&self, old: Option<SessiondHandle>, cx: &mut Context<Self>) {
        let socket_path = horizon_agent::socket::default_socket_path();
        let restart_socket = socket_path.clone();
        let control_socket = self.socket_path.clone();
        let (drained_tx, mut drained_rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            if let Some(handle) = old {
                if handle.begin_reload() {
                    if let Err(error) = wait_for_drain(&socket_path) {
                        eprintln!("horizon-sessiond did not drain cleanly: {error}");
                    }
                }
                handle.stop_and_wait();
            }
            let _ = drained_tx.unbounded_send(());
        });
        cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            if drained_rx.next().await.is_none() {
                return;
            }
            let _ = this.update(cx, |shell, cx| {
                ensure_workspace_has_pane(&mut shell.workspace);
                let (handle, host_tool_rx) =
                    SessiondHandle::start(&restart_socket, &control_socket);
                shell.sessiond = Some(handle.clone());
                shell.reload_in_progress = false;
                shell.wire_host_tools(handle.responder(), host_tool_rx, cx);
                shell.spawn_agent_resume(handle, cx);
            });
        })
        .detach();
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self
            .workspace
            .cursor_pane_id()
            .and_then(|id| self.panes.get(&id))
        {
            window.focus(&view.focus_handle(cx), cx);
        }
        self.sync_terminal_focus(window, cx);
        self.persist_workspace();
    }

    /// Composes Horizon's own window focus with which pane is active into
    /// a single `TerminalCommand::Focus` transition to the session store
    /// — the GPUI counterpart of the Floem shell's
    /// `app::runtime::wire_focus_reporting`. Only the active pane's
    /// terminal ever believes it has focus: an agent pane active (or the
    /// window itself losing OS focus) means "no terminal focused," so the
    /// previously-focused terminal (if any) gets `Focus(false)` and the
    /// newly-focused one (if any) gets `Focus(true)` — never both to the
    /// same session, and nothing at all when the composed target hasn't
    /// changed. Called from every mutation that can change the active
    /// pane ([`Self::focus_active`], [`Self::activate_pane`]) and from
    /// the window-activation observer registered in [`Self::new`].
    fn sync_terminal_focus(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        let (unfocus, focus) = focus_transition(
            window.is_window_active(),
            self.workspace.active_terminal_session_id(),
            self.last_focused_terminal,
        );
        if unfocus.is_none() && focus.is_none() {
            return;
        }
        if let Some(session_id) = unfocus {
            self.send_terminal_focus(session_id, false, cx);
        }
        if let Some(session_id) = focus {
            self.send_terminal_focus(session_id, true, cx);
        }
        self.last_focused_terminal = focus;
    }

    fn send_terminal_focus(&self, session_id: SessionId, focused: bool, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(&session_id) {
            session.read(cx).send_focus(focused);
        }
    }

    fn toggle_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        if self.workspace.is_workspace_mode_active() {
            self.workspace.cancel_workspace_mode();
            self.focus_active(window, cx);
        } else {
            self.workspace.enter_workspace_mode();
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn mode_move(&mut self, direction: Direction, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        self.workspace.move_cursor(direction);
        cx.notify();
    }

    fn mode_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        self.workspace.commit_workspace_mode();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn mode_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        ) {
            return;
        }
        self.workspace.cancel_workspace_mode();
        self.focus_active(window, cx);
        cx.notify();
    }

    fn pending_terminal_spawn(&self, explicit_source: Option<SessionId>) -> PendingTerminalSpawn {
        PendingTerminalSpawn {
            source_session_id: terminal_spawn_source(
                explicit_source,
                self.workspace.active_session_id(),
            ),
            fallback_cwd: Self::default_terminal_cwd(),
        }
    }

    fn default_terminal_cwd() -> std::path::PathBuf {
        terminal_fallback_cwd(
            std::env::current_dir().ok(),
            std::env::var_os("HOME").map(std::path::PathBuf::from),
        )
    }

    fn terminal_spawn_spec(&self, pending: PendingTerminalSpawn) -> TerminalSpawnSpec {
        let config = &horizon_config::load().terminal;
        let shell = std::env::var("SHELL")
            .ok()
            .or_else(|| config.shell.clone())
            .unwrap_or_else(|| "/bin/sh".to_string());
        TerminalSpawnSpec {
            shell,
            args: config.shell_args.clone().unwrap_or_default(),
            term: config
                .term
                .clone()
                .unwrap_or_else(|| "xterm-256color".to_string()),
            scrollback_lines: config
                .scrollback_lines
                .unwrap_or(TerminalCoreOptions::default().scrollback_lines),
            color_scheme: theme::terminal_color_scheme(),
            control_socket: self.socket_path.clone(),
            fallback_cwd: pending.fallback_cwd,
            spawn_source_session_id: pending.source_session_id.map(SessionId::as_uuid),
            initial_size: TerminalSize::new(80, 24),
        }
    }

    /// The one interactive session-creation path: what the view chooser
    /// confirms with. Terminal cwd and agent role ride the same staging
    /// maps `reconcile` consumes.
    fn create_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        if let PaneKind::View(view_kind) = kind {
            // A session-less first-party view: no session id to create,
            // no sessiond spawn, and no `pending_terminal_spawns`/
            // `pending_roles` bookkeeping -- those exist only for the
            // session-backed kinds handled below.
            match placement {
                Placement::NewTab => {
                    self.workspace.open_tab(kind, None);
                }
                Placement::SplitRight | Placement::SplitDown => {
                    let axis = if placement == Placement::SplitRight {
                        SplitAxis::Horizontal
                    } else {
                        SplitAxis::Vertical
                    };
                    self.workspace.split_active_tab_with_view(view_kind, axis);
                }
            }
            self.reconcile(window, cx);
            self.focus_active(window, cx);
            return;
        }
        let terminal_spawn =
            matches!(kind, PaneKind::Terminal).then(|| self.pending_terminal_spawn(None));
        let session_id = match placement {
            Placement::NewTab => Some(
                self.workspace
                    .open_tab_with_new_session_activated(kind, true),
            ),
            Placement::SplitRight | Placement::SplitDown => {
                let axis = if placement == Placement::SplitRight {
                    SplitAxis::Horizontal
                } else {
                    SplitAxis::Vertical
                };
                self.workspace.active_session_id().and_then(|target| {
                    self.workspace
                        .split_session_with_new_session(target, kind, axis, true)
                })
            }
        };
        if let Some(session_id) = session_id {
            if let Some(spawn) = terminal_spawn {
                self.pending_terminal_spawns.insert(session_id, spawn);
            }
            if let Some(role_id) = role_id {
                self.pending_roles.insert(session_id, role_id);
            }
        }
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    /// Snapshots workspace mode's dim/cursor pattern into `scrim_freeze`
    /// right before a modal-opening handler exits the mode -- see
    /// `effective_scrim_pattern`'s doc comment for why this is necessary
    /// (the mode's own key bindings must detach before the modal's `List`
    /// takes focus, which erases `cursor_pane_id`'s target).
    /// `render_node` consumes this for both the scrim and the cursor-pane
    /// border while a modal is open. Must be called before
    /// `Workspace::exit_workspace_mode`, not after.
    fn freeze_scrim_before_modal_exit(&mut self) {
        self.scrim_freeze = if self.workspace.is_workspace_mode_active() {
            self.workspace.cursor_pane_id()
        } else {
            None
        };
    }

    fn open_view_chooser(
        &mut self,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.freeze_scrim_before_modal_exit();
        self.workspace.exit_workspace_mode();
        self.pending_placement = Some(placement);
        let list = cx.new(|cx| {
            let mut list = ListState::new(ViewChooserDelegate::new(), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let choice = list.read(cx).delegate().choice_at(*index).cloned();
                    let placement = shell.pending_placement.take();
                    shell.close_view_chooser(window, cx);
                    if let (Some(choice), Some(placement)) = (choice, placement) {
                        shell.create_session(choice.kind, choice.role_id, placement, window, cx);
                    }
                }
                ListEvent::Cancel => {
                    shell.pending_placement = None;
                    shell.close_view_chooser(window, cx);
                }
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.view_chooser = Some(list);
        self._view_chooser_subscription = Some(subscription);
        cx.notify();
    }

    fn close_view_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_chooser = None;
        self._view_chooser_subscription = None;
        self.scrim_freeze = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The active pane's agent session, when it is an agent pane.
    fn active_agent_session(&self) -> Option<Entity<AgentSession>> {
        let pane_id = self.workspace.cursor_pane_id()?;
        let session_id = self.workspace.agent_session_id(pane_id)?;
        self.agent_sessions.get(&session_id).cloned()
    }

    /// The M3 dispatch point: every surface (palette, keybindings, and
    /// later the control plane) funnels through here — the GPUI
    /// counterpart of the Floem shell's `execute_command`.
    fn execute(&mut self, id: CommandId, window: &mut Window, cx: &mut Context<Self>) {
        if command_blocked_by_restore(self.restoring_workspace, self.workspace_restore_failed, id) {
            return;
        }
        match id {
            CommandId::SplitRight => self.open_view_chooser(Placement::SplitRight, window, cx),
            CommandId::SplitDown => self.open_view_chooser(Placement::SplitDown, window, cx),
            CommandId::NewTab => self.open_view_chooser(Placement::NewTab, window, cx),
            CommandId::FocusNextPane => {
                self.workspace.focus_next();
                self.focus_active(window, cx);
                cx.notify();
            }
            CommandId::CloseActivePane => self.close_pane(window, cx),
            CommandId::CloseActiveTab => {
                self.workspace.exit_workspace_mode();
                self.workspace.close_active_tab();
                self.reconcile(window, cx);
                self.focus_active(window, cx);
            }
            CommandId::TerminateActiveSession => {
                self.workspace.exit_workspace_mode();
                self.workspace.terminate_active_session();
                ensure_workspace_has_pane(&mut self.workspace);
                self.reconcile(window, cx);
                self.focus_active(window, cx);
            }
            CommandId::TerminateAllDetachedSessions => {
                for summary in self.workspace.detached_session_summaries() {
                    self.workspace.terminate_session(summary.id);
                }
                self.reconcile(window, cx);
            }
            CommandId::OpenSessionManager => self.open_session_manager(window, cx),
            CommandId::ApproveToolCall => {
                if let Some(session) = self.active_agent_session() {
                    let pending = horizon_agent::frame::actionable_pending_approval_call_ids_in(
                        &session.read(cx).frame.items,
                    );
                    if let Some(call_id) = pending.first() {
                        session.read(cx).approve(call_id.clone());
                    }
                }
            }
            CommandId::DenyToolCall => {
                if let Some(session) = self.active_agent_session() {
                    let pending = horizon_agent::frame::actionable_pending_approval_call_ids_in(
                        &session.read(cx).frame.items,
                    );
                    if let Some(call_id) = pending.first() {
                        session.read(cx).deny(call_id.clone());
                    }
                }
            }
            CommandId::CancelAgentTurn => {
                if let Some(session) = self.active_agent_session() {
                    session.read(cx).cancel();
                }
            }
            CommandId::ReloadConfig => match horizon_config::reload() {
                Ok(raw) => {
                    theme::reload_from(&raw);
                    theme::apply_gpui_component_theme(cx);
                    window.refresh();
                }
                Err(error) => eprintln!("reload-config failed: {error}"),
            },
            CommandId::ReloadSessionRuntime => {
                if self.reload_in_progress {
                    return;
                }
                self.reload_in_progress = true;
                let old = self.sessiond.take();
                if self.workspace_restore_failed {
                    self.workspace = Workspace::mvp();
                    self.restoring_workspace = false;
                    self.workspace_restore_failed = false;
                    self.persistence_ready = true;
                    self.persist_workspace();
                } else {
                    prepare_workspace_for_runtime_reload(&mut self.workspace);
                    self.persist_workspace();
                }
                self.pending_terminal_spawns.clear();
                self.sessions.clear();
                self.agent_sessions.clear();
                self.panes.clear();
                self.last_focused_terminal = None;
                cx.notify();
                self.reload_session_runtime(old, cx);
            }
        }
    }

    /// `execute` for control-plane callers — public without exposing the
    /// whole command surface.
    pub(crate) fn execute_external(
        &mut self,
        id: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.execute(id, window, cx);
    }

    fn open_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.freeze_scrim_before_modal_exit();
        self.workspace.exit_workspace_mode();
        let summaries = self.workspace.session_summaries();
        let list = cx.new(|cx| {
            let mut list =
                ListState::new(SessionManagerDelegate::new(summaries), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let (summary, secondary) = {
                        let delegate = list.read(cx).delegate();
                        (
                            delegate.summary_at(*index).cloned(),
                            delegate.last_confirm_secondary(),
                        )
                    };
                    let Some(summary) = summary else {
                        return;
                    };
                    if secondary {
                        // Secondary confirm (cmd-enter / right click)
                        // terminates the session; the modal stays open
                        // on refreshed data.
                        shell.workspace.terminate_session(summary.id);
                        ensure_workspace_has_pane(&mut shell.workspace);
                        shell.reconcile(window, cx);
                        let sessions = shell.workspace.session_summaries();
                        list.update(cx, |list, cx| {
                            list.delegate_mut().reset(sessions);
                            cx.notify();
                        });
                        return;
                    }
                    shell.close_session_manager(window, cx);
                    if summary.attached {
                        if let Some((tab, pane)) =
                            shell.workspace.pane_location_for_session(summary.id)
                        {
                            shell.workspace.activate_pane_index(tab, pane);
                        }
                    } else {
                        shell
                            .workspace
                            .attach_existing_session_to_split_activated(summary.id, true);
                    }
                    shell.reconcile(window, cx);
                    shell.focus_active(window, cx);
                }
                ListEvent::Cancel => shell.close_session_manager(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.session_manager = Some(list);
        self._session_manager_subscription = Some(subscription);
        cx.notify();
    }

    fn close_session_manager(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session_manager = None;
        self._session_manager_subscription = None;
        self.scrim_freeze = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(crate) fn session_summaries(&self) -> Vec<horizon_workspace::types::SessionSummary> {
        self.workspace.session_summaries()
    }

    /// External (control-plane) operations — the CLI's verbs, mirroring
    /// the Floem shell's `external_commands` semantics: `activate:
    /// false` never steals focus. `prompt` (agent sessions only) sends
    /// the first user message right after the session starts — the
    /// create-with-prompt composite from the CLI design. `role_id` is
    /// fixed by the caller (e.g. `new-config-agent`), never client-supplied
    /// — see `pending_roles`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn external_new_session(
        &mut self,
        kind: PaneKind,
        role_id: Option<horizon_agent::roles::RoleId>,
        split: Option<(SessionId, SplitAxis)>,
        activate: bool,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        let terminal_spawn = matches!(kind, PaneKind::Terminal)
            .then(|| self.pending_terminal_spawn(split.map(|(target, _)| target)));
        let session_id = match split {
            Some((target, axis)) => self
                .workspace
                .split_session_with_new_session(target, kind, axis, activate)
                .ok_or_else(|| "unknown split target session".to_string())?,
            None => self
                .workspace
                .open_tab_with_new_session_activated(kind, activate),
        };
        if let Some(spawn) = terminal_spawn {
            self.pending_terminal_spawns.insert(session_id, spawn);
        }
        if let Some(role_id) = role_id {
            self.pending_roles.insert(session_id, role_id);
        }
        self.reconcile(window, cx);
        if let Some(prompt) = prompt {
            if let Some(session) = self.agent_sessions.get(&session_id) {
                session.read(cx).send_user_message(prompt);
            }
        }
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }

    pub(crate) fn external_attach(
        &mut self,
        session_id: SessionId,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        self.workspace
            .attach_existing_session_to_split_activated(session_id, activate)
            .ok_or_else(|| "unknown session".to_string())?;
        self.reconcile(window, cx);
        if activate {
            self.focus_active(window, cx);
        }
        Ok(())
    }

    pub(crate) fn external_terminate(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.restoring_workspace {
            return Err("workspace restore is still in progress".to_string());
        }
        if !self.workspace.terminate_session(session_id) {
            return Err("unknown session".to_string());
        }
        ensure_workspace_has_pane(&mut self.workspace);
        self.reconcile(window, cx);
        Ok(())
    }

    /// Session-targeted approve/deny/cancel, for a control-plane caller
    /// that names an explicit `session_id` rather than "whichever pane is
    /// active" (unlike `CommandId::ApproveToolCall`/`DenyToolCall`/
    /// `CancelAgentTurn`, which resolve against `active_agent_session`).
    pub(crate) fn external_approve(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).approve(call_id);
        Ok(())
    }

    pub(crate) fn external_deny(
        &mut self,
        session_id: SessionId,
        call_id: horizon_agent::contract::ToolCallId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).deny(call_id);
        Ok(())
    }

    pub(crate) fn external_cancel(
        &mut self,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let session = self
            .agent_sessions
            .get(&session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        session.read(cx).cancel();
        Ok(())
    }

    pub(crate) fn external_terminate_all_detached(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.restoring_workspace {
            return;
        }
        for summary in self.workspace.detached_session_summaries() {
            self.workspace.terminate_session(summary.id);
        }
        self.reconcile(window, cx);
    }

    pub(crate) fn command_state_with(&self, cx: &App) -> CommandState {
        let (has_pending_approval, has_turn_in_flight) = self
            .active_agent_session()
            .map(|session| {
                let session = session.read(cx);
                let pending = !horizon_agent::frame::actionable_pending_approval_call_ids_in(
                    &session.frame.items,
                )
                .is_empty();
                let in_flight = matches!(
                    session.frame.state,
                    Some(horizon_agent::contract::SessionState::Running)
                        | Some(horizon_agent::contract::SessionState::ToolRunning)
                );
                (pending, in_flight)
            })
            .unwrap_or((false, false));
        CommandState {
            tab_count: self.workspace.tab_count(),
            visible_pane_count: self.workspace.visible_panes().len(),
            has_active_session: self.workspace.active_session_id().is_some(),
            detached_session_count: self.workspace.detached_session_count(),
            has_pending_approval,
            has_turn_in_flight,
        }
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.freeze_scrim_before_modal_exit();
        self.workspace.exit_workspace_mode();
        let entries = command_entries(self.command_state_with(cx));
        let list = cx.new(|cx| {
            let mut list =
                ListState::new(PaletteDelegate::new(entries), window, cx).searchable(true);
            select_first_row_on_open(&mut list, window, cx);
            list
        });
        let subscription = cx.subscribe_in(
            &list,
            window,
            |shell, list, event: &ListEvent, window, cx| match event {
                ListEvent::Confirm(index) => {
                    let entry = list.read(cx).delegate().entry_at(*index).cloned();
                    shell.close_palette(window, cx);
                    if let Some(entry) = entry.filter(|entry| entry.enabled) {
                        shell.execute(entry.spec.id, window, cx);
                    }
                }
                ListEvent::Cancel => shell.close_palette(window, cx),
                ListEvent::Select(_) => {}
            },
        );
        window.focus(&list.focus_handle(cx), cx);
        self.palette = Some(list);
        self._palette_subscription = Some(subscription);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self._palette_subscription = None;
        self.scrim_freeze = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        // The model detaches the session; in M2 the view (and its PTY)
        // simply drops with it — close-vs-terminate parity needs the M3
        // Registry.
        self.workspace.close_active_pane();
        self.reconcile(window, cx);
        self.focus_active(window, cx);
    }

    fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        let count = self.workspace.tab_count();
        if count > 1 {
            let next = (self.workspace.active_tab_index() + 1) % count;
            self.workspace.activate_tab_index(next);
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.exit_workspace_mode();
        self.workspace.activate_tab_index(index);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn activate_pane(&mut self, pane_id: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        self.workspace.activate_pane(pane_id);
        self.sync_terminal_focus(window, cx);
        self.persist_workspace();
        cx.notify();
    }

    fn render_tab_strip(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.workspace.tab_summaries();
        let tab_count = tabs.len();
        // `Tab`/`TabBar` colors come from `cx.theme()`, which
        // `theme::apply_gpui_component_theme` projects from Horizon's own
        // `[theme]` scheme (see `src/theme.rs`) -- so the label text and
        // the selected tab's pill already resolve to `tab_foreground`/
        // `tab_active_foreground`/`background` without any per-tab
        // override here. `Segmented` (replacing `Underline`, 2026-07-14
        // owner GO) is one of gpui-component's variants with an animated
        // sliding selection indicator; its track color is
        // `tab_bar_segmented`, which Horizon's projection leaves unset --
        // falling back to gpui-component's own `secondary` token, i.e.
        // `scheme.surface_panel` (see `gpui_component_theme_config`'s doc
        // table). The selected tab's own pill is `tokens.background`
        // (`scheme.background`), fixed inside gpui-component and not
        // separately overridable without changing every other
        // `background`-rooted surface in the app.
        let selected_index = tabs
            .iter()
            .find(|tab| tab.active)
            .map_or(0, |tab| tab.index);
        let strip_width = window.viewport_size().width;
        TabBar::new("workspace-tabs")
            .segmented()
            .w_full()
            .px_2()
            .selected_index(selected_index)
            .on_click(cx.listener(|shell, index: &usize, window, cx| {
                shell.activate_tab(*index, window, cx);
            }))
            .children(tabs.into_iter().map(|tab| {
                let label = Tab::new()
                    .label(format!("{} {}", tab.index + 1, tab.title))
                    // gpui-component's `Tab` already clips overflowing
                    // content (`overflow_hidden()`/`whitespace_nowrap()`
                    // on its inner label row, verified in the vendored
                    // `tab.rs`) but never marks the clip with an ellipsis;
                    // add that so a long title reads as truncated rather
                    // than cut off mid-character. `Tab: Styled` proxies
                    // straight into the same inner `div` its own render
                    // keeps building on, and nothing later in that render
                    // touches `text_overflow`, so this survives.
                    .text_ellipsis();
                if EQUAL_WIDTH_TABS {
                    // `Tab`'s own `Styled` impl mutates the same `div`
                    // its `RenderOnce::render` finishes building, so a
                    // `.flex_1()` set here *would* survive that render's
                    // later `.flex_shrink_0()` call (which only clobbers
                    // the shrink field, not grow/basis) -- except
                    // `TabBar` wraps every child in its own untouchable
                    // bounds-tracking `div` whenever the variant animates
                    // a selection indicator (`Segmented`, `Pill`, and
                    // `Underline` all qualify -- `tab_bar.rs`'s
                    // `has_indicator`), and *that* wrapper, not our
                    // `Tab`, is the actual flex item laid out in the tab
                    // row. Its own style is fixed (`flex_shrink_0()`, no
                    // grow, no exposed hook to inject one), so
                    // `Tab::flex_1()` never reaches the row's layout.
                    // `Tab::render` never sets its own width, though, so
                    // an explicit pixel width set here *does* survive --
                    // hence sizing from the strip's own measured
                    // viewport width instead of a flex trick.
                    label.w(equal_tab_width(strip_width, tab_count))
                } else {
                    label
                }
            }))
    }

    fn render_node(
        &self,
        tab_id: TabId,
        node: &LayoutNode,
        path: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match node {
            LayoutNode::Pane(pane_id) => {
                let pane_id = *pane_id;
                // The workspace-mode dim/cursor pattern: the live state
                // normally, but while a control-surface modal is open
                // that state has already gone inactive (every
                // modal-opening handler exits the mode first -- see
                // `effective_scrim_pattern`'s doc comment) -- substitute
                // the pattern frozen at modal-open time instead, so both
                // the scrim and the cursor-pane border stay exactly what
                // they were right before the modal opened (2026-07-15
                // round 3: modal-open is fully chrome-neutral, not just
                // scrim-neutral).
                let modal_open = self.palette.is_some()
                    || self.view_chooser.is_some()
                    || self.session_manager.is_some();
                let (mode_active, cursor_pane) = effective_scrim_pattern(
                    modal_open,
                    self.scrim_freeze,
                    self.workspace.is_workspace_mode_active(),
                    self.workspace.cursor_pane_id(),
                );
                let is_cursor = mode_active && cursor_pane == Some(pane_id);
                let is_active = self.workspace.is_active_pane(pane_id);
                let scrim_alpha = pane_scrim_alpha(mode_active);
                let border_role = pane_border_role(is_cursor, is_active);
                let border: Hsla = match border_role {
                    PaneBorderRole::Cursor => theme::accent(),
                    PaneBorderRole::Active => theme::border(),
                    PaneBorderRole::Inactive => rgb(theme::background()).into(),
                };
                let view = self.panes.get(&pane_id).cloned();
                let restoring = self.restoring_workspace && view.is_none();
                let restore_label = if self.workspace_restore_failed {
                    "Workspace restore failed"
                } else {
                    "Restoring session..."
                };
                div()
                    .size_full()
                    // 2px, uniformly across every border role -- widened
                    // from 1px (2026-07-15 round 3: the 1px accent border
                    // was nearly invisible). Kept identical across roles
                    // deliberately, so a pane's box never resizes as the
                    // cursor moves; only `border` (`pane_border_role`,
                    // above) changes.
                    .border_2()
                    .border_color(border)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |shell, _, window, cx| {
                            shell.activate_pane(pane_id, window, cx)
                        }),
                    )
                    .children(view.map(|view| view.element()))
                    .when(restoring, |this| {
                        this.flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.0))
                            .text_color(theme::text_muted())
                            .child(restore_label)
                    })
                    .when_some(scrim_alpha, |this, alpha| {
                        // Pane-dimming scrim
                        // (`docs/workspace-mode-design.md`,
                        // `docs/theme-design.md`'s scrim section): drawn
                        // whenever `pane_scrim_alpha` says workspace
                        // mode's dim pattern is active -- every pane
                        // uniformly, cursor pane included (2026-07-15
                        // round 3: the cursor/non-cursor distinction moved
                        // entirely to the border above, via
                        // `pane_border_role`). Opening a control-surface
                        // modal is chrome-*neutral*: the pattern visible
                        // right before the modal opened persists unchanged
                        // while it's open (see `mode_active`/`cursor_pane`
                        // above, from `effective_scrim_pattern`). A plain
                        // non-interactive overlay — no `.occlude()` — so
                        // it stays purely visual: the
                        // pane-activation `on_mouse_down` above keeps
                        // firing through it, and so does a modal
                        // backdrop's own click-to-close, since that
                        // backdrop renders as a later sibling of the
                        // whole pane tree (see the `when_some(self.palette
                        // ...)` etc. below) and so paints and hit-tests
                        // above every pane's scrim. The scrim's color is
                        // `theme::scrim_color()` -- the resolved
                        // `background`, composited at `alpha` over the
                        // pane -- so de-emphasis reduces the pane's
                        // *contrast* (every pixel compresses proportionally
                        // toward `background`) rather than shifting its
                        // lightness the way a black/white pole-color veil
                        // would (`docs/theme-design.md`'s scrim section:
                        // tried as a pole scrim 2026-07-15, withdrawn
                        // 2026-07-16 back to this).
                        this.child(
                            div()
                                .absolute()
                                .inset_0()
                                .bg(theme::scrim_color().opacity(alpha)),
                        )
                    })
                    .when(border_role == PaneBorderRole::Cursor, |this| {
                        // Cursor-pane inner glow (round 5, 2026-07-16):
                        // a soft accent-colored inset shadow, layered
                        // with (not replacing) the 2px accent border
                        // above. A separate later child, not a
                        // `box_shadow` on this outer div directly, so it
                        // paints *after* the scrim overlay above (gpui
                        // paints `inset: true` shadows before an
                        // element's own children, so a shadow set on
                        // this outer div would land *underneath* the
                        // scrim child and get washed out by it -- the
                        // cursor role only ever applies while workspace
                        // mode's dim pattern is active, so that's not a
                        // corner case, it's the only case). The pane's
                        // own border, painted last regardless (after
                        // every child), still ends up crisply on top of
                        // both. Non-interactive like the scrim overlay --
                        // no `.occlude()`, so pane-activation clicks keep
                        // passing through. See `CURSOR_GLOW_BLUR_PX`'s
                        // doc comment for the `inset: true` verification.
                        this.child(div().absolute().inset_0().shadow(vec![BoxShadow {
                            color: theme::accent().opacity(CURSOR_GLOW_ALPHA),
                            offset: point(px(0.0), px(0.0)),
                            blur_radius: px(CURSOR_GLOW_BLUR_PX),
                            spread_radius: px(0.0),
                            inset: true,
                        }]))
                    })
                    .into_any_element()
            }
            LayoutNode::Split { axis, children } => {
                let mut group: ResizablePanelGroup = match axis {
                    SplitAxis::Horizontal => h_resizable(SharedString::from(path.clone())),
                    SplitAxis::Vertical => v_resizable(SharedString::from(path.clone())),
                };
                let child_anchors = children
                    .iter()
                    .filter_map(|child| child.node.first_pane())
                    .collect::<Vec<_>>();
                let split_anchor = child_anchors[0];
                let weak_shell = cx.entity().downgrade();
                let resize_anchors = child_anchors.clone();
                group = group.on_resize(move |state, _, cx| {
                    let sizes = state
                        .read(cx)
                        .sizes()
                        .iter()
                        .map(|size| size.as_f32())
                        .collect::<Vec<_>>();
                    let _ = weak_shell.update(cx, |shell, cx| {
                        if shell.restoring_workspace {
                            return;
                        }
                        if shell.workspace.set_split_weights(
                            tab_id,
                            split_anchor,
                            &resize_anchors,
                            &sizes,
                        ) {
                            shell.persist_workspace();
                            cx.notify();
                        }
                    });
                });
                let total_weight = children.iter().map(|child| child.weight).sum::<f32>();
                let sibling_count = children.len();
                for (index, child) in children.iter().enumerate() {
                    let child_element =
                        self.render_node(tab_id, &child.node, format!("{path}-{index}"), cx);
                    let basis = px(child.weight / total_weight * 1_000.0);
                    // Pull this child's own rendered content back from
                    // every edge that faces a split boundary, so its
                    // pane border(s) never fall under gpui-component's
                    // resize-handle divider -- see
                    // `SPLIT_BOUNDARY_INSET_PX`'s doc comment for the
                    // root cause. Applies to the whole `child_element`
                    // subtree, not a specific leaf pane: for a nested
                    // split, that subtree's own outer edge is exactly
                    // its own first/last leaf pane's edge, so the inset
                    // still lands on the right pane recursively.
                    let (leading_inset, trailing_inset) = split_child_insets(index, sibling_count);
                    let inset_px = px(SPLIT_BOUNDARY_INSET_PX);
                    let inset_child = div()
                        .size_full()
                        .when(leading_inset, |this| match axis {
                            SplitAxis::Horizontal => this.pl(inset_px),
                            SplitAxis::Vertical => this.pt(inset_px),
                        })
                        .when(trailing_inset, |this| match axis {
                            SplitAxis::Horizontal => this.pr(inset_px),
                            SplitAxis::Vertical => this.pb(inset_px),
                        })
                        .child(child_element);
                    group = group.child(resizable_panel().flex_basis(basis).child(inset_child));
                }
                group.into_any_element()
            }
        }
    }
}

impl Render for WorkspaceShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode_active = self.workspace.is_workspace_mode_active();
        let content = self
            .workspace
            .active_tab()
            .map(|tab| (tab.id, tab.root.clone()))
            .map(|(tab_id, root)| self.render_node(tab_id, &root, format!("{tab_id:?}-root"), cx));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::background()))
            .key_context(if mode_active {
                MODE_CONTEXT
            } else {
                "Workspace"
            })
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|shell, _: &ToggleWorkspaceMode, window, cx| {
                shell.toggle_mode(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveLeft, _, cx| {
                shell.mode_move(Direction::Left, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveDown, _, cx| {
                shell.mode_move(Direction::Down, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveUp, _, cx| {
                shell.mode_move(Direction::Up, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeMoveRight, _, cx| {
                shell.mode_move(Direction::Right, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeCommit, window, cx| {
                shell.mode_commit(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ModeCancel, window, cx| {
                shell.mode_cancel(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NewTab, window, cx| {
                shell.execute(CommandId::NewTab, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NewAgentTab, window, cx| {
                shell.create_session(PaneKind::Agent, None, Placement::NewTab, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &SplitPane, window, cx| {
                shell.execute(CommandId::SplitRight, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ClosePane, window, cx| {
                shell.execute(CommandId::CloseActivePane, window, cx);
            }))
            .on_action(cx.listener(|shell, _: &NextTab, window, cx| {
                shell.next_tab(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &OpenPalette, window, cx| {
                shell.open_palette(window, cx);
            }))
            .on_action(cx.listener(|shell, action: &RunCommand, window, cx| {
                shell.execute(action.id, window, cx);
            }))
            .child(self.render_tab_strip(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .children(content)
                    .when_some(self.palette.clone(), |this, palette| {
                        this.child(
                            div()
                                .id("palette-backdrop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .justify_center()
                                .items_start()
                                .pt(px(64.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.close_palette(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(560.0))
                                        .h(px(400.0))
                                        .bg(rgb(theme::background()))
                                        .border_1()
                                        .border_color(theme::border())
                                        .shadow(theme::overlay_shadow())
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&palette)),
                                ),
                        )
                    })
                    .when_some(self.view_chooser.clone(), |this, chooser| {
                        this.child(
                            div()
                                .id("view-chooser-backdrop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .justify_center()
                                .items_start()
                                .pt(px(64.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.pending_placement = None;
                                        shell.close_view_chooser(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(420.0))
                                        .h(px(220.0))
                                        .bg(rgb(theme::background()))
                                        .border_1()
                                        .border_color(theme::border())
                                        .shadow(theme::overlay_shadow())
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&chooser)),
                                ),
                        )
                    })
                    .when_some(self.session_manager.clone(), |this, manager| {
                        this.child(
                            div()
                                .id("session-manager-backdrop")
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full()
                                .flex()
                                .justify_center()
                                .items_start()
                                .pt(px(64.0))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|shell, _, window, cx| {
                                        shell.close_session_manager(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(560.0))
                                        .h(px(400.0))
                                        .bg(rgb(theme::background()))
                                        .border_1()
                                        .border_color(theme::border())
                                        .shadow(theme::overlay_shadow())
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&manager)),
                                ),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        command_blocked_by_restore, effective_scrim_pattern, ensure_workspace_has_pane,
        equal_tab_width, first_row_to_select, load_workspace_state, pane_border_role,
        pane_scrim_alpha, prepare_workspace_for_runtime_reload, split_child_insets,
        terminal_fallback_cwd, terminal_resume_candidates, terminal_spawn_source,
        workspace_mode_blocked_by_restore, PaneBorderRole, SCRIM_DIM_ALPHA,
    };
    use gpui::px;
    use gpui_component::IndexPath;
    use horizon_terminal_core::TerminalSummary;
    use horizon_workspace::commands::CommandId;
    use horizon_workspace::{PaneId, PaneKind, SessionId, SessionKind, Workspace};

    use crate::workspace_state::WorkspaceStateStore;

    fn state_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "horizon-workspace-shell-{label}-{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn equal_tab_width_divides_the_strip_evenly_after_chrome_and_gaps() {
        // 824px strip, 4 tabs: 24px chrome allowance + 3 gaps * 2px = 30px
        // reserved, leaving 794px split four ways.
        assert_eq!(equal_tab_width(px(824.0), 4), px(198.5));
    }

    #[test]
    fn equal_tab_width_is_zero_for_no_tabs() {
        assert_eq!(equal_tab_width(px(800.0), 0), px(0.0));
    }

    #[test]
    fn equal_tab_width_never_drops_below_the_floor() {
        // A narrow window with many tabs: the even split (5.8px) would be
        // unreadable, so the floor wins -- the strip overflows into
        // `tabs-inner`'s existing `overflow_x_scroll()` instead, same as
        // content-sized tabs already do when they don't fit.
        assert_eq!(equal_tab_width(px(100.0), 10), px(40.0));
    }

    #[test]
    fn split_child_insets_the_first_child_only_on_its_trailing_edge() {
        // The first child of a split has no neighbor to its left/top, so
        // nothing reaches into its leading edge -- only its trailing edge
        // (touched by its own resize handle reaching backward from the
        // next child) needs the pullback.
        assert_eq!(split_child_insets(0, 3), (false, true));
    }

    #[test]
    fn split_child_insets_the_last_child_only_on_its_leading_edge() {
        // The last child owns a resize handle (every child but the first
        // does) reaching backward into its own leading edge, but has no
        // neighbor after it to be reached into on its trailing edge.
        assert_eq!(split_child_insets(2, 3), (true, false));
    }

    #[test]
    fn split_child_insets_a_middle_child_on_both_edges() {
        assert_eq!(split_child_insets(1, 3), (true, true));
    }

    #[test]
    fn split_child_insets_the_sole_child_of_a_single_pane_split_on_neither_edge() {
        // Degenerate case: a "split" with exactly one child (shouldn't
        // occur in practice, but the pure function should still answer
        // sanely) has no boundary at all.
        assert_eq!(split_child_insets(0, 1), (false, false));
    }

    #[test]
    fn pane_scrim_alpha_is_none_when_the_dim_pattern_is_inactive() {
        assert_eq!(pane_scrim_alpha(false), None);
    }

    #[test]
    fn pane_scrim_alpha_dims_uniformly_when_the_dim_pattern_is_active() {
        // 2026-07-15 round-3 feedback: no more cursor/non-cursor
        // distinction in the scrim itself -- every pane, cursor pane
        // included, gets the same alpha. The cursor signal moved entirely
        // to `pane_border_role`.
        assert_eq!(pane_scrim_alpha(true), Some(SCRIM_DIM_ALPHA));
    }

    #[test]
    fn effective_scrim_pattern_uses_the_live_state_when_no_modal_is_open() {
        let cursor = PaneId::new();
        assert_eq!(
            effective_scrim_pattern(false, None, true, Some(cursor)),
            (true, Some(cursor))
        );
        // A stale freeze left over from an already-closed modal must be
        // ignored once no modal is open.
        assert_eq!(
            effective_scrim_pattern(false, Some(PaneId::new()), false, None),
            (false, None)
        );
    }

    #[test]
    fn effective_scrim_pattern_substitutes_the_freeze_while_a_modal_is_open() {
        let frozen_cursor = PaneId::new();
        // The live state has already gone inactive by the time this
        // renders (every modal-opening handler exits workspace mode
        // first), so it must be ignored in favor of the freeze.
        assert_eq!(
            effective_scrim_pattern(true, Some(frozen_cursor), false, None),
            (true, Some(frozen_cursor))
        );
    }

    #[test]
    fn effective_scrim_pattern_is_inactive_when_the_modal_opened_outside_workspace_mode() {
        // Direct `ctrl+p` from outside workspace mode: the freeze is
        // `None` (nothing was active when the modal opened), so the
        // pattern stays inactive regardless of what the live state
        // happens to read.
        assert_eq!(
            effective_scrim_pattern(true, None, true, Some(PaneId::new())),
            (false, None)
        );
    }

    #[test]
    fn pane_border_role_prioritizes_the_cursor_pane_over_active() {
        assert_eq!(pane_border_role(true, true), PaneBorderRole::Cursor);
        // The workspace-mode cursor can sit on a pane that isn't the
        // tab's own focused pane (moved without committing) -- the cursor
        // role still wins.
        assert_eq!(pane_border_role(true, false), PaneBorderRole::Cursor);
    }

    #[test]
    fn pane_border_role_falls_back_to_active_then_inactive() {
        assert_eq!(pane_border_role(false, true), PaneBorderRole::Active);
        assert_eq!(pane_border_role(false, false), PaneBorderRole::Inactive);
    }

    #[test]
    fn first_row_to_select_is_the_default_index_when_the_list_is_nonempty() {
        assert_eq!(first_row_to_select(1), Some(IndexPath::default()));
        assert_eq!(first_row_to_select(5), Some(IndexPath::default()));
    }

    #[test]
    fn first_row_to_select_is_none_when_the_list_is_empty() {
        assert_eq!(first_row_to_select(0), None);
    }

    #[test]
    fn explicit_split_target_wins_as_terminal_spawn_source() {
        let explicit = SessionId::new();
        let active = SessionId::new();
        assert_eq!(
            terminal_spawn_source(Some(explicit), Some(active)),
            Some(explicit)
        );
        assert_eq!(terminal_spawn_source(None, Some(active)), Some(active));
    }

    #[test]
    fn terminal_fallback_prefers_current_dir_then_home_then_dot() {
        let cwd = std::path::PathBuf::from("/workspace");
        let home = std::path::PathBuf::from("/home/test");
        assert_eq!(
            terminal_fallback_cwd(Some(cwd.clone()), Some(home.clone())),
            cwd
        );
        assert_eq!(terminal_fallback_cwd(None, Some(home.clone())), home);
        assert_eq!(
            terminal_fallback_cwd(None, None),
            std::path::PathBuf::from(".")
        );
    }

    #[test]
    fn terminal_resume_candidates_exclude_known_cross_kind_ids_and_duplicates() {
        let fresh_terminal = SessionId::new();
        let known_agent = SessionId::new();
        let first_survivor = SessionId::new();
        let second_survivor = SessionId::new();
        let known = [fresh_terminal, known_agent].into_iter().collect();
        let summaries = [
            fresh_terminal,
            first_survivor,
            known_agent,
            first_survivor,
            second_survivor,
        ]
        .into_iter()
        .map(|id| TerminalSummary {
            session_id: id.as_uuid(),
        })
        .collect();

        assert_eq!(
            terminal_resume_candidates(summaries, &known),
            vec![first_survivor.as_uuid(), second_survivor.as_uuid()]
        );
    }

    #[test]
    fn missing_workspace_state_starts_fresh_and_enables_persistence() {
        let path = state_path("missing");
        let mut store = WorkspaceStateStore::new(path);
        let (workspace, restoring, persistence_ready) = load_workspace_state(&mut store);

        assert_eq!(workspace.tab_count(), 1);
        assert!(!restoring);
        assert!(persistence_ready);
    }

    #[test]
    fn valid_workspace_state_enters_the_restore_barrier() {
        let path = state_path("valid");
        let source = Workspace::mvp();
        let json = source.to_persisted_json().unwrap();
        let mut store = WorkspaceStateStore::new(path.clone());
        store.save(&json).unwrap();

        let (workspace, restoring, persistence_ready) = load_workspace_state(&mut store);

        assert_eq!(workspace.to_persisted_json().unwrap(), json);
        assert!(restoring);
        assert!(!persistence_ready);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_workspace_state_is_never_overwritten() {
        let path = state_path("newer");
        let contents = r#"{"version":999}"#;
        std::fs::write(&path, contents).unwrap();
        let mut store = WorkspaceStateStore::new(path.clone());

        let (_, restoring, persistence_ready) = load_workspace_state(&mut store);

        assert!(!restoring);
        assert!(!persistence_ready);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reload_prep_removes_terminals_but_retains_agent_model_and_pane() {
        let mut workspace = Workspace::mvp();
        let agent_id = workspace.open_tab_with_new_session_activated(PaneKind::Agent, true);
        assert!(workspace.pane_location_for_session(agent_id).is_some());

        prepare_workspace_for_runtime_reload(&mut workspace);

        let summaries = workspace.session_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, agent_id);
        assert_eq!(summaries[0].kind, SessionKind::Agent);
        assert!(workspace.pane_location_for_session(agent_id).is_some());
    }

    #[test]
    fn runtime_reload_reseeds_a_terminal_when_no_pane_survives() {
        let mut workspace = Workspace::mvp();
        prepare_workspace_for_runtime_reload(&mut workspace);
        assert_eq!(workspace.tab_count(), 0);

        let session_id = ensure_workspace_has_pane(&mut workspace).expect("fresh terminal");

        assert_eq!(workspace.active_session_id(), Some(session_id));
        assert_eq!(
            workspace.session_pane_kind(session_id),
            Some(PaneKind::Terminal)
        );
    }

    // `WorkspaceShell::handle_terminal_exited` (the receiving end of every
    // `TerminalSession`'s `exit_tx`) is itself GPUI-entity-shaped and not
    // unit-testable without a window, but its model-level steps --
    // `Workspace::terminate_session` then `ensure_workspace_has_pane` -- are
    // the same pure building blocks this module already tests elsewhere
    // (e.g. `runtime_reload_reseeds_a_terminal_when_no_pane_survives`
    // above). The two tests below exercise exactly that sequence, standing
    // in for an end-to-end exit-notification test.

    #[test]
    fn terminate_session_removes_it_whether_attached_or_detached() {
        // Decision 1: a PTY exit terminates its workspace session --
        // `handle_terminal_exited` calls `terminate_session` for whatever
        // session id the exit notification names, whether that session is
        // still attached to a pane or already sitting detached (a
        // session-manager entry that outlived its pane). Both must be
        // removed from the model.
        let mut workspace = Workspace::mvp();
        let attached = workspace.active_terminal_session_id().expect("session");
        let detached = SessionId::new();
        workspace.register_detached_session(PaneKind::Terminal, detached);
        assert!(!workspace.session_is_referenced(detached));

        assert!(workspace.terminate_session(attached));
        assert!(workspace.terminate_session(detached));

        assert!(workspace.session_summaries().is_empty());
    }

    #[test]
    fn ensure_workspace_has_pane_recovers_persistability_after_terminating_the_last_session() {
        // Owner item 2's root cause: `WorkspaceState::validate` rejects a
        // workspace with zero tabs, so `to_persisted_json` -- called by
        // `persist_workspace` after every mutation -- started failing the
        // moment the workspace's last pane vanished (e.g. every session
        // terminated, or the last shell exiting once decision 1 wires exit
        // to terminate). `ensure_workspace_has_pane`, called right after
        // `terminate_session` on every termination path, is the guard that
        // keeps a live workspace from ever reaching that state.
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();
        assert_eq!(workspace.tab_count(), 0);
        assert!(workspace.to_persisted_json().is_err());

        let reseeded = ensure_workspace_has_pane(&mut workspace).expect("fresh terminal");

        assert_eq!(workspace.tab_count(), 1);
        assert_eq!(workspace.active_session_id(), Some(reseeded));
        assert!(workspace.to_persisted_json().is_ok());
    }

    #[test]
    fn failed_restore_allows_only_the_explicit_runtime_reload_command() {
        assert!(command_blocked_by_restore(
            true,
            false,
            CommandId::ReloadSessionRuntime
        ));
        assert!(command_blocked_by_restore(true, true, CommandId::NewTab));
        assert!(!command_blocked_by_restore(
            true,
            true,
            CommandId::ReloadSessionRuntime
        ));
        assert!(!command_blocked_by_restore(false, false, CommandId::NewTab));
    }

    #[test]
    fn failed_restore_allows_workspace_mode_to_reach_the_reload_command() {
        assert!(workspace_mode_blocked_by_restore(true, false));
        assert!(!workspace_mode_blocked_by_restore(true, true));
        assert!(!workspace_mode_blocked_by_restore(false, false));
    }
}
