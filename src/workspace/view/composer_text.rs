use std::any::Any;
use std::borrow::Cow;

use crate::ui::fonts::font_family;
use crate::ui::theme;
use crate::workspace::AgentDraft;
use floem::context::{ComputeLayoutCx, LayoutCx, PaintCx, UpdateCx};
use floem::peniko::kurbo::{Point, Rect};
use floem::peniko::Color;
use floem::reactive::{create_updater, RwSignal, SignalWith};
use floem::style::Style;
use floem::taffy::tree::NodeId;
use floem::text::{Attrs, AttrsList, FamilyOwned, LineHeightValue, TextLayout, Wrap};
use floem::{IntoView, View, ViewId};
use floem_renderer::Renderer;

const COMPOSER_FONT_SIZE: f32 = 12.0;
const COMPOSER_LINE_HEIGHT: f32 = 1.2;
const CURSOR_WIDTH: f64 = 1.5;
const PLACEHOLDER_TEXT: &str = "Message agent...";

/// One resolved frame of composer content: `draft.text` with IME `preedit`
/// spliced in at the cursor (the same splice the pre-multiline design did
/// across four separate `label`s, here as one string feeding one
/// `TextLayout`), or the placeholder string when both are empty. `cursor`
/// is always a byte offset into `text` -- see `ComposerTextView::paint`'s
/// use of `TextLayout::hit_position`.
struct ComposerContent {
    text: String,
    cursor: usize,
    show_cursor: bool,
    color: Color,
}

/// Splices `preedit` into `text` at `cursor` (a byte offset, always on a
/// char boundary per `AgentDraft`'s invariant) and returns the spliced
/// string plus the byte offset right after the inserted preedit -- where
/// the caret should render while composing, matching where a committed
/// character would actually land (`insert_agent_draft_text` always inserts
/// at `draft.cursor`, unchanged by an in-flight composition). Pulled out of
/// `resolve_content` so the splice/cursor arithmetic is unit-testable
/// without a live `RwSignal` or the reactive runtime.
fn splice_preedit(text: &str, cursor: usize, preedit: &str) -> (String, usize) {
    let mut spliced = text.to_string();
    spliced.insert_str(cursor, preedit);
    (spliced, cursor + preedit.len())
}

fn resolve_content(
    draft: RwSignal<AgentDraft>,
    preedit: impl Fn() -> Option<String>,
    active: impl Fn() -> bool,
) -> ComposerContent {
    let (text, cursor) = draft.with(|draft| match preedit() {
        Some(preedit_text) => splice_preedit(&draft.text, draft.cursor, &preedit_text),
        None => (draft.text.clone(), draft.cursor),
    });
    let show_cursor = active();
    if text.is_empty() {
        ComposerContent {
            text: PLACEHOLDER_TEXT.to_string(),
            cursor: 0,
            show_cursor,
            color: theme::text_subtle(),
        }
    } else {
        ComposerContent {
            text,
            cursor,
            show_cursor,
            color: theme::text_primary(),
        }
    }
}

/// The composer's text: a single `TextLayout` word-wrapped to the view's
/// assigned width, with a caret drawn at `draft.cursor` (plus any spliced-in
/// IME preedit) via `TextLayout::hit_position` -- the same primitive
/// floem's own `text_input` uses for its cursor x (see
/// `docs/agent-composer-cursor-design.md`). Replaces the old fixed-height
/// `h_stack` of four labels plus a caret-bar `empty()`, which could neither
/// wrap nor grow: flex placement of intrinsically-sized labels only ever
/// produced one visual line.
///
/// This is a custom `View` (rather than a wrapping `label` plus a
/// separately positioned caret overlay) so the text used to draw the caret
/// is *always* the exact `TextLayout` used to draw the glyphs -- one
/// `hit_position` call against one buffer, with no second, independently
/// laid-out `TextLayout` that could drift out of sync with the visible
/// wrap.
pub(super) fn composer_text_view(
    draft: RwSignal<AgentDraft>,
    preedit: impl Fn() -> Option<String> + 'static + Copy,
    active: impl Fn() -> bool + 'static + Copy,
) -> impl IntoView {
    let id = ViewId::new();
    let compute = move || resolve_content(draft, preedit, active);
    let initial = create_updater(compute, move |content| id.update_state(content));
    ComposerTextView {
        id,
        content: initial,
        text_layout: None,
        wrapped_layout: None,
        available_width: None,
        text_node: None,
    }
}

struct ComposerTextView {
    id: ViewId,
    content: ComposerContent,
    /// `content.text` laid out with no width constraint -- gives the
    /// natural size (one visual line per real `\n` in the text, none of
    /// them word-wrapped yet), and is the buffer `wrapped_layout` is cloned
    /// from once this view's assigned width is known.
    text_layout: Option<TextLayout>,
    /// `text_layout` re-wrapped to `available_width`. `None` until the
    /// first `compute_layout` pass resolves a width; this is what's
    /// actually painted and hit-tested once set (see `effective_layout`).
    wrapped_layout: Option<TextLayout>,
    available_width: Option<f32>,
    text_node: Option<NodeId>,
}

impl ComposerTextView {
    fn build_attrs(&self) -> AttrsList {
        let family: Vec<FamilyOwned> = FamilyOwned::parse_list(font_family()).collect();
        let attrs = Attrs::new()
            .color(self.content.color)
            .family(&family)
            .font_size(COMPOSER_FONT_SIZE)
            .line_height(LineHeightValue::Normal(COMPOSER_LINE_HEIGHT));
        AttrsList::new(attrs)
    }

    fn rebuild_text_layout(&mut self) {
        let mut layout = TextLayout::new();
        layout.set_text(&self.content.text, self.build_attrs(), None);
        self.text_layout = Some(layout);
        self.wrapped_layout = None;
        self.available_width = None;
    }

    /// The `wrapped_layout` once a width is known, else the unwrapped
    /// `text_layout` -- always `Some` by the time `layout()` has run once.
    fn effective_layout(&self) -> &TextLayout {
        self.wrapped_layout
            .as_ref()
            .unwrap_or_else(|| self.text_layout.as_ref().expect("layout() runs first"))
    }
}

impl View for ComposerTextView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn debug_name(&self) -> Cow<'static, str> {
        "ComposerTextView".into()
    }

    fn update(&mut self, _cx: &mut UpdateCx, state: Box<dyn Any>) {
        if let Ok(content) = state.downcast::<ComposerContent>() {
            self.content = *content;
            // Always rebuilt, even if only `cursor`/`show_cursor` changed
            // (an arrow key press, or focus moving in/out): rebuilding is
            // cheap for composer-sized text, and keeping a single
            // invalidation path avoids a second "did the text/color really
            // change" comparison that `Color` would need `PartialEq` for.
            self.text_layout = None;
            self.wrapped_layout = None;
            self.available_width = None;
            self.id.request_layout();
        }
    }

    fn layout(&mut self, cx: &mut LayoutCx) -> NodeId {
        cx.layout_node(self.id(), true, |_cx| {
            if self.text_layout.is_none() {
                self.rebuild_text_layout();
            }
            if self.text_node.is_none() {
                self.text_node = Some(self.id.new_taffy_node());
            }
            let text_node = self.text_node.unwrap();
            let size = self.effective_layout().size();
            let style = Style::new()
                .width(size.width.ceil() as f32)
                .height(size.height as f32)
                .to_taffy_style();
            self.id.set_taffy_style(text_node, style);
            vec![text_node]
        })
    }

    /// Runs after taffy has resolved this view's own assigned width (from
    /// the external `.style(|s| s.width_full())` the caller applies) --
    /// exactly the two-pass trick floem's own `Label` uses for
    /// `TextOverflow::Wrap` (`floem`'s vendored `views/label.rs`): the
    /// first `layout()` pass sizes the leaf text node off the *unwrapped*
    /// natural layout, this pass compares that against the width taffy
    /// actually gave the view, and re-wraps + requests another `layout()`
    /// pass if it's narrower. Converges in one extra pass since the second
    /// `layout()` call reports the already-wrapped size, which matches the
    /// same `available_width` next time this runs.
    fn compute_layout(&mut self, _cx: &mut ComputeLayoutCx) -> Option<Rect> {
        let available_width = self.id.get_layout().unwrap_or_default().size.width;
        if available_width > 0.0 && self.available_width != Some(available_width) {
            let mut wrapped = self
                .text_layout
                .clone()
                .expect("layout() always runs before compute_layout()");
            wrapped.set_wrap(Wrap::WordOrGlyph);
            wrapped.set_size(available_width, f32::MAX);
            self.wrapped_layout = Some(wrapped);
            self.available_width = Some(available_width);
            self.id.request_layout();
        }
        None
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let Some(text_node) = self.text_node else {
            return;
        };
        let location = self
            .id
            .taffy_layout(text_node)
            .map(|layout| layout.location)
            .unwrap_or_default();
        let origin = Point::new(location.x as f64, location.y as f64);

        let text_layout = self.effective_layout();
        cx.draw_text(text_layout, origin);

        if self.content.show_cursor {
            let hit = text_layout.hit_position(self.content.cursor);
            let top = origin.y + hit.point.y - hit.glyph_ascent;
            let height = hit.glyph_ascent + hit.glyph_descent;
            let left = origin.x + hit.point.x;
            let rect = Rect::new(left, top, left + CURSOR_WIDTH, top + height);
            cx.fill(&rect, theme::accent(), 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::splice_preedit;

    #[test]
    fn splice_preedit_inserts_at_cursor_and_returns_the_post_preedit_offset() {
        let (spliced, cursor) = splice_preedit("hello world", 5, "!!!");
        assert_eq!(spliced, "hello!!! world");
        // The caret renders right after the composing preedit, not at the
        // original `draft.cursor` -- matches where the IME visually shows
        // its insertion point while composing.
        assert_eq!(cursor, 8);
    }

    #[test]
    fn splice_preedit_at_the_very_start_keeps_the_rest_of_the_text_intact() {
        let (spliced, cursor) = splice_preedit("world", 0, "hello ");
        assert_eq!(spliced, "hello world");
        assert_eq!(cursor, 6);
    }

    #[test]
    fn splice_preedit_handles_multibyte_japanese_text() {
        // Test data as multibyte/IME-relevant Japanese, per this repo's own
        // convention for exercising char-boundary correctness.
        let (spliced, cursor) = splice_preedit("こんにちは", "こんに".len(), "しか");
        assert_eq!(spliced, "こんにしかちは");
        assert_eq!(cursor, "こんにしか".len());
    }
}
