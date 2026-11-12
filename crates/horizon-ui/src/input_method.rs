//! Where the input method's candidates go: by the text being edited, in a
//! text field of the document's own or in an element that takes text
//! itself.

use blitz_dom::BaseDocument;

use crate::shared::Shared;

/// [`App::ime_cursor_area`], of a document just laid out.
pub(crate) fn ime_cursor_area(document: &BaseDocument) -> Option<(f32, f32, f32, f32)> {
    let node = document.get_node(document.get_focussed_node_id()?)?;
    let input = node.element_data()?.text_input_data()?;
    // The editor lays its text out in device pixels, scrolled inside
    // the input along the axis it runs out on.
    let scale = document.viewport().scale_f64();
    let area = input.editor.ime_cursor_area();
    let layout = node.final_layout();
    // The content box's corner: the node's place on the page, in from
    // its border and padding. (`Layout::content_box_x` is not that
    // inset alone: it has the node's own location in it already.)
    let origin = node.absolute_position(
        layout.border.left + layout.padding.left,
        layout.border.top + layout.padding.top,
    );
    let (scroll_x, scroll_y) = match input.is_multiline {
        true => (0., input.scroll_offset),
        false => (input.scroll_offset, 0.),
    };
    Some((
        origin.x + (area.x0 / scale) as f32 - scroll_x,
        origin.y + (area.y0 / scale) as f32 - scroll_y,
        ((area.x1 - area.x0) / scale) as f32,
        ((area.y1 - area.y0) / scale) as f32,
    ))
}

/// Tells the shell where the text being edited is. blitz-dom tells it
/// where an input is when it takes focus, and not again: the input
/// method's candidates would stay there while the caret moves on. Done
/// after a layout, when the boxes it reads are those of the document as it
/// is.
pub(crate) fn place_input_method(shared: &Shared, document: &BaseDocument) {
    if let Some((x, y, width, height)) = ime_cursor_area(document) {
        (document.shell_provider).set_ime_cursor_area(x, y, width, height);
        shared.composing_for.set(None);
        return;
    }
    // An element that takes text without being a text field.
    let focused = document.get_focussed_node_id();
    let area = focused.and_then(|id| {
        let area = shared.text_targets.borrow().get(&id)?();
        let origin = document.get_node(id)?.absolute_position(0., 0.);
        Some(area.map(|(x, y, width, height)| (origin.x + x, origin.y + y, width, height)))
    });
    match area {
        Some(area) => {
            if shared.composing_for.get() != focused {
                document.shell_provider.set_ime_enabled(true);
                shared.composing_for.set(focused);
            }
            if let Some((x, y, width, height)) = area {
                (document.shell_provider).set_ime_cursor_area(x, y, width, height);
            }
        }
        None if shared.composing_for.take().is_some() => {
            document.shell_provider.set_ime_enabled(false);
        }
        None => {}
    }
}
