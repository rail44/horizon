//! Row-keyed memo of `shape_line` results — the consumer end of goal 3
//! and the shaping half of goal 4 in `docs/terminal-protocol-goals.md`:
//! shaping work becomes proportional to *changed* rows while scene
//! construction (bg quads, selection overlay, cursor, and the cached
//! lines' `paint` calls) stays proportional to *visible* rows, because
//! GPUI rebuilds the scene every frame regardless.
//!
//! Validity is two-tier:
//!
//! - **Per row**: the generation stamp from
//!   [`super::session::RowGenerations`] — the surviving form of the
//!   wire's `changed_rows`. Same stamp ⇒ same row content ⇒ the cached
//!   items repaint as-is; a bumped stamp re-shapes just that row.
//! - **Per frame ([`CacheEpoch`])**: everything a cached item *bakes in*
//!   besides row content — resolved colors. Text color rides inside each
//!   `ShapedLine`'s decoration runs and each geometric glyph's `fg`, and
//!   color resolution reads the live theme scheme plus the frame's OSC
//!   palette overrides, so a `Reload Config` theme swap or an OSC 4/10/11
//!   override change must drop every row at once. The epoch is compared
//!   at the top of each paint; on mismatch the whole cache clears —
//!   deliberately the simplest invalidation shape.
//!
//! Font and cell metrics are *not* an epoch axis: `[ui] font_family` /
//! `[terminal] font_size` (and the line height derived from it) are
//! startup-only `OnceLock`s (see AGENTS.md's Configuration section — a
//! change needs a full restart), so a cached `ShapedLine` can never
//! outlive its metrics. Window scale factor is applied at paint time,
//! not baked into shaping, so DPI changes need no axis either.
//!
//! Memory is bounded naturally: `begin_frame` sizes the row table to the
//! visible viewport, so the cache never holds more than one screenful of
//! shaped lines per pane.

use std::time::{Duration, Instant};

use gpui::{Hsla, ShapedLine};
use horizon_terminal_core::TerminalColorScheme;

/// The out-of-band stamp for a row the generation table doesn't cover
/// (structurally impossible while the pump's `RowGenerations::apply_frame`
/// keeps the table in lockstep with the frame, but the paint loop maps a
/// missing entry here rather than panicking). Real stamps are always ≥ 1 —
/// `RowGenerations` starts at 0 and bumps before stamping — and
/// [`ShapedLineCache::get_or_shape`] never reports a hit for this value, so
/// such a row is simply re-shaped every frame.
pub(super) const NO_GENERATION: u64 = 0;

/// The frame-level invalidation axes: a change to either means every
/// cached item's baked-in colors are stale, so the whole cache clears.
#[derive(Clone, PartialEq)]
pub(super) struct CacheEpoch {
    /// The live theme's terminal-facing colors
    /// (`theme::terminal_color_scheme()`): the exact set `theme::resolve`
    /// reads for named/indexed colors, so any `Reload Config` /
    /// theme-settings live-apply that would recolor the grid changes this
    /// fingerprint.
    pub(super) theme: TerminalColorScheme,
    /// `TerminalFrame::palette_overrides` — the session's OSC 4/10/11/12
    /// overrides, consulted by `theme::resolve` before the scheme. An
    /// override-only change arrives with no `changed_rows`, so it must
    /// invalidate here, not per row.
    pub(super) palette_overrides: Vec<(u16, [u8; 3])>,
}

/// One item of a row's shaped text layer, positioned by starting column.
/// The paint side multiplies `col` by the live cell width, so cached
/// items carry grid coordinates, never pixels.
pub(super) enum RowItem {
    /// A `shape_line` result for a run of ordinary text. Boxed because
    /// `ShapedLine` is ~3 KB inline (its decoration-run `SmallVec`),
    /// which would otherwise balloon every `RowItem`.
    Text { col: usize, shaped: Box<ShapedLine> },
    /// A box-drawing/block/Braille character painted as cell-sized
    /// geometry (`super::glyphs`) rather than a font glyph. Cached
    /// alongside the shaped runs so a cache hit skips the whole
    /// char-classification walk, not just `shape_line`.
    Glyph {
        col: usize,
        ch: char,
        width_cols: usize,
        fg: Hsla,
    },
}

struct CachedRow {
    generation: u64,
    items: Vec<RowItem>,
}

/// How often [`ShapedLineCache::trace_line`] emits a snapshot (mirrors
/// the former platform frame-loop counter's one-second cadence).
const TRACE_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct ShapedLineCache {
    epoch: Option<CacheEpoch>,
    /// Indexed by viewport row; `None` is an empty slot (never painted,
    /// or dropped by an epoch clear).
    rows: Vec<Option<CachedRow>>,
    /// Cumulative row lookups served from cache / re-shaped, since
    /// construction. Observable per second via [`Self::trace_line`] on
    /// the `HORIZON_INPUT_TRACE` sink.
    hits: u64,
    misses: u64,
    last_trace: Instant,
}

impl ShapedLineCache {
    pub(super) fn new() -> Self {
        Self {
            epoch: None,
            rows: Vec::new(),
            hits: 0,
            misses: 0,
            last_trace: Instant::now(),
        }
    }

    /// Aligns the cache with this paint's frame-level axes and viewport:
    /// a changed epoch drops every row; the row table is then sized to
    /// the visible row count (truncating rows a shrink removed — the
    /// memory bound).
    pub(super) fn begin_frame(&mut self, epoch: CacheEpoch, visible_rows: usize) {
        if self.epoch.as_ref() != Some(&epoch) {
            self.rows.clear();
            self.epoch = Some(epoch);
        }
        self.rows.resize_with(visible_rows, || None);
    }

    /// The per-row decision: returns the cached items when `generation`
    /// matches the stamp they were shaped under, otherwise calls `shape`
    /// and caches its result. `row` must be within the `visible_rows`
    /// passed to this paint's [`Self::begin_frame`].
    pub(super) fn get_or_shape(
        &mut self,
        row: usize,
        generation: u64,
        shape: impl FnOnce() -> Vec<RowItem>,
    ) -> &[RowItem] {
        let hit = generation != NO_GENERATION
            && self.rows[row]
                .as_ref()
                .is_some_and(|cached| cached.generation == generation);
        if hit {
            self.hits += 1;
        } else {
            self.misses += 1;
            self.rows[row] = Some(CachedRow {
                generation,
                items: shape(),
            });
        }
        &self.rows[row].as_ref().expect("filled above").items
    }

    /// A per-second hit/miss snapshot for the `HORIZON_INPUT_TRACE` sink,
    /// or `None` inside the cadence window (the common case).
    pub(super) fn trace_line(&mut self) -> Option<String> {
        self.trace_line_at(Instant::now())
    }

    /// Clock-injected core, testable without sleeping (the
    /// `FrameLoopStats::record_redraw_requested_at` pattern).
    fn trace_line_at(&mut self, now: Instant) -> Option<String> {
        if now.duration_since(self.last_trace) < TRACE_INTERVAL {
            return None;
        }
        self.last_trace = now;
        Some(format!(
            "shape-cache: rows={} hits={} misses={}",
            self.rows.len(),
            self.hits,
            self.misses
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheEpoch, RowItem, ShapedLineCache, NO_GENERATION, TRACE_INTERVAL};
    use gpui::ShapedLine;
    use horizon_terminal_core::TerminalColorScheme;

    fn epoch() -> CacheEpoch {
        CacheEpoch {
            theme: TerminalColorScheme::default(),
            palette_overrides: Vec::new(),
        }
    }

    /// A recognizable one-item row so tests can tell a cached result from
    /// a re-shaped one by its `col`.
    fn items(col: usize) -> Vec<RowItem> {
        vec![RowItem::Text {
            col,
            shaped: Box::new(ShapedLine::default()),
        }]
    }

    fn col_of(items: &[RowItem]) -> usize {
        match items {
            [RowItem::Text { col, .. }] => *col,
            other => panic!("expected one text item, got {} items", other.len()),
        }
    }

    #[test]
    fn a_matching_generation_hits_without_reshaping() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 2);
        cache.get_or_shape(0, 7, || items(1));

        cache.begin_frame(epoch(), 2);
        let mut shaped = false;
        let cached = cache.get_or_shape(0, 7, || {
            shaped = true;
            items(2)
        });
        assert_eq!(col_of(cached), 1);
        assert!(!shaped);
        assert_eq!((cache.hits, cache.misses), (1, 1));
    }

    #[test]
    fn an_advanced_generation_misses_and_reshapes() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 2);
        cache.get_or_shape(0, 7, || items(1));

        cache.begin_frame(epoch(), 2);
        let cached = cache.get_or_shape(0, 8, || items(2));
        assert_eq!(col_of(cached), 2);
        assert_eq!((cache.hits, cache.misses), (0, 2));
    }

    #[test]
    fn an_unchanged_row_beside_a_changed_one_still_hits() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 2);
        cache.get_or_shape(0, 7, || items(1));
        cache.get_or_shape(1, 7, || items(1));

        cache.begin_frame(epoch(), 2);
        cache.get_or_shape(0, 9, || items(2));
        let untouched = cache.get_or_shape(1, 7, || items(2));
        assert_eq!(col_of(untouched), 1);
        assert_eq!((cache.hits, cache.misses), (1, 3));
    }

    #[test]
    fn a_theme_change_clears_every_row() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 2);
        cache.get_or_shape(0, 7, || items(1));
        cache.get_or_shape(1, 7, || items(1));

        let mut recolored = epoch();
        recolored.theme.foreground.r = recolored.theme.foreground.r.wrapping_add(1);
        cache.begin_frame(recolored, 2);
        assert_eq!(col_of(cache.get_or_shape(0, 7, || items(2))), 2);
        assert_eq!(col_of(cache.get_or_shape(1, 7, || items(2))), 2);
    }

    #[test]
    fn a_palette_override_change_clears_every_row() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 1);
        cache.get_or_shape(0, 7, || items(1));

        let overridden = CacheEpoch {
            palette_overrides: vec![(1, [0xff, 0, 0])],
            ..epoch()
        };
        cache.begin_frame(overridden.clone(), 1);
        assert_eq!(col_of(cache.get_or_shape(0, 7, || items(2))), 2);

        // The new epoch then persists: the re-shaped row hits again.
        cache.begin_frame(overridden, 1);
        let mut shaped = false;
        cache.get_or_shape(0, 7, || {
            shaped = true;
            items(3)
        });
        assert!(!shaped);
    }

    #[test]
    fn a_viewport_shrink_truncates_the_row_table() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 3);
        for row in 0..3 {
            cache.get_or_shape(row, 7, || items(1));
        }

        cache.begin_frame(epoch(), 2);
        assert_eq!(cache.rows.len(), 2);

        // Growing back exposes empty slots, not stale rows.
        cache.begin_frame(epoch(), 3);
        assert_eq!(col_of(cache.get_or_shape(2, 7, || items(2))), 2);
    }

    #[test]
    fn the_no_generation_sentinel_never_hits() {
        let mut cache = ShapedLineCache::new();
        cache.begin_frame(epoch(), 1);
        cache.get_or_shape(0, NO_GENERATION, || items(1));
        let reshaped = cache.get_or_shape(0, NO_GENERATION, || items(2));
        assert_eq!(col_of(reshaped), 2);
        assert_eq!(cache.hits, 0);
    }

    #[test]
    fn trace_line_reports_at_most_once_per_interval() {
        let mut cache = ShapedLineCache::new();
        let start = cache.last_trace;
        assert_eq!(cache.trace_line_at(start), None);
        let line = cache
            .trace_line_at(start + TRACE_INTERVAL)
            .expect("interval elapsed");
        assert!(line.starts_with("shape-cache: rows=0 hits=0 misses=0"));
        assert_eq!(cache.trace_line_at(start + TRACE_INTERVAL), None);
    }
}
