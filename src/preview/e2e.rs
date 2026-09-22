//! The preview pane's end-to-end check: a real `wasm32-wasip2` component in
//! a real wasmtime store, driven through GPUI's deterministic test executor
//! with no window on screen.
//!
//! `#[ignore]`d, because it needs the plugin built first (a multi-minute
//! wasm build) — `scripts/check-preview-plugin.sh` builds it, points
//! `HORIZON_PREVIEW_WASM` at the artifact, and runs these.
//!
//! Two traps this kind of test walks into, both hit in the spike this work
//! productizes:
//!
//! * gpui's `NoopTextSystem` reports zero raster bounds for every glyph and
//!   gpui skips painting those, so a Noop-backed surface receives no glyph
//!   primitives at all and the test is blind to text. [`ProbeTextSystem`]
//!   reports a real raster box and maps one character to one glyph id.
//! * display-list order is not reading order: gpui batches sprites before
//!   the scene is serialized, so glyphs from different strings interleave.
//!   Every assertion here is on the character multiset, never on order.
//!
//! What driving input asserts beyond the keystroke itself: that the guest
//! settles at all. A guest paces no frames of its own — the host runs one
//! turn per exchange with it, and a turn that draws leads to the next one —
//! so a view that asks for another frame from inside the frame it is
//! drawing (any repeating `gpui::Animation`, e.g. gpui-component's
//! `Spinner`) keeps handing itself work and `settle` never returns. Nothing
//! turns a quiet guest, so such a view looks idle until the first input
//! event arrives. A test that hangs here after a `press` is reporting that,
//! not a slow guest; `.config/nextest.toml` turns the hang into a failure.
//! The board pane's `board-list` is in that state today — its rows draw the
//! shipped animated activity indicator — so it is driven with no input
//! here. See `docs/preview-pane-design.md`, "Frame pacing".
//!
//! What it cannot assert: the painted *colors*. `Surface::scene_summary()`
//! counts primitives and reports glyph ids; the primitives' colors are not
//! exposed. The sample preview paints its accent role's hex as text, so a
//! theme change is asserted through the glyph stream instead.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use embedded_gpui::surface::Geometry;
use embedded_gpui::{
    PluginHost, PluginHostHandle as _, PluginInstance, PluginOptions, SceneSummary, Surface,
};
use gpui::{
    px, size, AppContext as _, Bounds, DevicePixels, Entity, Font, FontId, FontMetrics, FontRun,
    GlyphId, LineLayout, Pixels, PlatformTextSystem, Point, RenderGlyphParams, ShapedGlyph,
    ShapedRun, Size, TestAppContext, TextRenderingMode,
};
use horizon_config::{RawConfig, RawThemeConfig};

use crate::board_next::previews as board_next;
use crate::board_pane::previews as board;
use crate::preview::host::{PreviewHostRoot, PreviewThemeSource};
use crate::preview::schema::{PreviewPlugin, PreviewPluginCaller as _};
use crate::preview::{registry, sample};

// ---------------------------------------------------------------------------
// A text system that makes rendered text visible to assertions
// ---------------------------------------------------------------------------

struct ProbeTextSystem;

const UNITS_PER_EM: u32 = 1000;
const ADVANCE_UNITS: f32 = 500.0;

impl ProbeTextSystem {
    fn metrics() -> FontMetrics {
        FontMetrics {
            units_per_em: UNITS_PER_EM,
            ascent: 800.0,
            descent: -200.0,
            line_gap: 0.0,
            underline_position: -100.0,
            underline_thickness: 50.0,
            cap_height: 700.0,
            x_height: 500.0,
            bounding_box: Bounds {
                origin: Point { x: 0.0, y: -200.0 },
                size: Size {
                    width: 500.0,
                    height: 1000.0,
                },
            },
        }
    }
}

impl PlatformTextSystem for ProbeTextSystem {
    fn add_fonts(&self, _fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        Ok(())
    }

    fn all_font_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn font_id(&self, _descriptor: &Font) -> anyhow::Result<FontId> {
        Ok(FontId(1))
    }

    fn font_metrics(&self, _font_id: FontId) -> FontMetrics {
        Self::metrics()
    }

    fn typographic_bounds(
        &self,
        _font_id: FontId,
        _glyph_id: GlyphId,
    ) -> anyhow::Result<Bounds<f32>> {
        Ok(Bounds {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: ADVANCE_UNITS,
                height: 700.0,
            },
        })
    }

    fn advance(&self, _font_id: FontId, _glyph_id: GlyphId) -> anyhow::Result<Size<f32>> {
        Ok(size(ADVANCE_UNITS, 0.0))
    }

    /// The whole point: the glyph id *is* the character.
    fn glyph_for_char(&self, _font_id: FontId, ch: char) -> Option<GlyphId> {
        Some(GlyphId(ch as u32))
    }

    fn glyph_raster_bounds(
        &self,
        _params: &RenderGlyphParams,
    ) -> anyhow::Result<Bounds<DevicePixels>> {
        Ok(Bounds {
            origin: Point {
                x: DevicePixels(0),
                y: DevicePixels(-8),
            },
            size: Size {
                width: DevicePixels(6),
                height: DevicePixels(10),
            },
        })
    }

    fn rasterize_glyph(
        &self,
        _params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> anyhow::Result<(Size<DevicePixels>, Vec<u8>)> {
        let count = (raster_bounds.size.width.0 * raster_bounds.size.height.0).max(0) as usize;
        Ok((raster_bounds.size, vec![0u8; count]))
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let font_id = runs.first().map(|run| run.font_id).unwrap_or(FontId(1));
        let em_width = font_size * (ADVANCE_UNITS / UNITS_PER_EM as f32);
        let mut position = px(0.);
        let mut glyphs = Vec::new();
        for (index, ch) in text.char_indices() {
            glyphs.push(ShapedGlyph {
                id: GlyphId(ch as u32),
                position: gpui::point(position, px(0.)),
                index,
                is_emoji: false,
            });
            position += em_width;
        }
        let metrics = Self::metrics();
        let mut shaped = Vec::new();
        if !glyphs.is_empty() {
            shaped.push(ShapedRun { font_id, glyphs });
        } else {
            position = px(0.);
        }
        LineLayout {
            font_size,
            width: position,
            ascent: font_size * (metrics.ascent / UNITS_PER_EM as f32),
            descent: font_size * (metrics.descent / UNITS_PER_EM as f32),
            runs: shaped,
            len: text.len(),
        }
    }

    fn recommended_rendering_mode(
        &self,
        _font_id: FontId,
        _font_size: Pixels,
    ) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Where `scripts/check-preview-plugin.sh` leaves the built component.
fn built_artifact() -> PathBuf {
    let path = std::env::var_os("HORIZON_PREVIEW_WASM").expect(
        "HORIZON_PREVIEW_WASM must name the built preview plugin -- run \
         scripts/check-preview-plugin.sh, which builds it and sets this",
    );
    PathBuf::from(path)
}

/// A scratch copy the test overwrites, so the built artifact is never
/// touched and neither parallel runs nor two tests in one process collide.
fn live_artifact(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "horizon-preview-e2e-{tag}-{}.wasm",
        std::process::id()
    ))
}

const SLOT: Geometry = Geometry {
    width: 480.,
    height: 360.,
    scale_factor: 1.,
};

/// The board previews get a tall slot: gpui culls primitives outside the
/// content mask, so a scroll region only paints what fits. The detail
/// preview's thread sits below a multi-paragraph body, and an assertion on
/// text that scrolled out of view would be an assertion on nothing.
const BOARD_SLOT: Geometry = Geometry {
    width: 900.,
    height: 1600.,
    scale_factor: 1.,
};

/// One loaded plugin plus the weak handle that says whether its wasmtime
/// store is really gone: the store owns the text system `PluginOptions` was
/// built with, so a `Weak` to it fails to upgrade once the instance drops.
struct Loaded {
    host: Entity<PluginHost>,
    _root: Entity<PreviewHostRoot>,
    /// Held so the object the guest observes for theme changes outlives the
    /// guest's remote.
    _theme_source: Entity<PreviewThemeSource>,
    text_system: Weak<ProbeTextSystem>,
}

fn load(path: &std::path::Path, preview: &str, cx: &mut TestAppContext) -> anyhow::Result<Loaded> {
    let text_system = Arc::new(ProbeTextSystem);
    let weak = Arc::downgrade(&text_system);
    let options = PluginOptions::new(text_system);
    let instance = cx.update(|_| PluginInstance::new(path, options))?;
    let host = cx.new(|cx| PluginHost::new(instance, cx));
    let (root, theme_source) = cx.update(|cx| {
        let theme_source = cx.new(PreviewThemeSource::new);
        let theme_ref = host.share(&theme_source, cx);
        let root =
            cx.new(|_| PreviewHostRoot::new(theme_source.clone(), theme_ref, preview.to_string()));
        host.share_root(&root, cx);
        (root, theme_source)
    });
    Ok(Loaded {
        host,
        _root: root,
        _theme_source: theme_source,
        text_system: weak,
    })
}

fn settle(cx: &mut TestAppContext) {
    for _ in 0..10 {
        cx.executor().run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(100));
    }
    cx.executor().run_until_parked();
}

/// Send one keystroke to the guest's view and let it settle.
///
/// A guest paces no frames of its own, so a view that asks for another
/// frame from inside the frame it is drawing never lets [`settle`] return
/// (see the module doc). Driving a keystroke is therefore also the check
/// that the view under test does not.
fn press(surface: &Entity<Surface>, key: &str, cx: &mut TestAppContext) {
    use embedded_gpui::surface::{KeyEvent, Keystroke, ViewApiCaller as _};

    let view = surface
        .read_with(cx, |surface, _| surface.view().cloned())
        .expect("the guest attached a view");
    cx.update(|cx| {
        view.key(
            KeyEvent::Down {
                keystroke: Keystroke {
                    modifiers: Default::default(),
                    key: key.to_string(),
                    key_char: Some(key.to_string()),
                },
                is_held: false,
            },
            cx,
        )
    });
    settle(cx);
}

/// Hand the guest a surface of `slot` and drive one frame on it.
fn mount(
    host: &Entity<PluginHost>,
    surface: &Entity<Surface>,
    slot: Geometry,
    cx: &mut TestAppContext,
) {
    let surface_ref = cx.update(|cx| host.share(surface, cx));
    let plugin = cx.update(|cx| host.root::<PreviewPlugin>(cx));
    cx.update(|cx| plugin.mount(surface_ref, cx));
    settle(cx);
    let view = surface
        .read_with(cx, |surface, _| surface.view().cloned())
        .expect("the guest attached a view");
    cx.update(|cx| {
        use embedded_gpui::surface::ViewApiCaller as _;
        view.resize(slot, cx);
    });
    settle(cx);
}

fn summary(surface: &Entity<Surface>, cx: &mut TestAppContext) -> SceneSummary {
    surface
        .read_with(cx, |surface, _| surface.scene_summary())
        .expect("the surface received a display list")
}

/// Every character a guest painted, with multiplicities (see the module
/// doc: order is meaningless, multiplicity is not).
fn glyph_counts(summary: &SceneSummary) -> BTreeMap<char, usize> {
    let mut counts = BTreeMap::new();
    for id in &summary.glyph_ids {
        if let Some(ch) = char::from_u32(*id) {
            *counts.entry(ch).or_insert(0) += 1;
        }
    }
    counts
}

fn word_counts(word: &str) -> BTreeMap<char, usize> {
    let mut counts = BTreeMap::new();
    for ch in word.chars() {
        *counts.entry(ch).or_insert(0) += 1;
    }
    counts
}

/// Whether `word` could have been painted: every character present at least
/// as often as the word needs it.
fn painted(counts: &BTreeMap<char, usize>, word: &str) -> bool {
    word_counts(word)
        .into_iter()
        .all(|(ch, needed)| counts.get(&ch).copied().unwrap_or(0) >= needed)
}

fn count_of(counts: &BTreeMap<char, usize>, ch: char) -> usize {
    counts.get(&ch).copied().unwrap_or(0)
}

/// `after - before`, as a signed per-character map with the zeros dropped.
fn delta(before: &BTreeMap<char, usize>, after: &BTreeMap<char, usize>) -> BTreeMap<char, i64> {
    let mut out = BTreeMap::new();
    for ch in before.keys().chain(after.keys()) {
        let change = count_of(after, *ch) as i64 - count_of(before, *ch) as i64;
        if change != 0 {
            out.insert(*ch, change);
        }
    }
    out
}

fn text_delta(from: &str, to: &str) -> BTreeMap<char, i64> {
    delta(&word_counts(from), &word_counts(to))
}

/// Waits for the live instance count to settle at `expected`, up to a
/// second of real time: an instance's epoch ticker exits on its next tick
/// after the drop, not synchronously. Returns the last count seen, so a
/// caller asserts on it and gets the real number in the failure message.
fn await_instance_threads(expected: usize) -> usize {
    for _ in 0..100 {
        let live = live_instance_threads();
        if live == expected {
            return live;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    live_instance_threads()
}

/// One epoch ticker thread per live `PluginInstance`, so this counts live
/// instances in this process.
fn live_instance_threads() -> usize {
    let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|comm| comm.trim().starts_with("embedded_gpui"))
        })
        .count()
}

/// A `[theme]` section with one distinctive accent.
fn theme_with_accent(hex: &str) -> RawThemeConfig {
    let mut theme = RawThemeConfig::default();
    theme.colors.insert("accent".to_string(), hex.to_string());
    theme
}

/// The text the sample preview paints for `theme`'s accent role, computed
/// by running the same resolver the guest runs.
fn expected_accent_text(theme: &RawThemeConfig) -> String {
    crate::theme::reload_from(&RawConfig {
        theme: theme.clone(),
        ..RawConfig::default()
    });
    sample::accent_swatch_text()
}

// ---------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------

#[gpui::test]
#[ignore = "needs the preview plugin built; run scripts/check-preview-plugin.sh"]
async fn preview_plugin_paints_reacts_to_the_theme_and_reloads(cx: &mut TestAppContext) {
    let built = built_artifact();
    let live = live_artifact("sample");
    std::fs::copy(&built, &live).expect("stage the built artifact");

    let baseline_threads = live_instance_threads();

    // gpui-component's global theme is what `theme::live::apply_scheme`
    // re-projects onto; the guest runs its own copy inside wasm.
    cx.update(gpui_component::init);

    let first_theme = theme_with_accent("#0055ff");
    let first_accent = expected_accent_text(&first_theme);
    cx.update(|cx| {
        crate::theme::live::apply_scheme(
            &RawConfig {
                theme: first_theme.clone(),
                ..RawConfig::default()
            },
            cx,
        )
    });

    // --- the sample preview paints ---------------------------------------
    let loaded = load(&live, sample::NAME, cx).expect("the built artifact instantiates");
    let surface = cx.new(Surface::new);
    let surface_id = surface.entity_id();
    mount(&loaded.host, &surface, SLOT, cx);

    let scene = summary(&surface, cx);
    assert!(scene.quads > 0, "the preview painted no quads: {scene:?}");
    assert!(scene.glyphs > 0, "the preview painted no glyphs: {scene:?}");
    assert!(
        scene.images > 0,
        "the icon SVG did not reach the host as an image: {scene:?}"
    );
    let before = glyph_counts(&scene);
    assert!(
        painted(&before, sample::LABEL),
        "painted glyphs cannot spell the sample's label: {before:?}"
    );
    assert!(
        painted(&before, &first_accent),
        "the guest did not resolve the host's accent: expected {first_accent:?} in {before:?}"
    );

    // --- a theme change from the host changes what the guest paints -------
    let second_theme = theme_with_accent("#cc7700");
    let second_accent = expected_accent_text(&second_theme);
    cx.update(|cx| {
        crate::theme::live::apply_scheme(
            &RawConfig {
                theme: second_theme.clone(),
                ..RawConfig::default()
            },
            cx,
        )
    });
    settle(cx);

    let after = glyph_counts(&summary(&surface, cx));
    assert!(
        painted(&after, &second_accent),
        "the theme change never reached the guest: expected {second_accent:?} in {after:?}"
    );
    assert_eq!(
        delta(&before, &after),
        text_delta(&first_accent, &second_accent),
        "the painted text changed by something other than the accent swatch"
    );

    // --- swapping the artifact reloads in-process -------------------------
    // The out-of-band rebuild: the same path gets a fresh copy of the
    // artifact, the loaded plugin is dropped, and the file is loaded again
    // onto the same surface entity.
    std::fs::copy(&built, &live).expect("stage the rebuilt artifact");
    let first_store = loaded.text_system.clone();
    // Inside an app update: gpui flushes entity releases at the end of one,
    // so a drop outside any update leaves the host entity queued and the
    // store alive.
    cx.update(|_| drop(loaded));
    settle(cx);
    assert!(
        first_store.upgrade().is_none(),
        "the old wasmtime store outlived its PluginHost"
    );
    assert_eq!(
        await_instance_threads(baseline_threads),
        baseline_threads,
        "dropping the plugin left its instance running"
    );

    let reloaded = load(&live, sample::NAME, cx).expect("the rebuilt artifact instantiates");
    mount(&reloaded.host, &surface, SLOT, cx);

    let reloaded_counts = glyph_counts(&summary(&surface, cx));
    assert!(
        painted(&reloaded_counts, sample::LABEL),
        "the reloaded preview painted nothing recognizable: {reloaded_counts:?}"
    );
    assert!(
        painted(&reloaded_counts, &second_accent),
        "the reloaded guest did not pick up the current theme: {reloaded_counts:?}"
    );
    assert_eq!(
        surface.entity_id(),
        surface_id,
        "the host's surface entity did not survive the reload"
    );
    // One live instance, not two: the reload replaced the old one rather
    // than stacking on it.
    assert_eq!(
        await_instance_threads(baseline_threads + 1),
        baseline_threads + 1,
        "reloading leaked a plugin instance"
    );

    let plugin = cx.update(|cx| reloaded.host.root::<PreviewPlugin>(cx));
    let names = cx.update(|cx| plugin.preview_names(cx));
    settle(cx);
    assert_eq!(
        names.await.expect("preview_names"),
        registry::previews()
            .iter()
            .map(|preview| preview.name.to_string())
            .collect::<Vec<_>>(),
        "the plugin does not carry the registry's previews"
    );

    // --- a broken artifact fails the load, it does not load something -----
    // Different bytes on the same path produce a different outcome, which
    // is what says a load reads the file rather than reusing a module it
    // already has; the error is the caller's to report (the pane turns it
    // into its status line) and nothing else is disturbed.
    std::fs::write(&live, b"not a wasm component").expect("stage a broken artifact");
    assert!(
        load(&live, sample::NAME, cx).is_err(),
        "a broken artifact must fail to load, not load something"
    );
    assert!(
        summary(&surface, cx).glyphs > 0,
        "the failed load disturbed the scene the loaded plugin had painted"
    );

    cx.update(|_| drop(reloaded));
    settle(cx);
    std::fs::remove_file(&live).ok();
}

#[gpui::test]
#[ignore = "needs the preview plugin built; run scripts/check-preview-plugin.sh"]
async fn preview_plugin_paints_the_board_over_its_sample_store(cx: &mut TestAppContext) {
    let built = built_artifact();
    let live = live_artifact("board");
    std::fs::copy(&built, &live).expect("stage the built artifact");

    cx.update(gpui_component::init);
    cx.update(|cx| crate::theme::live::apply_scheme(&RawConfig::default(), cx));

    // --- the list shows rows folded out of the sample events -------------
    let list = load(&live, board::LIST, cx).expect("the board list preview instantiates");
    let list_surface = cx.new(Surface::new);
    mount(&list.host, &list_surface, BOARD_SLOT, cx);
    let list_scene = summary(&list_surface, cx);
    assert!(
        list_scene.images > 0,
        "no session-activity icon reached the host: {list_scene:?}"
    );
    let rows = glyph_counts(&list_scene);
    assert!(
        painted(&rows, board::LIST_PROBE_LATIN),
        "the board list painted no row carrying a Latin sample title: {rows:?}"
    );
    assert!(
        painted(&rows, board::LIST_PROBE_JAPANESE),
        "the board list painted no row carrying a Japanese sample title: {rows:?}"
    );
    cx.update(|_| drop(list));
    settle(cx);

    // --- the same view over an empty store -------------------------------
    // The Japanese probe is the discriminator in both directions: its
    // katakana appear in no other string any board preview paints.
    let empty = load(&live, board::LIST_EMPTY, cx).expect("the empty board preview instantiates");
    let empty_surface = cx.new(Surface::new);
    mount(&empty.host, &empty_surface, BOARD_SLOT, cx);
    let blank = glyph_counts(&summary(&empty_surface, cx));
    assert!(
        !blank.is_empty(),
        "the empty board painted no chrome at all"
    );
    assert!(
        !painted(&blank, board::LIST_PROBE_JAPANESE),
        "the empty board painted a sample row: {blank:?}"
    );
    cx.update(|_| drop(empty));
    settle(cx);

    // --- the detail view, reached by confirming the row ------------------
    let detail = load(&live, board::DETAIL, cx).expect("the board detail preview instantiates");
    let detail_surface = cx.new(Surface::new);
    mount(&detail.host, &detail_surface, BOARD_SLOT, cx);
    let open = glyph_counts(&summary(&detail_surface, cx));
    assert!(
        painted(&open, board::DETAIL_BODY_PROBE),
        "the detail view painted no item body: {open:?}"
    );
    assert!(
        painted(&open, board::DETAIL_COMMENT_PROBE),
        "the detail view painted no comment thread: {open:?}"
    );
    assert!(
        !painted(&open, board::LIST_PROBE_JAPANESE),
        "the detail view never replaced the list: {open:?}"
    );

    cx.update(|_| drop(detail));
    settle(cx);
    std::fs::remove_file(&live).ok();
}

#[gpui::test]
#[ignore = "needs the preview plugin built; run scripts/check-preview-plugin.sh"]
async fn preview_plugin_paints_the_board_next_prototype(cx: &mut TestAppContext) {
    let built = built_artifact();
    let live = live_artifact("board-next");
    std::fs::copy(&built, &live).expect("stage the built artifact");

    cx.update(gpui_component::init);
    cx.update(|cx| crate::theme::live::apply_scheme(&RawConfig::default(), cx));

    // The leading character of each title occurs in exactly one string the
    // sample board can paint (held by that module's own tests), so counting
    // it separates a list row from a row plus the thread header.
    let first_marker = board_next::FIRST_TASK_TITLE
        .chars()
        .next()
        .expect("a title");
    let second_marker = board_next::SECOND_TASK_TITLE
        .chars()
        .next()
        .expect("a title");

    // --- list and thread are one view ------------------------------------
    let prototype = load(&live, board_next::NEXT, cx).expect("the prototype instantiates");
    let surface = cx.new(Surface::new);
    mount(&prototype.host, &surface, BOARD_SLOT, cx);
    let master = glyph_counts(&summary(&surface, cx));
    assert_eq!(
        count_of(&master, first_marker),
        2,
        "the selected task is painted as a list row and as the thread header: {master:?}"
    );
    assert_eq!(
        count_of(&master, second_marker),
        1,
        "an unselected task is painted as a list row only: {master:?}"
    );
    assert!(
        painted(&master, board_next::THREAD_PROBE),
        "the selected task's thread is not painted next to the list: {master:?}"
    );

    // --- `j` moves the selection, and the thread follows it --------------
    press(&surface, "j", cx);
    let moved = glyph_counts(&summary(&surface, cx));
    assert_eq!(
        count_of(&moved, second_marker),
        2,
        "`j` did not carry the thread header onto the next task: {moved:?}"
    );
    assert_eq!(
        count_of(&moved, first_marker),
        1,
        "the task `j` left is still painted as the thread header: {moved:?}"
    );
    assert!(
        !painted(&moved, board_next::THREAD_PROBE),
        "the previous task's thread is still painted: {moved:?}"
    );
    cx.update(|_| drop(prototype));
    settle(cx);

    // --- the same view over an empty store -------------------------------
    let empty = load(&live, board_next::NEXT_EMPTY, cx).expect("the empty prototype instantiates");
    let empty_surface = cx.new(Surface::new);
    mount(&empty.host, &empty_surface, BOARD_SLOT, cx);
    let blank = glyph_counts(&summary(&empty_surface, cx));
    assert!(!blank.is_empty(), "the empty prototype painted no chrome");
    assert_eq!(
        count_of(&blank, first_marker),
        0,
        "the empty prototype painted a sample row: {blank:?}"
    );
    cx.update(|_| drop(empty));
    settle(cx);

    // --- a long post is folded to its first lines ------------------------
    let long = load(&live, board_next::NEXT_LONG_THREAD, cx)
        .expect("the long-thread preview instantiates");
    let long_surface = cx.new(Surface::new);
    mount(&long.host, &long_surface, BOARD_SLOT, cx);
    let folded = glyph_counts(&summary(&long_surface, cx));
    assert!(
        painted(&folded, board_next::FOLDED_PROBE),
        "the long post's first line is not painted: {folded:?}"
    );
    assert!(
        !painted(&folded, board_next::DEEP_PROBE),
        "the long post was not folded: {folded:?}"
    );

    // --- `e` unfolds every folded post in the open thread ----------------
    press(&long_surface, "e", cx);
    let unfolded = glyph_counts(&summary(&long_surface, cx));
    assert!(
        painted(&unfolded, board_next::DEEP_PROBE),
        "`e` did not unfold the long post: {unfolded:?}"
    );

    cx.update(|_| drop(long));
    settle(cx);
    std::fs::remove_file(&live).ok();
}
