//! Reading the layout: a view asks a [`Page`], at one of the two times its
//! answers are sound.

use blitz_dom::BaseDocument;
use reactive_graph::owner::{Owner, on_cleanup};

use crate::el::El;
use crate::shared::{current, fresh_id, shared};

/// A box on the page, in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The page as it was last laid out, to ask things of. A view never has
/// the document itself: it builds and changes elements through [`El`], and
/// what it needs to know of the layout it asks here, at one of the two
/// times the answer is sound ([`dispatched`], [`each_layout`]).
pub struct Page<'a>(pub(crate) &'a BaseDocument);

impl Page<'_> {
    /// Where an element's box is on the page.
    pub fn bounds(&self, element: El) -> Option<Bounds> {
        let bounds = self.0.get_client_bounding_rect(element.0)?;
        Some(Bounds {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        })
    }

    /// Where an element's box is painted from, in CSS pixels: its place as
    /// layout rounded it, which is what the painting goes by.
    /// [`bounds`](Self::bounds) is its place before the rounding, and at a
    /// display scale that is not whole the two differ by part of a device
    /// pixel.
    pub fn painted_at(&self, element: El) -> Option<(f64, f64)> {
        let at = self.0.get_node(element.0)?.absolute_position(0., 0.);
        let scroll = self.0.viewport_scroll();
        Some((at.x as f64 - scroll.x, at.y as f64 - scroll.y))
    }

    /// An element's width and height.
    pub fn size(&self, element: El) -> Option<(f64, f64)> {
        let size = self.0.get_node(element.0)?.final_layout().size;
        Some((size.width as f64, size.height as f64))
    }

    /// The element drawn at a point of the page and the elements around
    /// it, innermost first.
    pub fn under(&self, x: f32, y: f32) -> Vec<El> {
        let mut path = Vec::new();
        let mut node = self.0.hit(x, y).map(|hit| hit.node_id);
        while let Some(id) = node {
            path.push(El(id));
            node = self.0.get_node(id).and_then(|node| node.parent);
        }
        path
    }

    /// How far a scrolling element is scrolled down, and how far it can be.
    pub fn scrolled(&self, element: El) -> Option<(f64, f64)> {
        let node = self.0.get_node(element.0)?;
        Some((
            node.scroll_offset().y,
            node.final_layout().scroll_height() as f64,
        ))
    }

    /// How many lines a text field's text takes, wrapped ones counted.
    pub fn lines(&self, field: El) -> Option<usize> {
        let node = self.0.get_node(field.0)?;
        let input = node.element_data()?.text_input_data()?;
        input.editor.try_layout().map(|layout| layout.len())
    }

    /// Whether the keys go to this element.
    pub fn has_focus(&self, element: El) -> bool {
        self.0.get_focussed_node_id() == Some(element.0)
    }

    /// The text selected on the page outside any field: for each element
    /// whose own lines of text the selection covers part of, the bytes of
    /// that text ([`inline_text`](Self::inline_text)) it covers.
    pub fn selection(&self) -> Vec<(El, usize, usize)> {
        (self.0.get_text_selection_ranges().into_iter())
            .map(|(node, start, end)| (El(node), start, end))
            .collect()
    }

    /// The text an element lays out in lines of its own: its text nodes'
    /// and its inline descendants', in order. `None` for an element that
    /// lays out blocks, or has not been laid out.
    pub fn inline_text(&self, element: El) -> Option<String> {
        let node = self.0.get_node(element.0)?;
        let layout = node.element_data()?.inline_layout_data.as_ref()?;
        Some(layout.text.clone())
    }

    /// Whether the keys go to this element or to something inside it.
    pub fn has_focus_within(&self, element: El) -> bool {
        let mut node = self.0.get_focussed_node_id();
        while let Some(id) = node {
            if id == element.0 {
                return true;
            }
            node = self.0.get_node(id).and_then(|node| node.parent);
        }
        false
    }
}

/// Runs `work` after every layout from now on, before the page is painted:
/// the place for what depends on the boxes a change produced. It runs for
/// as long as what it was made under does: a view's work ends with the
/// view.
pub fn each_layout(mut work: impl FnMut(&Page) + 'static) {
    each_layout_raw(move |document| work(&Page(document)));
}

/// [`each_layout`] with the document itself, for the layer's own parts.
pub(crate) fn each_layout_raw(work: impl FnMut(&mut BaseDocument) + 'static) {
    let id = fresh_id();
    shared().laid_out.borrow_mut().push((id, Box::new(work)));
    if Owner::current().is_some() {
        on_cleanup(move || {
            if let Some(shared) = current() {
                shared.retired.borrow_mut().insert(id);
            }
        });
    }
}

/// Asks `work`'s questions of the page as soon as the event being handled
/// has been dispatched, before any effect runs: the place for reading the
/// layout from a handler (what is under a point, where a box is). Once
/// effects have changed the document, its boxes are those of nodes that
/// may be gone until the next layout.
pub fn dispatched(work: impl FnOnce(&Page) + 'static) {
    (shared().dispatched.borrow_mut()).push(Box::new(move |document| work(&Page(document))));
}
