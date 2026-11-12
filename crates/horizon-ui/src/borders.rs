//! Borders a device pixel wide, at a fractional display scale.

use blitz_dom::BaseDocument;

/// Gives back the borders layout rounded away.
///
/// At a fractional display scale a border one device pixel wide is less
/// than one CSS pixel, and blitz-dom rounds layout to whole CSS pixels from
/// each box's position: where both edges of such a border land on the same
/// pixel, its width comes out 0 and that side is not drawn. A side that has
/// a width before rounding and none after is given one device pixel.
///
/// It visits every node after every resolve, which is the cost of doing
/// this from outside: the rounding itself is inside blitz-dom's resolve.
pub(crate) fn restore_thin_borders(document: &mut BaseDocument) {
    let scale = document.viewport().scale();
    if scale.fract() == 0. {
        return;
    }
    let device_pixel = 1. / scale;
    let mut pending = vec![document.root_node().id];
    while let Some(id) = pending.pop() {
        let Some(node) = document.get_node_mut(id) else {
            continue;
        };
        pending.extend(node.children.iter().copied());
        // Only an element has a box of its own.
        if node.element_data().is_none() {
            continue;
        }
        let unrounded = node.unrounded_layout().border;
        let border = &mut node.final_layout_mut().border;
        for (side, before) in [
            (&mut border.top, unrounded.top),
            (&mut border.right, unrounded.right),
            (&mut border.bottom, unrounded.bottom),
            (&mut border.left, unrounded.left),
        ] {
            if before > 0. && *side == 0. {
                *side = device_pixel;
            }
        }
    }
}
