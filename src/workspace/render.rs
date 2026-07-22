//! Tab-strip and pane-tree rendering: `render_tab_strip`/`render_node`,
//! the `Render` impl that wires GPUI actions to model calls, the
//! workspace-mode cursor/dim pattern's pure pane-chrome functions
//! (`pane_scrim_alpha`, `effective_scrim_pattern`, `pane_border_role`,
//! `equal_tab_width`), the pure `mode_key_context_active` (whether the
//! root's key context should be `MODE_CONTEXT` this render -- see its own
//! doc comment), the mode/tab/pane action handlers (`toggle_mode`,
//! `mode_move`, `mode_commit`, `mode_cancel`, `next_tab`, `activate_tab`,
//! `activate_pane`) that only the `Render` impl below dispatches into, and
//! the split-handle drag pipeline (`SplitDrag`, `begin_split_drag`/
//! `update_split_drag`/`end_split_drag`, the pure pairwise clamp
//! `pairwise_resize_weights` and its `effective_container_px` pixel
//! accounting) that replaces `gpui_component::resizable` -- see
//! `docs/split-resize-design.md`.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::List;
use gpui_component::tab::{Tab, TabBar};
use horizon_workspace::commands::CommandId;
use horizon_workspace::types::{LayoutNode, TabId};
use horizon_workspace::{Direction, PaneId, PaneKind, SplitAxis};

use super::{
    ClosePane, ModeCancel, ModeCommit, ModeMoveDown, ModeMoveLeft, ModeMoveRight, ModeMoveUp,
    NewAgentTab, NewTab, NextTab, OpenPalette, OpenSessionDirectory, RunCommand, SplitPane,
    TerminateSessionSubtree, ToggleWorkspaceMode, WorkspaceShell, MODE_CONTEXT,
    SESSION_MANAGER_CONTEXT,
};
use crate::theme;
use crate::view_chooser::Placement;

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

/// Visible width (or height, for a vertical-axis split) of a split's
/// resize-handle divider line -- mirrors the vendored gpui-component
/// `resizable` module's own `HANDLE_SIZE` (`docs/split-resize-design.md`).
const SPLIT_HANDLE_LINE_PX: f32 = 1.0;

/// Total clickable/draggable extent of a resize handle along its split's
/// axis: the 1px line plus a comfortable hit-area padding on each side
/// (mirrors the vendored component's `HANDLE_SIZE + 2 * HANDLE_PADDING`,
/// i.e. `1 + 2*4`), so the handle is easy to grab without widening what's
/// visibly drawn.
const SPLIT_HANDLE_HIT_PX: f32 = 9.0;

/// Display-side floor for a split child's size along its split's axis --
/// replaces gpui-component's `PANEL_MIN_SIZE` (same 100px value). Weights
/// are untouched when the *window* shrinks past this (flex clamps the
/// display only, via `min_w`/`min_h` below); ratios restore exactly when
/// it grows back. See `docs/split-resize-design.md`'s Decision section.
const SPLIT_PANEL_MIN_SIZE_PX: f32 = 100.0;

/// `Interactivity::group`/`group_hover` name shared by every split
/// handle: scoped by nearest ancestor (the same Tailwind-style scheme the
/// vendored `resize_handle.rs` itself uses with a single hardcoded name),
/// so reusing one literal across every handle in the tree is safe -- a
/// sibling handle's hover never lights up another handle's line.
const SPLIT_HANDLE_GROUP: &str = "split-handle";

/// Live state for an in-progress split-handle drag (`docs/split-resize-
/// design.md`'s Drag semantics): pairwise, live-reflowing, persisted once
/// on release. Set by a handle's own `on_mouse_down`
/// ([`WorkspaceShell::begin_split_drag`]), read and applied by the split
/// container's `on_mouse_move` ([`WorkspaceShell::update_split_drag`]),
/// and cleared by its `on_mouse_up`/`on_mouse_up_out`
/// ([`WorkspaceShell::end_split_drag`]). Purely view-side scratch state,
/// held on `WorkspaceShell` itself (not GPUI element state, not the
/// model) -- every actual mutation still goes through
/// `Workspace::set_split_weights`, so `horizon_workspace` never sees a
/// pixel.
#[derive(Clone)]
pub(super) struct SplitDrag {
    tab_id: TabId,
    split_anchor: PaneId,
    child_anchors: Vec<PaneId>,
    /// The handle's leading child index among `child_anchors`; the pair
    /// being resized is `(pair_index, pair_index + 1)` -- pairwise, no
    /// cascade onto other siblings.
    pair_index: usize,
    /// Every child's weight at drag start. `Workspace::set_split_weights`
    /// sets the whole vector at once, so unchanged siblings are re-sent
    /// at their original value -- their *relative* shares stay put once
    /// the vector is renormalized.
    start_weights: Vec<f32>,
    /// Mouse position along `axis`, in pixels, at drag start. Deltas are
    /// measured from this fixed point for the whole drag (not
    /// incrementally frame-to-frame), so there's no accumulation drift.
    start_pos_px: f32,
    /// Whether any move during this drag actually changed a weight (a
    /// `Workspace::set_split_weights` call returned `true`). An unmoved
    /// click -- mouse-down immediately followed by mouse-up with no move
    /// in between -- shouldn't persist on release; see
    /// `end_active_split_drag`.
    applied: bool,
}

/// The component of `position` along `axis` -- horizontal splits (a row of
/// side-by-side children) resize along x, vertical splits (a stack) along
/// y.
fn axis_position(axis: SplitAxis, position: Point<Pixels>) -> f32 {
    match axis {
        SplitAxis::Horizontal => f32::from(position.x),
        SplitAxis::Vertical => f32::from(position.y),
    }
}

/// The component of `size` along `axis` -- the split container's own
/// pixel length in the direction handles actually resize along.
fn axis_extent(axis: SplitAxis, size: Size<Pixels>) -> f32 {
    match axis {
        SplitAxis::Horizontal => f32::from(size.width),
        SplitAxis::Vertical => f32::from(size.height),
    }
}

/// The pixel length actually available for weight-proportional
/// distribution among a split's children: the container's own measured
/// length minus every resize handle's fixed footprint. Child wrappers use
/// `flex_basis(0) + flex_grow(weight)` (see `render_node`'s `LayoutNode::
/// Split` arm), so *all* of this length divides by weight ratio -- a
/// handle is a separate `flex_grow_0`/`flex_shrink_0` sibling with an
/// explicit `SPLIT_HANDLE_HIT_PX` width, so it never participates in that
/// distribution and its pixels must come out of the denominator before
/// converting a drag's pixel delta to a weight delta (`pairwise_resize_
/// weights`'s `container_px`) -- otherwise both drag tracking and the
/// floor land at a slightly wrong pixel size. `child_count` children have
/// `child_count - 1` handles between them.
fn effective_container_px(container_px: f32, child_count: usize) -> f32 {
    let handles = child_count.saturating_sub(1) as f32;
    (container_px - handles * SPLIT_HANDLE_HIT_PX).max(0.0)
}

/// The pairwise-clamped resize computation for one split-handle drag
/// (`docs/split-resize-design.md`'s Drag semantics): converts a pixel
/// delta into a weight transfer strictly between two adjacent children,
/// hard-stopping at `floor_px` (translated into the same weight units)
/// rather than cascading into other siblings. `total_weight` is the
/// split's full weight sum at drag start, used only to convert between
/// pixel and weight units; `container_px` is the pixel length that
/// actually divides by weight ratio -- callers pass
/// [`effective_container_px`]'s result, not the container's raw measured
/// length, or both drag tracking and the floor land at the wrong pixel
/// size (see that function's doc comment). Returns the pair's two new
/// weights, still summing to `weight_a + weight_b` -- callers renormalize
/// the whole child vector via `Workspace::set_split_weights`.
fn pairwise_resize_weights(
    weight_a: f32,
    weight_b: f32,
    total_weight: f32,
    container_px: f32,
    delta_px: f32,
    floor_px: f32,
) -> (f32, f32) {
    if container_px <= 0.0 || total_weight <= 0.0 {
        return (weight_a, weight_b);
    }
    let pair_total = weight_a + weight_b;
    let floor_weight = floor_px / container_px * total_weight;
    let (lo, hi) = if pair_total >= 2.0 * floor_weight {
        (floor_weight, pair_total - floor_weight)
    } else {
        // Both floors can't be satisfied at once (e.g. a narrow window
        // with many panes already below floor) -- split the difference
        // rather than handing `clamp` an inverted (min > max) range.
        (pair_total / 2.0, pair_total / 2.0)
    };
    let delta_weight = delta_px / container_px * total_weight;
    let new_a = (weight_a + delta_weight).clamp(lo, hi);
    (new_a, pair_total - new_a)
}

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

fn workspace_mode_blocked_by_restore(restoring: bool, failed: bool) -> bool {
    restoring && !failed
}

/// Whether the shell root's key context should be [`MODE_CONTEXT`] this
/// render. `is_workspace_mode_active` is `Workspace::
/// is_workspace_mode_active`'s own answer -- `true` either because the
/// mode was explicitly toggled on, or, unconditionally, because the
/// workspace has zero tabs (see that method's doc comment for the
/// 2026-07-19 "empty workspace is an implicit command surface" decision).
/// `modal_open` suppresses it regardless: a control-surface modal already
/// exits workspace mode for a non-empty workspace (every modal-opening
/// handler calls `Workspace::exit_workspace_mode` first), but the
/// zero-tab bypass would otherwise survive that exit and keep reporting
/// active -- letting the mode's own fixed hjkl/Enter/Escape bindings
/// compete with the modal's typed search/confirm keys instead of
/// reaching the modal's own `List` context. Same hazard
/// `effective_scrim_pattern` already freezes against on the scrim/border
/// side; this is the key-dispatch-side counterpart. Pure and unit-tested
/// so the combination is covered without a GPUI render.
fn mode_key_context_active(is_workspace_mode_active: bool, modal_open: bool) -> bool {
    is_workspace_mode_active && !modal_open
}

impl WorkspaceShell {
    /// Whether any control-surface modal (palette, view chooser, session
    /// manager) currently has the shell's attention -- shared by
    /// [`mode_key_context_active`]'s caller below and `render_node`'s
    /// scrim/border freeze logic (`effective_scrim_pattern`), both of
    /// which must treat "a modal is open" identically.
    fn any_modal_open(&self) -> bool {
        self.palette.is_some()
            || self.view_chooser.is_some()
            || self.session_manager.is_some()
            || self.markdown_open.is_some()
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

    fn render_node(&self, tab_id: TabId, node: &LayoutNode, cx: &mut Context<Self>) -> AnyElement {
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
                let (mode_active, cursor_pane) = effective_scrim_pattern(
                    self.any_modal_open(),
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
                let axis = *axis;
                let total_weight = children.iter().map(|child| child.weight).sum::<f32>();
                let start_weights = children
                    .iter()
                    .map(|child| child.weight)
                    .collect::<Vec<_>>();
                let child_anchors = children
                    .iter()
                    .filter_map(|child| child.node.first_pane())
                    .collect::<Vec<_>>();
                let split_anchor = child_anchors[0];

                // This container's own pixel length along `axis`,
                // refreshed every frame by the absolutely positioned,
                // full-size canvas child below -- the standard gpui
                // trick for reading an element's own layout bounds
                // without a custom `Element` impl (an invisible canvas
                // whose bounds equal its parent's content box). Purely a
                // view-layer scratch value shared between this frame's
                // own canvas and mouse-event closures; never touches the
                // model.
                let container_len: Rc<Cell<f32>> = Rc::new(Cell::new(0.0));

                let mut container = div().size_full().flex();
                container = match axis {
                    SplitAxis::Horizontal => container.flex_row(),
                    SplitAxis::Vertical => container.flex_col(),
                };
                container = container.child({
                    let container_len = container_len.clone();
                    canvas(
                        move |bounds, _, _| {
                            container_len.set(axis_extent(axis, bounds.size));
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full()
                });
                container = container.on_mouse_move({
                    let child_anchors = child_anchors.clone();
                    let container_len = container_len.clone();
                    cx.listener(move |shell, event: &MouseMoveEvent, _window, cx| {
                        let effective_px =
                            effective_container_px(container_len.get(), child_anchors.len());
                        shell.update_split_drag(
                            split_anchor,
                            &child_anchors,
                            axis_position(axis, event.position),
                            effective_px,
                            event.dragging(),
                            cx,
                        );
                    })
                });
                container = container.on_mouse_up(MouseButton::Left, {
                    let child_anchors = child_anchors.clone();
                    cx.listener(move |shell, _event: &MouseUpEvent, _window, cx| {
                        shell.end_split_drag(split_anchor, &child_anchors, cx);
                    })
                });
                container = container.on_mouse_up_out(MouseButton::Left, {
                    let child_anchors = child_anchors.clone();
                    cx.listener(move |shell, _event: &MouseUpEvent, _window, cx| {
                        shell.end_split_drag(split_anchor, &child_anchors, cx);
                    })
                });

                for (index, child) in children.iter().enumerate() {
                    if index > 0 {
                        let pair_index = index - 1;
                        let is_dragging = self.active_split_drag.as_ref().is_some_and(|drag| {
                            drag.split_anchor == split_anchor
                                && drag.child_anchors == child_anchors
                                && drag.pair_index == pair_index
                        });
                        let line_color = if is_dragging {
                            theme::accent()
                        } else {
                            theme::border()
                        };
                        let handle_child_anchors = child_anchors.clone();
                        let handle_start_weights = start_weights.clone();
                        container = container.child(
                            div()
                                .relative()
                                .flex_shrink_0()
                                .flex_grow_0()
                                .group(SPLIT_HANDLE_GROUP)
                                .when(axis == SplitAxis::Horizontal, |this| {
                                    this.w(px(SPLIT_HANDLE_HIT_PX)).h_full().cursor_col_resize()
                                })
                                .when(axis == SplitAxis::Vertical, |this| {
                                    this.h(px(SPLIT_HANDLE_HIT_PX)).w_full().cursor_row_resize()
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .when(axis == SplitAxis::Horizontal, |this| {
                                            this.left(px((SPLIT_HANDLE_HIT_PX
                                                - SPLIT_HANDLE_LINE_PX)
                                                / 2.0))
                                                .top_0()
                                                .w(px(SPLIT_HANDLE_LINE_PX))
                                                .h_full()
                                        })
                                        .when(axis == SplitAxis::Vertical, |this| {
                                            this.top(px((SPLIT_HANDLE_HIT_PX
                                                - SPLIT_HANDLE_LINE_PX)
                                                / 2.0))
                                                .left_0()
                                                .h(px(SPLIT_HANDLE_LINE_PX))
                                                .w_full()
                                        })
                                        .bg(line_color)
                                        .when(!is_dragging, |this| {
                                            this.group_hover(SPLIT_HANDLE_GROUP, |style| {
                                                style.bg(theme::accent())
                                            })
                                        }),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(
                                        move |shell, event: &MouseDownEvent, _window, cx| {
                                            shell.begin_split_drag(
                                                SplitDrag {
                                                    tab_id,
                                                    split_anchor,
                                                    child_anchors: handle_child_anchors.clone(),
                                                    pair_index,
                                                    start_weights: handle_start_weights.clone(),
                                                    start_pos_px: axis_position(
                                                        axis,
                                                        event.position,
                                                    ),
                                                    applied: false,
                                                },
                                                cx,
                                            );
                                        },
                                    ),
                                ),
                        );
                    }

                    let child_element = self.render_node(tab_id, &child.node, cx);
                    container = container.child(
                        div()
                            .size_full()
                            // `flex_basis(0) + flex_grow(weight)`, not a
                            // weight-scaled basis -- flex only distributes
                            // *leftover* space (container minus the sum of
                            // every sibling's basis) by grow-factor ratio,
                            // so a nonzero basis with an equal grow factor
                            // dilutes the ratio toward equal the wider the
                            // container is past that basis sum (e.g. a 3:1
                            // weight split rendered as ~1.7:1 in a
                            // container much bigger than the old fixed
                            // 1000px basis budget). Zero basis puts the
                            // *entire* container into that leftover -- the
                            // idiom the pre-gpui-component Floem shell used
                            // (`floem-shell-final:src/workspace/view/
                            // layout_tree.rs`).
                            //
                            // `flex_grow` takes `child.weight / total_weight`,
                            // not the raw weight: Taffy's flexbox
                            // implementation (`compute/flexbox.rs`, pinned
                            // 0.10.1) only distributes *all* of the
                            // leftover space when the grow factors sum to
                            // >= 1 -- when they sum to less, the space
                            // actually distributed is capped at
                            // `leftover * sum_of_grow_factors`, leaving the
                            // rest as dead space (verified against the
                            // pinned source, `flexbox.rs:1271-1272`; CSS
                            // Flexbox's own spec, not a Taffy quirk). Raw
                            // model weights routinely sum below 1 --
                            // `Workspace::set_split_weights` normalizes to
                            // a sum of 1 on every *drag*, but closing a
                            // pane afterwards removes one child's weight
                            // from the sum without renormalizing the
                            // others (by design -- `without_pane` doesn't
                            // rescale siblings), so three panes normalized
                            // to 1/3 each become two panes still at 1/3
                            // each (sum 2/3) the moment one closes. Only
                            // the *ratio* between siblings' grow factors
                            // needs to be preserved, so dividing by
                            // `total_weight` here keeps that ratio while
                            // guaranteeing the sum is exactly 1 (mod
                            // float rounding, which lands sub-pixel and
                            // isn't visible).
                            .flex_basis(px(0.0))
                            .flex_grow(child.weight / total_weight)
                            .when(axis == SplitAxis::Horizontal, |this| {
                                this.min_w(px(SPLIT_PANEL_MIN_SIZE_PX))
                            })
                            .when(axis == SplitAxis::Vertical, |this| {
                                this.min_h(px(SPLIT_PANEL_MIN_SIZE_PX))
                            })
                            .child(child_element),
                    );
                }
                container.into_any_element()
            }
        }
    }

    /// A split handle's `on_mouse_down`: opens a pairwise drag session
    /// (`docs/split-resize-design.md`'s Drag semantics), immediately
    /// visible via the handle's own dragging-highlight check in
    /// `render_node`.
    fn begin_split_drag(&mut self, drag: SplitDrag, cx: &mut Context<Self>) {
        if self.restoring_workspace {
            return;
        }
        self.active_split_drag = Some(drag);
        cx.notify();
    }

    /// The split container's `on_mouse_move`. Two responsibilities:
    ///
    /// - Live reflow, once per move, for as long as `self.
    ///   active_split_drag` identifies *this* split (`split_anchor` plus
    ///   `child_anchors`, the same disambiguation `Workspace::
    ///   set_split_weights` itself uses -- a first-pane anchor alone is
    ///   ambiguous between nested splits). No-ops for every other
    ///   split's container, and for this one when no drag (or someone
    ///   else's) is in progress.
    /// - Recovering a *leaked* drag session: if the split container that
    ///   started a drag gets unmounted mid-drag (e.g. the active tab
    ///   changes), its own `on_mouse_up`/`on_mouse_up_out` never fires,
    ///   so `active_split_drag` would otherwise survive indefinitely --
    ///   and every later plain hover (no button held) over *whatever*
    ///   split the leaked drag still points at would silently resume
    ///   resizing it, since `set_split_weights` doesn't know the
    ///   difference between a real drag and a stale one.  `is_dragging`
    ///   (the platform's own `pressed_button == Some(Left)`, exposed as
    ///   `MouseMoveEvent::dragging()`) distinguishes the two; when it's
    ///   `false` and a drag is still active, end it exactly like a real
    ///   mouse-up would -- checked *before* the anchor match below, and
    ///   unconditionally via `end_active_split_drag`, so this also
    ///   cleans up the rarer variant where a pane close changed the
    ///   tree enough that no split's anchors match the leaked drag
    ///   anymore (it would otherwise never be reachable by the
    ///   anchor-checked `end_split_drag` path at all).
    fn update_split_drag(
        &mut self,
        split_anchor: PaneId,
        child_anchors: &[PaneId],
        mouse_pos_px: f32,
        container_px: f32,
        is_dragging: bool,
        cx: &mut Context<Self>,
    ) {
        if !is_dragging {
            self.end_active_split_drag(cx);
            return;
        }
        let Some(drag) = self.active_split_drag.clone() else {
            return;
        };
        if drag.split_anchor != split_anchor || drag.child_anchors != child_anchors {
            return;
        }
        let total_weight = drag.start_weights.iter().sum::<f32>();
        let delta_px = mouse_pos_px - drag.start_pos_px;
        let (new_a, new_b) = pairwise_resize_weights(
            drag.start_weights[drag.pair_index],
            drag.start_weights[drag.pair_index + 1],
            total_weight,
            container_px,
            delta_px,
            SPLIT_PANEL_MIN_SIZE_PX,
        );
        let mut sizes = drag.start_weights.clone();
        sizes[drag.pair_index] = new_a;
        sizes[drag.pair_index + 1] = new_b;
        if self
            .workspace
            .set_split_weights(drag.tab_id, split_anchor, child_anchors, &sizes)
        {
            if let Some(active) = self.active_split_drag.as_mut() {
                active.applied = true;
            }
            cx.notify();
        }
    }

    /// The split container's `on_mouse_up`/`on_mouse_up_out`: ends the
    /// drag if `self.active_split_drag` is *this* split's (both are
    /// registered on the same container so the release is caught
    /// regardless of whether the cursor is still over the split when
    /// the button comes up). A no-op for every other split's container.
    fn end_split_drag(
        &mut self,
        split_anchor: PaneId,
        child_anchors: &[PaneId],
        cx: &mut Context<Self>,
    ) {
        let matches_this_split = self.active_split_drag.as_ref().is_some_and(|drag| {
            drag.split_anchor == split_anchor && drag.child_anchors == child_anchors
        });
        if !matches_this_split {
            return;
        }
        self.end_active_split_drag(cx);
    }

    /// Ends whatever drag is currently active (if any), persisting once
    /// -- matching the pre-existing `on_resize` cadence -- but only if
    /// it actually changed a weight (`SplitDrag::applied`; an unmoved
    /// click shouldn't touch the persisted file). Shared by the normal,
    /// anchor-checked mouse-up path (`end_split_drag`) and the leaked-
    /// drag recovery path in `update_split_drag`, which calls this
    /// unconditionally (deliberately not anchor-checked there -- a
    /// leaked drag must be clearable by *any* split's hover, not only
    /// the one whose anchors still happen to match).
    fn end_active_split_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.active_split_drag.take() else {
            return;
        };
        if drag.applied {
            self.persist_workspace();
        }
        cx.notify();
    }
}

impl Render for WorkspaceShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Suppressed outright while a restore is in progress (unless it
        // failed, which still allows reaching `Reload Session Runtime` --
        // same predicate `toggle_mode`/`mode_move`/`mode_commit`/
        // `mode_cancel` already gate on): a workspace persisted with zero
        // tabs would otherwise have `is_workspace_mode_active()`'s
        // zero-tab bypass firing immediately on load, before the restore
        // sweep (which still runs a background round trip to sessiond
        // even when there's nothing to resume) has actually finished.
        let restore_blocked = workspace_mode_blocked_by_restore(
            self.restoring_workspace,
            self.workspace_restore_failed,
        );
        let mode_active = !restore_blocked
            && mode_key_context_active(
                self.workspace.is_workspace_mode_active(),
                self.any_modal_open(),
            );
        let content = self
            .workspace
            .active_tab()
            .map(|tab| (tab.id, tab.root.clone()))
            .map(|(tab_id, root)| self.render_node(tab_id, &root, cx));

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
                shell.create_session(PaneKind::Agent, None, false, Placement::NewTab, window, cx);
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
                    .when_some(self.markdown_open.clone(), |this, markdown_open| {
                        this.child(
                            div()
                                .id("markdown-open-backdrop")
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
                                        shell.close_markdown_modal(window, cx);
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(560.0))
                                        .h(px(120.0))
                                        .bg(rgb(theme::background()))
                                        .border_1()
                                        .border_color(theme::border())
                                        .shadow(theme::overlay_shadow())
                                        .rounded_md()
                                        .overflow_hidden()
                                        .child(List::new(&markdown_open)),
                                ),
                        )
                    })
                    .when_some(self.session_manager.clone(), |this, manager| {
                        this.child(
                            div()
                                .id("session-manager-backdrop")
                                .key_context(SESSION_MANAGER_CONTEXT)
                                .on_action(cx.listener(
                                    |shell, _: &OpenSessionDirectory, window, cx| {
                                        shell.open_selected_session_directory(window, cx);
                                    },
                                ))
                                .on_action(cx.listener(
                                    |shell, _: &TerminateSessionSubtree, window, cx| {
                                        shell.terminate_selected_session_subtree(window, cx);
                                    },
                                ))
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
    use gpui::px;
    use horizon_workspace::PaneId;

    use super::{
        effective_container_px, effective_scrim_pattern, equal_tab_width, mode_key_context_active,
        pairwise_resize_weights, pane_border_role, pane_scrim_alpha,
        workspace_mode_blocked_by_restore, PaneBorderRole, SCRIM_DIM_ALPHA,
    };

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
    fn effective_container_px_subtracts_every_handles_fixed_width() {
        // 4 children -> 3 handles, each SPLIT_HANDLE_HIT_PX (9px) wide --
        // only the remainder actually divides by weight ratio (child
        // wrappers are flex_basis(0) + flex_grow(weight), the handles are
        // flex_grow_0/flex_shrink_0 at a fixed width, so their pixels
        // never participate in that distribution).
        assert_eq!(effective_container_px(1000.0, 4), 1000.0 - 3.0 * 9.0);
    }

    #[test]
    fn effective_container_px_is_unchanged_for_a_bare_pane() {
        // No handles at all -- the whole length is available (also
        // covers the degenerate `child_count == 0` case sanely).
        assert_eq!(effective_container_px(500.0, 1), 500.0);
        assert_eq!(effective_container_px(500.0, 0), 500.0);
    }

    #[test]
    fn effective_container_px_never_goes_negative() {
        // A pathologically narrow container with many handles -- clamp
        // at zero rather than handing a negative length downstream.
        assert_eq!(effective_container_px(10.0, 10), 0.0);
    }

    // `pairwise_resize_weights`'s own `container_px` parameter is always
    // the *effective* length (`effective_container_px`'s result) in real
    // drag code -- these tests exercise the pure conversion math directly
    // with round numbers, so "container" below means that effective
    // length, not a container's raw measured size.

    #[test]
    fn pairwise_resize_weights_transfers_between_the_pair_only() {
        // 1000px effective container, two equal siblings (500px each),
        // dragged 100px toward b: weight moves from a to b in direct
        // proportion, and the pair's total weight is preserved
        // (renormalization by the caller keeps other siblings' shares
        // untouched).
        let (new_a, new_b) = pairwise_resize_weights(1.0, 1.0, 2.0, 1000.0, 100.0, 100.0);
        assert!((new_a - 1.2).abs() < 1e-4);
        assert!((new_b - 0.8).abs() < 1e-4);
        assert!((new_a + new_b - 2.0).abs() < 1e-4);
    }

    #[test]
    fn pairwise_resize_weights_hard_stops_at_the_floor_instead_of_cascading() {
        // Same pair, dragged far past where `b` would go below its 100px
        // floor -- clamps to exactly the floor rather than continuing
        // (there are no other siblings here to cascade into, but the
        // clamp itself must not overshoot).
        let (new_a, new_b) = pairwise_resize_weights(1.0, 1.0, 2.0, 1000.0, 900.0, 100.0);
        assert!((new_b - 0.2).abs() < 1e-4); // 100px / 1000px * 2.0 total weight
        assert!((new_a + new_b - 2.0).abs() < 1e-4);
    }

    #[test]
    fn pairwise_resize_weights_hard_stops_in_the_other_direction_too() {
        let (new_a, new_b) = pairwise_resize_weights(1.0, 1.0, 2.0, 1000.0, -900.0, 100.0);
        assert!((new_a - 0.2).abs() < 1e-4);
        assert!((new_a + new_b - 2.0).abs() < 1e-4);
    }

    #[test]
    fn pairwise_resize_weights_is_a_no_op_for_a_degenerate_container() {
        assert_eq!(
            pairwise_resize_weights(1.0, 1.0, 2.0, 0.0, 50.0, 100.0),
            (1.0, 1.0)
        );
    }

    #[test]
    fn pairwise_resize_weights_splits_evenly_when_both_floors_cant_fit() {
        // A 150px container with a 100px floor: the floor alone is
        // already two-thirds of the whole container, so no split of the
        // pair can keep both children at or above it simultaneously.
        // There's no valid clamp range, so this degrades to an even
        // split rather than panicking on an inverted (min > max) range --
        // and the large delta below confirms it holds regardless of drag
        // direction/distance.
        let (new_a, new_b) = pairwise_resize_weights(0.5, 0.5, 1.0, 150.0, 500.0, 100.0);
        assert_eq!((new_a, new_b), (0.5, 0.5));
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
    fn failed_restore_allows_workspace_mode_to_reach_the_reload_command() {
        assert!(workspace_mode_blocked_by_restore(true, false));
        assert!(!workspace_mode_blocked_by_restore(true, true));
        assert!(!workspace_mode_blocked_by_restore(false, false));
    }

    #[test]
    fn mode_key_context_follows_the_live_state_when_no_modal_is_open() {
        assert!(mode_key_context_active(true, false));
        assert!(!mode_key_context_active(false, false));
    }

    #[test]
    fn mode_key_context_is_suppressed_while_a_modal_is_open() {
        // The hazard this guards against: an empty workspace's
        // `is_workspace_mode_active()` stays `true` even after a
        // modal-opening handler calls `Workspace::exit_workspace_mode`
        // (the zero-tab bypass doesn't care about the raw field) -- so
        // without this suppression, the mode's fixed hjkl/Enter/Escape
        // bindings would keep competing with the modal's own typed
        // search/confirm keys.
        assert!(!mode_key_context_active(true, true));
        assert!(!mode_key_context_active(false, true));
    }
}
