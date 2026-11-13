//! Building and changing elements.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::Rc;

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName, ns};
use blitz_traits::events::DomEvent;
use reactive_graph::effect::Effect;
use reactive_graph::graph::untrack;
use reactive_graph::owner::Owner;

use crate::class::Classes;
use crate::component::Component;
use crate::shared::{drop_node, name, shared, with_document, write};

/// An element being built: a node of the document already.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct El(pub NodeId);

pub fn el(tag: &str) -> El {
    let tag = QualName::new(None, ns!(html), LocalName::from(tag));
    El(with_document(|document| {
        document.mutate().create_element(tag, Vec::new())
    }))
}

/// Puts `child` at the end of `parent`.
///
/// An element that was on the page and was taken out keeps the styles it
/// had, and what is added inside it while it is out has none. Put back
/// (itself, or inside something new), it is not looked into for what has
/// no styles, and the layout that follows would read styles that are not
/// there. So when something is put on the page, every styled element in
/// it with an unstyled one inside is marked as having something to style.
pub(crate) fn append(document: &mut BaseDocument, parent: NodeId, child: NodeId) {
    document.mutate().append_children(parent, &[child]);
    let on_the_page = (document.get_node(parent)).is_some_and(|node| node.flags.is_in_document());
    if !on_the_page {
        return;
    }
    let mut pending = vec![child];
    while let Some(id) = pending.pop() {
        let Some(node) = document.get_node(id) else {
            continue;
        };
        let styled = node.primary_styles().is_some();
        let mut marked = false;
        for inside in &node.children {
            let Some(found) = document.get_node(*inside) else {
                continue;
            };
            if found.element_data().is_none() {
                continue;
            }
            pending.push(*inside);
            if styled && !marked && found.primary_styles().is_none() {
                node.set_dirty_descendants();
                node.mark_ancestors_dirty();
                marked = true;
            }
        }
    }
}

impl El {
    pub fn attr(self, attribute: &str, value: &str) -> Self {
        with_document(|document| {
            document
                .mutate()
                .set_attribute(self.0, name(attribute), value)
        });
        self
    }

    /// An attribute that follows the signals `value` reads.
    pub fn attr_dyn(self, attribute: &'static str, value: impl Fn() -> String + 'static) -> Self {
        Effect::new(move |_| {
            let value = value();
            with_document(|document| {
                document
                    .mutate()
                    .set_attribute(self.0, name(attribute), &value)
            });
        });
        self
    }

    /// Adds classes to the element's: a CSS module's (whose sheet comes
    /// into the document with it), or the page's own by name.
    pub fn class(self, classes: impl Classes) -> Self {
        with_document(|document| crate::sheet::change(document, self.0, &[], &classes));
        self
    }

    /// Classes that follow the signals `classes` reads, beside the ones
    /// the element has otherwise.
    pub fn class_dyn<C: Classes>(self, classes: impl Fn() -> C + 'static) -> Self {
        Effect::new(move |earlier: Option<Vec<String>>| {
            let classes = classes();
            let earlier = earlier.unwrap_or_default();
            with_document(|document| crate::sheet::change(document, self.0, &earlier, &classes))
        });
        self
    }

    /// One property of the element's own style, following the signals
    /// `value` reads.
    pub fn style_dyn(self, property: &'static str, value: impl Fn() -> String + 'static) -> Self {
        Effect::new(move |_| {
            let value = value();
            with_document(|document| {
                (document.mutate()).set_style_property(self.0, property, &value)
            });
        });
        self
    }

    /// Puts a component's element at the end of this one's.
    pub fn child(self, child: impl Component) -> Self {
        let child = child.view();
        with_document(|document| append(document, self.0, child.0));
        self
    }

    pub fn text(self, text: &str) -> Self {
        with_document(|document| {
            let mut mutator = document.mutate();
            let node = mutator.create_text_node(text);
            mutator.append_children(self.0, &[node]);
        });
        self
    }

    /// A text node that follows the signals `text` reads.
    pub fn text_dyn(self, text: impl Fn() -> String + 'static) -> Self {
        let node = with_document(|document| {
            let mut mutator = document.mutate();
            let node = mutator.create_text_node("");
            mutator.append_children(self.0, &[node]);
            node
        });
        Effect::new(move |_| {
            let text = text();
            with_document(|document| document.mutate().set_node_text(node, &text));
        });
        self
    }

    /// Scrolls a scrolling element to a place in its content.
    pub fn scroll_to(self, x: f64, y: f64) -> Self {
        write(move |document| document.scroll_to(self.0, x, y, blitz_dom::ScrollBehavior::Instant));
        self
    }

    /// Scrolls a scrolling element by an amount.
    pub fn scroll_by(self, x: f64, y: f64) -> Self {
        write(move |document| document.scroll_by(self.0, x, y, blitz_dom::ScrollBehavior::Instant));
        self
    }

    /// Replaces what a text field holds.
    pub fn set_value(self, text: &str) -> Self {
        let text = text.to_string();
        write(move |document| {
            (document.mutate()).set_attribute(self.0, name("value"), &text);
        });
        self
    }

    /// Takes the element out of where it is, whole, to be put somewhere
    /// else: what it holds (typed text, a scroll position) goes with it.
    pub fn detach(self) -> Self {
        write(move |document| {
            // (It may have been removed since: a pane closed.)
            if document.get_node(self.0).is_none() {
                return;
            }
            let mut mutator = document.mutate();
            if mutator.node_has_parent(self.0) {
                mutator.remove_node(self.0);
            }
            drop(mutator);
            // blitz-dom let go of the widgets of what was taken out: each
            // is made again, to go with its element.
            let shared = shared();
            let mut pending = vec![self.0];
            while let Some(id) = pending.pop() {
                let Some(node) = document.get_node(id) else {
                    continue;
                };
                pending.extend(node.children.iter().copied());
                let maker = shared.widgets.borrow().get(&id).cloned();
                if let Some(maker) = maker {
                    document.set_custom_widget(id, maker());
                }
            }
        });
        self
    }

    /// Takes the element out for good, with everything inside it.
    pub fn remove(self) {
        write(move |document| drop_node(&shared(), document, self.0));
    }

    /// Has the input method on while this element has the focus, with its
    /// candidates by `area`: for an element that takes text and is not a
    /// text field (a terminal). blitz-dom turns the input method on for its
    /// own text fields only. What is composed arrives as `composition`
    /// events.
    pub fn takes_text(self, area: impl Fn() -> Option<(f32, f32, f32, f32)> + 'static) -> Self {
        (shared().text_targets.borrow_mut()).insert(self.0, Box::new(area));
        self
    }

    /// Runs `handler` with what is pasted while this element has the
    /// focus. blitz-dom gives a paste to its own text fields only.
    pub fn on_paste(self, handler: impl Fn(&str) + 'static) -> Self {
        (shared().pastes.borrow_mut()).insert(self.0, Rc::new(handler));
        self
    }

    /// Hands the element's box to the widget `make` makes, which paints
    /// it itself: the place for content the document does not describe (a
    /// terminal's grid). The widget is made again when the element is put
    /// somewhere else, so what it keeps is what it can make again.
    pub fn widget<W: blitz_dom::Widget + 'static>(self, make: impl Fn() -> W + 'static) -> Self {
        let make: crate::shared::WidgetMaker = Rc::new(move || Box::new(make()));
        with_document(|document| document.set_custom_widget(self.0, make()));
        shared().widgets.borrow_mut().insert(self.0, make);
        self
    }

    /// Runs `handler` for the DOM event `event` (`click`, `keydown`, ...)
    /// that reaches this element.
    pub fn on(self, event: &'static str, handler: impl Fn(&DomEvent) + 'static) -> Self {
        let shared = shared();
        let mut handlers = shared.handlers.borrow_mut();
        (handlers.entry(self.0).or_default()).push((event, Rc::new(handler)));
        self
    }

    /// Runs `handler` for the DOM event `event` on its way in to an
    /// element inside this one, before that element's own handlers: an
    /// outer element's say over what its contents get to hear (a shell's
    /// chords over a terminal's keys). [`stop_propagation`] from it keeps
    /// the event from everything further in.
    ///
    /// blitz-dom runs handlers from the target outwards only; it hands the
    /// whole path over at once, and this layer walks it inwards first.
    pub fn on_capture(self, event: &'static str, handler: impl Fn(&DomEvent) + 'static) -> Self {
        let shared = shared();
        let mut captures = shared.captures.borrow_mut();
        (captures.entry(self.0).or_default()).push((event, Rc::new(handler)));
        self
    }

    /// A child that is there while `shown` says so, built by `view` each
    /// time it appears.
    pub fn when(self, shown: impl Fn() -> bool + 'static, view: impl Fn() -> El + 'static) -> Self {
        self.each(
            move || if shown() { vec![()] } else { Vec::new() },
            |_| (),
            move |_| view(),
        )
    }

    /// Moves keyboard focus here, so that keys reach this element's
    /// handlers, once the document is next laid out. Not at once:
    /// blitz-dom, focusing a node, asks where it is on the page, and
    /// follows boxes that are gone if the node was built or moved since
    /// the last layout.
    pub fn focus(self) -> Self {
        shared().focus.set(Some((self.0, false)));
        self
    }

    /// [`focus`](Self::focus) for the first thing inside this element that
    /// takes focus: a text field, or an element with a `tabindex`.
    pub fn focus_within(self) -> Self {
        shared().focus.set(Some((self.0, true)));
        self
    }

    /// Children that follow a list: one per item of `items`, told apart by
    /// `key`. An item whose key stays keeps its node; `view` runs once for
    /// each key that is new.
    pub fn each<T, K>(
        self,
        items: impl Fn() -> Vec<T> + 'static,
        key: impl Fn(&T) -> K + 'static,
        view: impl Fn(&T) -> El + 'static,
    ) -> Self
    where
        T: 'static,
        K: Eq + Hash + Clone + 'static,
    {
        // The list's place among its parent's children: its nodes go just
        // before this marker, whatever else the parent holds.
        let marker = with_document(|document| {
            let mut mutator = document.mutate();
            let marker = mutator.create_comment_node("each");
            mutator.append_children(self.0, &[marker]);
            marker
        });
        let outer = Owner::current().expect("an owner");
        Effect::new(move |previous: Option<Vec<(K, NodeId, Owner)>>| {
            let items = items();
            let previous = previous.unwrap_or_default();
            let kept_before: Vec<NodeId> = previous.iter().map(|(_, node, _)| *node).collect();
            let mut old: HashMap<K, (NodeId, Owner)> = (previous.into_iter())
                .map(|(key, node, owner)| (key, (node, owner)))
                .collect();
            let mut next = Vec::with_capacity(items.len());
            let mut created = Vec::new();
            for item in &items {
                let key = key(item);
                let (node, owner) = old.remove(&key).unwrap_or_else(|| {
                    // The row's own effects belong to the row, and what
                    // `view` reads is not what the list depends on.
                    let owner = outer.child();
                    let node = owner.with(|| untrack(|| view(item))).0;
                    created.push(node);
                    (node, owner)
                });
                next.push((key, node, owner));
            }
            let order: Vec<NodeId> = next.iter().map(|(_, node, _)| *node).collect();
            // The nodes that stay, in the order they were in and are in now.
            let stayed = |nodes: &[NodeId], other: &[NodeId]| -> Vec<NodeId> {
                let other: HashSet<NodeId> = other.iter().copied().collect();
                (nodes.iter().copied())
                    .filter(|node| other.contains(node))
                    .collect()
            };
            let moved = stayed(&kept_before, &order) != stayed(&order, &kept_before);
            let appended = order.ends_with(&created);
            with_document(|document| {
                for (node, owner) in old.into_values() {
                    owner.cleanup();
                    drop_node(&shared(), document, node);
                }
                let mut mutator = document.mutate();
                if moved || !appended {
                    // Put in order again, each before the marker.
                    mutator.insert_nodes_before(marker, &order);
                } else if !created.is_empty() {
                    mutator.insert_nodes_before(marker, &created);
                }
            });
            next
        });
        self
    }
}

/// Puts `text` on the clipboard.
pub fn copy(text: String) {
    write(move |document| {
        let _ = document.shell_provider.set_clipboard_text(text);
    });
}

/// Asks for the clipboard's text, which arrives as a paste.
pub fn request_paste() {
    write(|document| {
        let _ = document.shell_provider.request_clipboard_text();
    });
}
