//! A long list in a scrolling box, with only the items near what is in
//! view built as elements.
//!
//! The box holds a blank above, the items of a range, and a blank below.
//! After every layout the heights of the built items are read and kept;
//! the blanks are as tall as the items they stand for, by their kept
//! heights or, for one never built, the average of those that were. The
//! range is then chosen again from where the box is scrolled to, and if it
//! changed, the list is built for it and laid out again before the paint.
//!
//! An item built for the first time is rarely as tall as was guessed. So
//! that what is in view does not jump when that happens above it, one item
//! in view is kept where it was on the screen: the box is scrolled by
//! however far that item moved.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;

use blitz_dom::{BaseDocument, NodeId, ScrollBehavior};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, GetUntracked, Set};

use crate::{El, each_layout, el, relayout};

/// How far past the box's edges items are kept built, so a scroll does not
/// wait on them.
const OVERDRAW: f64 = 600.;
/// The height guessed for an item before any has been built.
const FIRST_GUESS: f64 = 120.;
/// How many items the list is first built with.
const FIRST_RANGE: usize = 8;

struct State<K> {
    /// The keys of every item, in order, as of the last build.
    order: Vec<K>,
    /// The items built: these positions of `order`.
    range: (usize, usize),
    /// The elements of the built items.
    nodes: HashMap<K, NodeId>,
    /// The heights read off built items.
    heights: HashMap<K, f64>,
    /// The height taken for an item never built, and the range it was
    /// worked out at: it moves only when the range does, so that an item
    /// in view changing its height does not move the blanks.
    guess: (f64, (usize, usize)),
    /// The blanks' heights as last written.
    blanks: (f64, f64),
    /// Whether the box is kept at its end as the list grows.
    following: bool,
    /// Where the box was scrolled to, and how far it could go, when the
    /// last layout was done with.
    seen: (f64, f64),
    /// An item in view and where its top was, from the box's top edge.
    anchor: Option<(K, f64)>,
}

impl<K: Eq + Hash + Clone> State<K> {
    /// The average of the heights read so far, as of the last change of
    /// range.
    fn guess(&mut self) -> f64 {
        if self.guess.1 != self.range && !self.heights.is_empty() {
            let average = self.heights.values().sum::<f64>() / self.heights.len() as f64;
            self.guess = (average, self.range);
        }
        self.guess.0
    }

    fn height(&self, position: usize, guess: f64) -> f64 {
        (self.heights.get(&self.order[position]).copied()).unwrap_or(guess)
    }
}

/// Fills `scroller` with the list `items`, told apart by `key` and built by
/// `view`. With `follow_tail`, the box starts at its end and stays there
/// as the list grows, until a scroll away from the end; a scroll back down
/// to the end takes it up again.
///
/// Returns the range of items built, for whoever wants to know.
pub fn windowed<T, K>(
    scroller: El,
    items: impl Fn() -> Vec<T> + 'static,
    key: impl Fn(&T) -> K + Clone + 'static,
    view: impl Fn(&T) -> El + 'static,
    follow_tail: bool,
) -> RwSignal<(usize, usize)>
where
    T: Clone + 'static,
    K: Eq + Hash + Clone + 'static,
{
    let state = Rc::new(RefCell::new(State::<K> {
        order: Vec::new(),
        range: (0, 0),
        nodes: HashMap::new(),
        heights: HashMap::new(),
        guess: (FIRST_GUESS, (0, 0)),
        blanks: (0., 0.),
        following: follow_tail,
        seen: (0., 0.),
        anchor: None,
    }));
    // Written when the range changes: the list is built again.
    let built = RwSignal::new((0usize, 0usize));

    let above = el("div");
    let below = el("div");
    scroller.child(above);
    let (shared, key_of, key_built) = (state.clone(), key.clone(), key.clone());
    let recorded = state.clone();
    scroller.each(
        move || {
            built.get();
            let all = items();
            let mut state = shared.borrow_mut();
            let count = all.len();
            let grown = count > state.order.len();
            let first = state.order.is_empty();
            state.order = all.iter().map(&key_of).collect();
            let (mut start, mut end) = state.range;
            if first {
                (start, end) = match state.following {
                    true => (count.saturating_sub(FIRST_RANGE), count),
                    false => (0, count.min(FIRST_RANGE)),
                };
            }
            // What is added at the end of a list being followed is in
            // view at once, not a layout later.
            if state.following && grown {
                end = count;
            }
            (start, end) = (start.min(count), end.min(count));
            state.range = (start, end);
            // Only the built items' elements are known.
            let kept: Vec<K> = state.order[start..end].to_vec();
            state.nodes.retain(|key, _| kept.contains(key));
            all[start..end].to_vec()
        },
        key,
        move |item| {
            let element = view(item);
            (recorded.borrow_mut().nodes).insert(key_built(item), element.0);
            element
        },
    );
    scroller.child(below);

    each_layout(move |document| {
        let mut state = state.borrow_mut();
        let state = &mut *state;
        let count = state.order.len();
        let (start, end) = state.range;
        let Some(node) = document.get_node(scroller.0) else {
            return;
        };
        let view_height = node.final_layout().size.height as f64;
        let mut at = node.scroll_offset().y;
        let top_of = |document: &BaseDocument, node: NodeId| {
            (document.get_node(node)).map(|node| node.final_layout().location.y as f64)
        };

        // The built items' heights, as this layout made them.
        for key in &state.order[start..end] {
            let height = (state.nodes.get(key))
                .and_then(|node| document.get_node(*node))
                .map(|node| node.final_layout().size.height as f64);
            if let Some(height) = height {
                state.heights.insert(key.clone(), height);
            }
        }
        if state.heights.len() > 2 * count + 64 {
            let order = &state.order;
            state.heights.retain(|key, _| order.contains(key));
        }

        // The blanks, as tall as what they stand for.
        let guess = state.guess();
        let sum = |from: usize, to: usize| -> f64 {
            (from..to)
                .map(|position| state.height(position, guess))
                .sum()
        };
        let blanks = (sum(0, start), sum(end, count));
        if blanks != state.blanks {
            state.blanks = blanks;
            let mut mutator = document.mutate();
            mutator.set_style_property(above.0, "height", &format!("{}px", blanks.0));
            mutator.set_style_property(below.0, "height", &format!("{}px", blanks.1));
            relayout();
            return;
        }

        let scrolled_by_hand = (at - state.seen.0).abs() > 0.5;
        if scrolled_by_hand && follow_tail {
            state.following = at >= state.seen.1 - 1.;
        }
        let end_of_scroll = node.final_layout().scroll_height() as f64;
        if state.following {
            if (end_of_scroll - at).abs() > 0.5 {
                document.scroll_to(scroller.0, 0., end_of_scroll, ScrollBehavior::Instant);
            }
        } else if !scrolled_by_hand {
            // What was in view stays where it was, whatever was built
            // above it since.
            let moved = state.anchor.as_ref().and_then(|(key, was)| {
                let top = top_of(document, *state.nodes.get(key)?)?;
                Some(top - at - was)
            });
            if let Some(moved) = moved.filter(|moved| moved.abs() > 0.5) {
                document.scroll_by(scroller.0, 0., moved, ScrollBehavior::Instant);
            }
        }
        at = (document.get_node(scroller.0)).map_or(at, |node| node.scroll_offset().y);
        state.seen = (at, end_of_scroll);
        state.anchor = (state.order[start..end].iter()).find_map(|key| {
            let node = document.get_node(*state.nodes.get(key)?)?;
            let layout = node.final_layout();
            let top = layout.location.y as f64;
            (top + layout.size.height as f64 > at).then(|| (key.clone(), top - at))
        });

        // The range for where the box is now: from the first item that
        // ends below the upper bound to the last that starts above the
        // lower one.
        let origin = top_of(document, above.0).unwrap_or(0.);
        let (upper, lower) = (at - OVERDRAW - origin, at + view_height + OVERDRAW - origin);
        let (mut top, mut wanted) = (0., (count, count));
        for position in 0..count {
            let bottom = top + state.height(position, guess);
            if wanted.0 == count && bottom > upper {
                wanted.0 = position;
            }
            if top < lower {
                wanted.1 = position + 1;
            }
            top = bottom;
        }
        if state.following {
            wanted.1 = count;
        }
        wanted.0 = wanted.0.min(wanted.1);
        if wanted != (start, end) {
            state.range = wanted;
            built.set(wanted);
            relayout();
        } else if built.get_untracked() != wanted {
            built.set(wanted);
        }
    });
    built
}
