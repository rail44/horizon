//! Live and held-history painting with separate row caches and shared geometry.

use super::shape_cache::{CacheEpoch, RowItem, ShapedLineCache, NO_GENERATION};
use super::{
    font::{font_size, line_height, resolved_font},
    glyphs,
    shaping::shape_row_items,
    TerminalView,
};
use crate::input_trace::input_trace;
use crate::theme;
use gpui::*;
use horizon_terminal_core::TerminalSize;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

/// Alpha for the selection overlay quad (`paint_terminal`'s
/// `frame.selection` pass): translucent enough that the selected text --
/// painted underneath, with its own untouched colors -- stays readable,
/// following the block cursor's translucent-quad-over-glyph precedent.
const SELECTION_OVERLAY_ALPHA: f32 = 0.35;

/// Bar thickness for the Underline/Beam cursor shapes
/// (`TerminalCursor::shape`).
const CURSOR_BAR_THICKNESS: Pixels = px(2.0);

/// Paint-time geometry shared with event handlers (which need to convert
/// window-relative pixel positions into cell coordinates). Written every
/// paint, read by the mouse handlers.
#[derive(Clone, Copy)]
pub(super) struct PaintMetrics {
    pub(super) origin: Point<Pixels>,
    pub(super) cell_width: Pixels,
    pub(super) line_height: Pixels,
}

pub(super) struct PaintCaches {
    live: RefCell<ShapedLineCache>,
    scrollback: RefCell<ShapedLineCache>,
}

impl PaintCaches {
    pub(super) fn new() -> Self {
        Self {
            live: RefCell::new(ShapedLineCache::new()),
            scrollback: RefCell::new(ShapedLineCache::new()),
        }
    }
}

/// Paint a held scrollback window's visible slice
/// (`docs/terminal-scrollback-design.md` §3.3). The client scrolls within the
/// window locally, so these rows come straight from the held
/// `TerminalScrollWindow` rather than the live `watch<TerminalFrame>`. Painting
/// mirrors the live row loop (background quads then shaped text) minus the
/// cursor/selection/IME overlays, which are live-viewport concepts a
/// scrolled-back history view does not carry. Its cache is keyed by stable row
/// index within the held window plus window generation, so scrolling reuses
/// every overlapping shaped row and shapes only a newly exposed edge.
#[allow(clippy::too_many_arguments)]
fn paint_scrollback_window(
    lines: &[horizon_terminal_core::TerminalLine],
    first_window_row: usize,
    fractional_row: f32,
    generation: u64,
    bounds: Bounds<Pixels>,
    palette_overrides: &[(u16, [u8; 3])],
    default_bg: Hsla,
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    text_system: &WindowTextSystem,
    shape_cache: &mut ShapedLineCache,
    window: &mut Window,
    cx: &mut App,
) {
    let y_offset = -(line_height * fractional_row);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for (row, line) in lines.iter().enumerate() {
            let row_origin = bounds.origin + point(px(0.0), y_offset + line_height * row as f32);

            paint_row_backgrounds(
                line,
                PaintMetrics {
                    origin: row_origin,
                    cell_width,
                    line_height,
                },
                palette_overrides,
                default_bg,
                window,
            );

            let window_row = first_window_row + row;
            let items = shape_cache.get_or_shape(window_row, generation, || {
                shape_row_items(
                    line,
                    palette_overrides,
                    font,
                    font_size,
                    cell_width,
                    text_system,
                )
            });
            paint_row_items(items, row_origin, cell_width, window, cx);
        }
    });
}

pub(super) fn paint_terminal(
    bounds: Bounds<Pixels>,
    entity: &Entity<TerminalView>,
    last_size: &Rc<Cell<TerminalSize>>,
    metrics: &Rc<Cell<Option<PaintMetrics>>>,
    paint_caches: &PaintCaches,
    window: &mut Window,
    cx: &mut App,
) {
    let text_system = window.text_system().clone();
    let font = resolved_font();
    let font_size = px(font_size());
    let line_height = px(line_height());
    let font_id = text_system.resolve_font(&font);
    let cell_width = text_system
        .advance(font_id, font_size, 'M')
        .map(|size| size.width)
        .unwrap_or(px(8.0));
    metrics.set(Some(PaintMetrics {
        origin: bounds.origin,
        cell_width,
        line_height,
    }));

    let cols = (f32::from(bounds.size.width) / f32::from(cell_width)).floor() as u16;
    let rows = (f32::from(bounds.size.height) / f32::from(line_height)).floor() as u16;
    let size = TerminalSize {
        cols: cols.max(2),
        rows: rows.max(2),
        pixel_width: (f32::from(cell_width) * cols.max(2) as f32) as u16,
        pixel_height: (f32::from(line_height) * rows.max(2) as f32) as u16,
    };
    if last_size.get() != size {
        last_size.set(size);
        let view = entity.read(cx);
        view.session.read(cx).send_resize(size);
    }

    let (session, marked_text) = {
        let view = entity.read(cx);
        (
            view.session.clone(),
            view.keyboard.marked_text().map(str::to_owned),
        )
    };
    let (frame, row_generations, scrollback_lines) = {
        let session = session.read(cx);
        let Some(frame) = session.frame.clone() else {
            return;
        };
        // While scrolled back the client paints a held window slice locally
        // instead of the live frame (`docs/terminal-scrollback-design.md`
        // §3.3); `None` means follow the live tail.
        let scrollback_lines = session.visible_scrollback(size.rows as usize);
        (frame, session.row_generations().to_vec(), scrollback_lines)
    };

    let default_bg = theme::to_hsla(theme::resolve(
        horizon_terminal_core::TerminalColor::Named(horizon_terminal_core::NamedColor::Background),
        &frame.palette_overrides,
    ));

    // Scrollback windowed local paint: the client owns the offset into a held
    // window and repaints from it with zero IPC. History only — no cursor,
    // selection, or IME preedit (all live-viewport concepts; alacritty hides
    // the cursor while scrolled back too). The held window is immutable and
    // Arc-backed. Its own cache is indexed
    // by row in that window (not viewport row), so scrolling shifts paint
    // origins without invalidating overlapping shaped rows.
    if let Some(scrollback) = scrollback_lines {
        let mut cache = paint_caches.scrollback.borrow_mut();
        cache.begin_frame(
            CacheEpoch {
                theme: theme::terminal_color_scheme(),
                palette_overrides: frame.palette_overrides.clone(),
                font_size: f32::from(font_size),
            },
            scrollback.window.lines.len(),
        );
        let lines = &scrollback.window.lines[scrollback.range.clone()];
        paint_scrollback_window(
            lines,
            scrollback.range.start,
            scrollback.fractional_row,
            scrollback.generation,
            bounds,
            &frame.palette_overrides,
            default_bg,
            &font,
            font_size,
            cell_width,
            line_height,
            &text_system,
            &mut cache,
            window,
            cx,
        );
        if let Some(trace) = cache.trace_line() {
            input_trace!("scrollback-{trace}");
        }
        return;
    }

    // Row-keyed shaping memo (goal 3/4 of docs/terminal-protocol-goals.md,
    // see `shape_cache`): a row whose generation stamp is unchanged since
    // the last paint replays its cached ShapedLines/geometric glyphs;
    // only changed rows walk their spans and shape again. The epoch
    // compare clears everything when resolved colors moved out from under
    // the cached runs (theme reload, OSC palette override).
    let mut cache = paint_caches.live.borrow_mut();
    cache.begin_frame(
        CacheEpoch {
            theme: theme::terminal_color_scheme(),
            palette_overrides: frame.palette_overrides.clone(),
            font_size: f32::from(font_size),
        },
        size.rows as usize,
    );

    // Grid-positioned painting (the pattern every surveyed GPUI terminal
    // converges on): each span is painted at its computed column offset,
    // never left to shaped-text flow, and glyph advances are snapped to
    // the cell grid via shape_line's force_width when the span is
    // width-uniform (see `shape_row_items`).
    for (row, line) in frame.lines.iter().enumerate() {
        if row >= size.rows as usize {
            break;
        }
        let row_origin = bounds.origin + point(px(0.0), line_height * row as f32);

        paint_row_backgrounds(
            line,
            PaintMetrics {
                origin: row_origin,
                cell_width,
                line_height,
            },
            &frame.palette_overrides,
            default_bg,
            window,
        );

        let generation = row_generations.get(row).copied().unwrap_or(NO_GENERATION);
        let items = cache.get_or_shape(row, generation, || {
            shape_row_items(
                line,
                &frame.palette_overrides,
                &font,
                font_size,
                cell_width,
                &text_system,
            )
        });
        paint_row_items(items, row_origin, cell_width, window, cx);
    }
    if let Some(trace) = cache.trace_line() {
        input_trace!("{trace}");
    }
    drop(cache);

    // Selection overlay -- the client-side half of the v7 semantic
    // selection (`TerminalFrame::selection`, goal 2 of
    // docs/terminal-protocol-goals.md): a theme-resolved translucent quad
    // per selected row, painted over the finished text so the spans
    // underneath stay untouched (their fg/bg no longer carry selection
    // state). Endpoint rows cover their partial column range; every row
    // between them is fully covered.
    if let Some(selection) = frame.selection {
        let mut color = theme::terminal_selection();
        color.a = SELECTION_OVERLAY_ALPHA;
        let last_row = (size.rows as usize).saturating_sub(1);
        let last_col = (size.cols as usize).saturating_sub(1);
        for row in selection.start.row..=selection.end.row.min(last_row) {
            let start_col = if row == selection.start.row {
                selection.start.col
            } else {
                0
            };
            let end_col = if row == selection.end.row {
                selection.end.col.min(last_col)
            } else {
                last_col
            };
            if start_col > end_col {
                continue;
            }
            let origin =
                bounds.origin + point(cell_width * start_col as f32, line_height * row as f32);
            window.paint_quad(fill(
                Bounds::new(
                    origin,
                    gpui::size(cell_width * (end_col - start_col + 1) as f32, line_height),
                ),
                color,
            ));
        }
    }

    // IME preedit overlay: paint the composing text at the cursor cell,
    // underlined, over an opaque background quad that hides whatever the
    // grid has there. The regular cursor is suppressed while composing.
    if let Some(marked) = marked_text.filter(|marked| !marked.is_empty()) {
        if let Some(cursor) = frame.cursor {
            let origin = bounds.origin
                + point(
                    cell_width * cursor.col as f32,
                    line_height * cursor.row as f32,
                );
            let columns: usize = marked.chars().map(|ch| char_columns(ch).max(1)).sum();
            window.paint_quad(fill(
                Bounds::new(origin, gpui::size(cell_width * columns as f32, line_height)),
                default_bg,
            ));
            let fg = theme::to_hsla(theme::resolve(
                horizon_terminal_core::TerminalColor::Named(
                    horizon_terminal_core::NamedColor::Foreground,
                ),
                &frame.palette_overrides,
            ));
            let run = TextRun {
                len: marked.len(),
                font: font.clone(),
                color: fg,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(fg),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let shaped = text_system.shape_line(marked.into(), font_size, &[run], None);
            let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
        }
        return;
    }

    if let Some(cursor) = frame.cursor {
        let origin = bounds.origin
            + point(
                cell_width * cursor.col as f32,
                line_height * cursor.row as f32,
            );
        let mut color = theme::to_hsla(theme::resolve(
            horizon_terminal_core::TerminalColor::Named(horizon_terminal_core::NamedColor::Cursor),
            &frame.palette_overrides,
        ));
        let cell = Bounds::new(origin, gpui::size(cell_width, line_height));
        // Shape from the v7 vocabulary (`TerminalCursor::shape`, DECSCUSR).
        // Block keeps its pre-v7 translucency so the glyph under it stays
        // readable; the bar/outline shapes cover no glyph and paint opaque.
        match cursor.shape {
            horizon_terminal_core::TerminalCursorShape::Block => {
                color.a = 0.6;
                window.paint_quad(fill(cell, color));
            }
            horizon_terminal_core::TerminalCursorShape::Underline => {
                window.paint_quad(fill(
                    Bounds::new(
                        origin + point(px(0.0), line_height - CURSOR_BAR_THICKNESS),
                        gpui::size(cell_width, CURSOR_BAR_THICKNESS),
                    ),
                    color,
                ));
            }
            horizon_terminal_core::TerminalCursorShape::Beam => {
                window.paint_quad(fill(
                    Bounds::new(origin, gpui::size(CURSOR_BAR_THICKNESS, line_height)),
                    color,
                ));
            }
            horizon_terminal_core::TerminalCursorShape::HollowBlock => {
                window.paint_quad(outline(cell, color, BorderStyle::Solid));
            }
        }
    }
}

/// Repaint backgrounds even for empty-text spans: erased cells and colored
/// padding carry their fill in `columns`/`bg` in both live and history frames.
fn paint_row_backgrounds(
    line: &horizon_terminal_core::TerminalLine,
    metrics: PaintMetrics,
    palette_overrides: &[(u16, [u8; 3])],
    default_bg: Hsla,
    window: &mut Window,
) {
    let mut col = 0_usize;
    for span in &line.spans {
        let x = metrics.cell_width * col as f32;
        col += span.columns;
        let bg = theme::to_hsla(theme::resolve(span.bg, palette_overrides));
        if bg != default_bg {
            window.paint_quad(fill(
                Bounds::new(
                    metrics.origin + point(x, px(0.0)),
                    gpui::size(
                        metrics.cell_width * span.columns as f32,
                        metrics.line_height,
                    ),
                ),
                bg,
            ));
        }
    }
}

/// Replays a row's [`RowItem`]s into the scene at `row_origin` — the
/// per-frame half of the split: cheap paint calls only, no shaping, no
/// span walking. Cell metrics come from the same startup-fixed globals
/// `shape_row_items` shaped under; scale factor is read live because it
/// is applied at paint time, not baked into the cached items.
fn paint_row_items(
    items: &[RowItem],
    row_origin: Point<Pixels>,
    cell_width: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let font_size = px(font_size());
    let line_height = px(line_height());
    let scale_factor = window.scale_factor();
    for item in items {
        match item {
            RowItem::Text { col, shaped } => {
                let origin = row_origin + point(cell_width * *col as f32, px(0.0));
                let _ = shaped.paint(origin, line_height, TextAlign::Left, None, window, cx);
            }
            RowItem::Glyph {
                col,
                ch,
                width_cols,
                fg,
            } => {
                let cell_bounds = Bounds::new(
                    row_origin + point(cell_width * *col as f32, px(0.0)),
                    gpui::size(cell_width * *width_cols as f32, line_height),
                );
                glyphs::paint_glyph(window, cell_bounds, *ch, *fg, font_size, scale_factor);
            }
        }
    }
}

/// East-Asian width of a char in terminal columns (mirrors the
/// `char_width` helper in horizon-terminal-core's frame module).
pub(super) fn char_columns(ch: char) -> usize {
    use unicode_width::UnicodeWidthChar as _;
    ch.width().unwrap_or(0)
}
