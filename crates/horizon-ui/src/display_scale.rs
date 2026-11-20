//! What the display's scale asks of a frame that blitz-dom does not do:
//! styles computed again when it changes, and the layout rounded to the
//! display's pixels when it is fractional.
//!
//! Some computed values are in the display's pixels: a border's width is
//! made a whole number of them (at 150 % a `1px` border is two thirds of a
//! CSS pixel). blitz-dom builds the style system's device again when the
//! scale changes, but computes no element's style again, so the elements
//! styled before it keep the widths of the scale before: a page laid out
//! at a scale of 1 and then at 150 % keeps its `1px` borders a whole CSS
//! pixel, one and a half display pixels, wide. So when the scale has
//! changed since the styles were last computed, they all are again, as
//! blitz-dom has them for a change of color scheme.
//!
//! blitz-dom rounds the layout it computes to whole CSS pixels
//! (`taffy::round_layout`, inside its resolve). At a fractional scale a
//! whole CSS pixel is not a whole number of the display's pixels: at 150 %
//! a box lands on half a pixel, a `1px` border (which reaches layout as
//! two thirds of a CSS pixel, one display pixel) comes out either one CSS
//! pixel wide, drawn blurred across two display pixels, or not at all, and
//! the sides of one box differ.
//!
//! So after each resolve the layout is rounded again, from the same
//! unrounded layout, by taffy's own `round_layout` -- handed every length
//! in display pixels, which it rounds to whole numbers, and giving back
//! CSS pixels. Upstream's fix rounds to the same grid inside blitz-dom
//! (DioxusLabs/blitz#902, for #837); once a Blitz with it is in, this goes.

use std::cell::Cell;

use blitz_dom::{BaseDocument, Node, RestyleHint, dom_node_id, taffy_node_id};
use taffy::geometry::{Point, Rect, Size};
use taffy::{Layout, NodeId, RoundTree, ScrollableOverflow, TraversePartialTree, TraverseTree};

/// Before `document` is resolved: has its every element's style computed
/// again if its scale is not `styled_at`, the scale its styles were
/// computed at, which it then is.
pub(crate) fn restyle_on_a_new_scale(document: &mut BaseDocument, styled_at: &Cell<f32>) {
    let scale = document.viewport().scale();
    if styled_at.replace(scale) == scale {
        return;
    }
    let Some(root) = document.try_root_element().map(|root| root.id) else {
        return;
    };
    if let Some(root) = document.get_node_mut(root) {
        root.set_restyle_hint(RestyleHint::recascade_subtree());
    }
}

/// Rounds `document`'s layout, just resolved, to whole display pixels.
///
/// At a whole-number scale a whole CSS pixel is already a whole number of
/// display pixels, and the layout blitz-dom rounded is left as it is.
pub(crate) fn round_to_device_pixels(document: &mut BaseDocument) {
    let scale = document.viewport().scale();
    if scale.fract() == 0. {
        return;
    }
    let root = taffy_node_id(document.root_element().id);
    taffy::round_layout(&mut DevicePixels { document, scale }, root);
}

/// The document's layout tree as taffy rounds it: the tree blitz-dom lays
/// out (its layout children, and the boxes taken out of the flow by
/// their containing block), with lengths in display pixels.
struct DevicePixels<'a> {
    document: &'a mut BaseDocument,
    scale: f32,
}

impl DevicePixels<'_> {
    fn node(&self, id: NodeId) -> &Node {
        (self.document.get_node(dom_node_id(id))).expect("a node of the layout tree")
    }

    fn layout_children(&self, id: NodeId) -> Vec<NodeId> {
        let children = self.node(id).layout_children.borrow();
        (children.iter().flatten().map(|child| taffy_node_id(*child))).collect()
    }
}

impl TraversePartialTree for DevicePixels<'_> {
    type ChildIter<'a>
        = std::vec::IntoIter<NodeId>
    where
        Self: 'a;

    fn child_ids(&self, id: NodeId) -> Self::ChildIter<'_> {
        self.layout_children(id).into_iter()
    }

    fn child_count(&self, id: NodeId) -> usize {
        self.node(id)
            .layout_children
            .borrow()
            .as_ref()
            .map_or(0, |children| children.len())
    }

    fn get_child_id(&self, id: NodeId, index: usize) -> NodeId {
        let children = self.node(id).layout_children.borrow();
        taffy_node_id(children.as_ref().expect("a node with children")[index])
    }
}

impl TraverseTree for DevicePixels<'_> {}

impl RoundTree for DevicePixels<'_> {
    fn get_unrounded_layout(&self, id: NodeId) -> Layout {
        scaled(*self.node(id).unrounded_layout(), self.scale)
    }

    fn set_final_layout(&mut self, id: NodeId, rounded: &Layout) {
        let scale = self.scale;
        let node =
            (self.document.get_node_mut(dom_node_id(id))).expect("a node of the layout tree");
        // What taffy rounds, back in CSS pixels; the rest as laid out,
        // as taffy itself leaves it.
        let mut layout = *node.unrounded_layout();
        let rounded = scaled(*rounded, 1. / scale);
        layout.location = rounded.location;
        layout.size = rounded.size;
        layout.scrollable_overflow_rect = rounded.scrollable_overflow_rect;
        layout.scrollbar_size = rounded.scrollbar_size;
        layout.border = rounded.border;
        layout.padding = rounded.padding;
        *node.final_layout_mut() = layout;
    }

    fn is_out_of_flow(&self, id: NodeId) -> bool {
        (self.node(id).out_of_flow).load(std::sync::atomic::Ordering::Relaxed)
    }

    fn hoisted_child_count(&self, id: NodeId) -> usize {
        self.node(id).hoisted_children.borrow().len()
    }

    fn get_hoisted_child_id(&self, id: NodeId, index: usize) -> NodeId {
        taffy_node_id(self.node(id).hoisted_children.borrow()[index])
    }
}

/// `layout` with the lengths taffy rounds multiplied by `by`.
fn scaled(layout: Layout, by: f32) -> Layout {
    let length = |length: f32| length * by;
    let rect = |rect: Rect<f32>| rect.map(length);
    let overflow = layout.scrollable_overflow_rect;
    Layout {
        location: Point {
            x: length(layout.location.x),
            y: length(layout.location.y),
        },
        size: Size {
            width: length(layout.size.width),
            height: length(layout.size.height),
        },
        scrollable_overflow_rect: ScrollableOverflow::new(
            rect(overflow.rect()),
            overflow.is_reversed(),
        ),
        scrollbar_size: Size {
            width: length(layout.scrollbar_size.width),
            height: length(layout.scrollbar_size.height),
        },
        border: rect(layout.border),
        padding: rect(layout.padding),
        ..layout
    }
}
