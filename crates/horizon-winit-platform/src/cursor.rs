//! `gpui::CursorStyle` -> winit `CursorIcon` mapping. Split out as pure
//! data so it's a colocated unit-test target (AGENTS.md's convention for
//! key/mouse mapping tables) independent of a live window/event loop.

use gpui::CursorStyle;
use winit::window::CursorIcon;

/// Maps every `CursorStyle` variant to the winit/CSS cursor icon that best
/// matches its doc comment (`gpui::platform::CursorStyle`'s docs literally
/// name the CSS cursor value each variant corresponds to, which lines up
/// 1:1 with `cursor-icon`'s naming). Exhaustive match (no wildcard arm) so
/// a new `CursorStyle` variant added upstream fails this crate's build
/// instead of silently falling back to `Default`.
pub(crate) fn cursor_style_to_icon(style: CursorStyle) -> CursorIcon {
    match style {
        CursorStyle::Arrow => CursorIcon::Default,
        CursorStyle::IBeam => CursorIcon::Text,
        CursorStyle::Crosshair => CursorIcon::Crosshair,
        CursorStyle::ClosedHand => CursorIcon::Grabbing,
        CursorStyle::OpenHand => CursorIcon::Grab,
        CursorStyle::PointingHand => CursorIcon::Pointer,
        CursorStyle::ResizeLeft => CursorIcon::WResize,
        CursorStyle::ResizeRight => CursorIcon::EResize,
        CursorStyle::ResizeLeftRight => CursorIcon::EwResize,
        CursorStyle::ResizeUp => CursorIcon::NResize,
        CursorStyle::ResizeDown => CursorIcon::SResize,
        CursorStyle::ResizeUpDown => CursorIcon::NsResize,
        CursorStyle::ResizeUpLeftDownRight => CursorIcon::NwseResize,
        CursorStyle::ResizeUpRightDownLeft => CursorIcon::NeswResize,
        CursorStyle::ResizeColumn => CursorIcon::ColResize,
        CursorStyle::ResizeRow => CursorIcon::RowResize,
        CursorStyle::IBeamCursorForVerticalLayout => CursorIcon::VerticalText,
        CursorStyle::OperationNotAllowed => CursorIcon::NotAllowed,
        CursorStyle::DragLink => CursorIcon::Alias,
        CursorStyle::DragCopy => CursorIcon::Copy,
        CursorStyle::ContextualMenu => CursorIcon::ContextMenu,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `CursorStyle` maps to *some* icon without panicking — mostly a
    /// guard against the match arms above going stale (each arm below is
    /// exercised by iterating every variant would be nicer, but
    /// `CursorStyle` has no `iter()`; the exhaustive match without a
    /// wildcard is what actually guards against drift, this test is a
    /// smoke check on top).
    #[test]
    fn maps_arrow_to_default() {
        assert_eq!(
            cursor_style_to_icon(CursorStyle::Arrow),
            CursorIcon::Default
        );
    }

    #[test]
    fn maps_ibeam_to_text() {
        assert_eq!(cursor_style_to_icon(CursorStyle::IBeam), CursorIcon::Text);
    }

    #[test]
    fn maps_pointing_hand_to_pointer() {
        assert_eq!(
            cursor_style_to_icon(CursorStyle::PointingHand),
            CursorIcon::Pointer
        );
    }

    #[test]
    fn maps_resize_variants_to_directional_resize_icons() {
        assert_eq!(
            cursor_style_to_icon(CursorStyle::ResizeLeftRight),
            CursorIcon::EwResize
        );
        assert_eq!(
            cursor_style_to_icon(CursorStyle::ResizeUpDown),
            CursorIcon::NsResize
        );
        assert_eq!(
            cursor_style_to_icon(CursorStyle::ResizeColumn),
            CursorIcon::ColResize
        );
        assert_eq!(
            cursor_style_to_icon(CursorStyle::ResizeRow),
            CursorIcon::RowResize
        );
    }
}
