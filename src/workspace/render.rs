//! Tab-strip and pane-tree rendering: `render_tab_strip`/`render_node`,
//! the `Render` impl that wires GPUI actions to model calls, the
//! workspace-mode cursor/dim pattern's pure pane-chrome functions
//! (`pane_scrim_alpha`, `effective_scrim_pattern`, `pane_border_role`,
//! `split_child_insets`, `equal_tab_width`), and the mode/tab/pane
//! action handlers (`toggle_mode`, `mode_move`, `mode_commit`,
//! `mode_cancel`, `next_tab`, `activate_tab`, `activate_pane`) that only
//! the `Render` impl below dispatches into.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::list::List;
use gpui_component::resizable::{h_resizable, resizable_panel, v_resizable, ResizablePanelGroup};
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

impl WorkspaceShell {
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
        effective_scrim_pattern, equal_tab_width, pane_border_role, pane_scrim_alpha,
        split_child_insets, workspace_mode_blocked_by_restore, PaneBorderRole, SCRIM_DIM_ALPHA,
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
    fn failed_restore_allows_workspace_mode_to_reach_the_reload_command() {
        assert!(workspace_mode_blocked_by_restore(true, false));
        assert!(!workspace_mode_blocked_by_restore(true, true));
        assert!(!workspace_mode_blocked_by_restore(false, false));
    }
}
