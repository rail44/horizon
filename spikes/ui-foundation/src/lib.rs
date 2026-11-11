//! Spike: components that run once, over signals, writing a `blitz-dom`
//! document.
//!
//! A view function builds its elements once, as nodes of the document. What
//! changes afterwards is a closure over signals, run as an effect that
//! writes one thing — a text node's contents, an attribute, a list's
//! children — when a signal it read changes. Nothing is rendered again and
//! nothing is compared, apart from a keyed list's keys.
//!
//! `blitz-dom` owns everything below: CSS (Stylo), layout (Taffy), text
//! (Parley), hit testing, focus and event dispatch.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::Rc;

use any_spawner::Executor;
use blitz_dom::{
    BaseDocument, Document, DocumentConfig, EventDriver, EventHandler, LocalName, NodeId, QualName,
    UserAgentStylesheet, ns,
};
use blitz_traits::events::{DomEvent, EventState, UiEvent};
use blitz_traits::shell::{ColorScheme, Viewport};
use reactive_graph::effect::Effect;
use reactive_graph::graph::untrack;
use reactive_graph::owner::Owner;

pub mod agent;
pub mod board;
pub mod demo;
pub mod markdown;
pub mod term;

pub use blitz_dom;
pub use blitz_traits;
pub use reactive_graph;

type Handler = Rc<dyn Fn(&DomEvent)>;

/// What the builders and the effects reach: the document, and the handlers
/// by node.
struct Shared {
    document: Rc<RefCell<BaseDocument>>,
    handlers: RefCell<HashMap<NodeId, Vec<(&'static str, Handler)>>>,
    /// What handlers asked to do with the document itself, done once the
    /// event that ran them has been dispatched.
    later: RefCell<Vec<Deferred>>,
    /// What runs after every layout, with the boxes that layout made.
    laid_out: RefCell<Vec<Box<dyn FnMut(&mut BaseDocument)>>>,
    /// What runs when the shell polls the document.
    polled: RefCell<Vec<Box<dyn FnMut(&std::task::Waker)>>>,
}

type Deferred = Box<dyn FnOnce(&mut BaseDocument)>;

/// Runs `work` after every layout from now on, before the page is painted:
/// the place for what depends on the boxes a change produced (keeping a
/// scrolled box at its end as its content grows).
pub fn each_layout(work: impl FnMut(&mut BaseDocument) + 'static) {
    shared().laid_out.borrow_mut().push(Box::new(work));
}

/// What a shell calls after it has resolved the document itself, in place
/// of [`App::resolve`]: the [`each_layout`] work.
pub fn after_resolve(document: &mut BaseDocument) {
    let Some(shared) = CURRENT.with(|current| current.borrow().clone()) else {
        return;
    };
    for work in shared.laid_out.borrow_mut().iter_mut() {
        work(document);
    }
}

thread_local! {
    static PREVENTED: Cell<bool> = const { Cell::new(false) };
    static STOPPED: Cell<bool> = const { Cell::new(false) };
}

/// From a handler: the document does not do what it would have done with
/// the event itself (a text field does not take the key).
pub fn prevent_default() {
    PREVENTED.set(true);
}

/// From a handler: the event goes to no element further out.
pub fn stop_propagation() {
    STOPPED.set(true);
}

/// Does `work` on the document once the event being handled has been
/// dispatched: a handler runs while the document is busy dispatching, so
/// what it wants of the document beyond a signal's write (scrolling an
/// element, moving focus) waits until then.
pub fn later(work: impl FnOnce(&mut BaseDocument) + 'static) {
    shared().later.borrow_mut().push(Box::new(work));
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
}

fn shared() -> Rc<Shared> {
    CURRENT.with(|current| current.borrow().clone().expect("inside an App"))
}

fn with_document<R>(write: impl FnOnce(&mut BaseDocument) -> R) -> R {
    let shared = shared();
    let mut document = shared.document.borrow_mut();
    write(&mut document)
}

fn name(local: &str) -> QualName {
    QualName::new(None, ns!(), LocalName::from(local))
}

/// An element being built: a node of the document already.
#[derive(Clone, Copy)]
pub struct El(pub NodeId);

pub fn el(tag: &str) -> El {
    let tag = QualName::new(None, ns!(html), LocalName::from(tag));
    El(with_document(|document| {
        document.mutate().create_element(tag, Vec::new())
    }))
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

    pub fn class(self, class: &str) -> Self {
        self.attr("class", class)
    }

    pub fn class_dyn(self, class: impl Fn() -> String + 'static) -> Self {
        self.attr_dyn("class", class)
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

    pub fn child(self, child: El) -> Self {
        with_document(|document| document.mutate().append_children(self.0, &[child.0]));
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

    /// Hands the element's box to `widget`, which paints it itself: the
    /// place for content the document does not describe (a terminal's grid).
    pub fn widget(self, widget: impl blitz_dom::Widget + 'static) -> Self {
        with_document(|document| document.set_custom_widget(self.0, Box::new(widget)));
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
    /// handlers.
    pub fn focus(self) -> Self {
        later(move |document| {
            document.set_focus_to(self.0);
        });
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
                let mut mutator = document.mutate();
                for (node, owner) in old.into_values() {
                    owner.cleanup();
                    mutator.remove_and_drop_node(node);
                }
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

/// A document, the view built into it, and the signals' effects.
pub struct App {
    shared: Rc<Shared>,
    _owner: Owner,
}

impl App {
    /// Builds `view` once into a new document of `width` by `height`,
    /// styled by `css`.
    pub fn new(width: u32, height: u32, css: &str, view: impl FnOnce() -> El) -> Self {
        // One executor a thread; a second `App` on it finds it set.
        let _ = Executor::init_futures_executor();
        let owner = Owner::new();
        owner.set();
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(width, height, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        });
        document.add_user_agent_stylesheet(UserAgentStylesheet::parse(css));
        let shared = Rc::new(Shared {
            document: Rc::new(RefCell::new(document)),
            handlers: RefCell::new(HashMap::new()),
            later: RefCell::new(Vec::new()),
            laid_out: RefCell::new(Vec::new()),
            polled: RefCell::new(Vec::new()),
        });
        CURRENT.with(|current| *current.borrow_mut() = Some(shared.clone()));
        let body = el("body");
        let html = el("html").child(body);
        with_document(|document| {
            let root = document.root_node().id;
            document.mutate().append_children(root, &[html.0]);
        });
        let view = view();
        body.child(view);
        let app = Self {
            shared,
            _owner: owner,
        };
        app.run_effects();
        app
    }

    /// The document, for whatever draws it or feeds it events.
    pub fn document(&self) -> Rc<RefCell<BaseDocument>> {
        self.shared.document.clone()
    }

    /// The document where it lives, for a borrow that outlasts the call.
    pub fn document_cell(&self) -> &RefCell<BaseDocument> {
        &self.shared.document
    }

    /// Runs the effects of the signals written since the last call: the
    /// writes to the document.
    pub fn run_effects(&self) {
        CURRENT.with(|current| *current.borrow_mut() = Some(self.shared.clone()));
        Executor::poll_local();
        // What handlers and effects left for the document, and the effects
        // of that in turn.
        loop {
            let later = std::mem::take(&mut *self.shared.later.borrow_mut());
            if later.is_empty() {
                break;
            }
            for work in later {
                work(&mut self.shared.document.borrow_mut());
            }
            Executor::poll_local();
        }
    }

    /// Restyles and lays out what the writes since the last call touched.
    pub fn resolve(&self) {
        self.resolve_at(0.0);
    }

    /// [`resolve`](Self::resolve) with the clock CSS animations run on at
    /// `seconds`.
    pub fn resolve_at(&self, seconds: f64) {
        let mut document = self.shared.document.borrow_mut();
        document.resolve(seconds);
        restore_thin_borders(&mut document);
        for work in self.shared.laid_out.borrow_mut().iter_mut() {
            work(&mut document);
        }
    }

    /// Runs `work` whenever the shell polls the document, with the waker
    /// that makes it poll again: how something outside the document (a
    /// session's updates, a timer) gets its changes in and drawn.
    pub fn on_poll(&self, work: impl FnMut(&std::task::Waker) + 'static) {
        self.shared.polled.borrow_mut().push(Box::new(work));
    }

    /// Hands the document an input event, runs the handlers it reaches and
    /// then the effects of what they wrote.
    pub fn dispatch(&self, event: UiEvent) {
        let pressed = match &event {
            UiEvent::PointerDown(pointer) | UiEvent::PointerUp(pointer) => {
                Some((pointer.page_x(), pointer.page_y()))
            }
            _ => None,
        };
        let mut document = self.shared.document.clone();
        let handler = Handlers(self.shared.clone());
        EventDriver::new(&mut document as &mut dyn Document, handler).handle_ui_event(event);
        if let Some((x, y)) = pressed {
            self.focus_from(x, y);
        }
        self.run_effects();
        // blitz-dom tells the shell where an input is when it takes focus,
        // and not again: the input method's candidates would stay there
        // while the caret moves on.
        if let Some((x, y, width, height)) = self.ime_cursor_area() {
            let shell = self.shared.document.borrow().shell_provider.clone();
            shell.set_ime_cursor_area(x, y, width, height);
        }
    }

    /// A press on something that cannot take focus leaves the page itself
    /// focused, and keys then go to the page and to no view. A browser
    /// gives the focus to the nearest thing around the press that can take
    /// it; this does that.
    fn focus_from(&self, x: f32, y: f32) {
        let mut document = self.shared.document.borrow_mut();
        let on_a_view = document.get_focussed_node_id().is_some_and(|id| {
            let tag = document.get_node(id).and_then(|node| node.element_data());
            tag.is_some_and(|element| !matches!(&*element.name.local, "html" | "body"))
        });
        if on_a_view {
            return;
        }
        let mut node = document.hit(x, y).map(|hit| hit.node_id);
        while let Some(id) = node {
            let Some(found) = document.get_node(id) else {
                return;
            };
            if found.attr(blitz_dom::local_name!("tabindex")).is_some() {
                document.set_focus_to(id);
                return;
            }
            node = found.parent;
        }
    }

    /// Where the text being edited is, in the page's CSS pixels: the text
    /// being composed if there is some, the caret otherwise. `None` unless
    /// a text input has focus.
    pub fn ime_cursor_area(&self) -> Option<(f32, f32, f32, f32)> {
        let document = self.shared.document.borrow();
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
}

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
fn restore_thin_borders(document: &mut BaseDocument) {
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

struct Handlers(Rc<Shared>);

impl EventHandler for Handlers {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        _document: &mut dyn Document,
        state: &mut EventState,
    ) {
        for node in chain {
            // Cloned out: a handler may add handlers (a row made in it).
            let handlers: Vec<Handler> = (self.0.handlers.borrow().get(node).into_iter().flatten())
                .filter(|(name, _)| *name == event.name())
                .map(|(_, handler)| handler.clone())
                .collect();
            for handler in handlers {
                handler(event);
            }
            if PREVENTED.replace(false) {
                state.prevent_default();
            }
            if STOPPED.replace(false) || !event.bubbles {
                break;
            }
        }
    }
}

/// One step of the mouse at a point of the page: what a test drives the
/// document's hit testing and dispatch with.
#[derive(Clone, Copy, PartialEq)]
pub enum Pointer {
    /// A move with no button held.
    Move,
    /// The main button goes down.
    Down,
    /// A move with the main button held.
    Drag,
    /// The main button comes up.
    Up,
}

pub fn pointer(app: &App, x: f32, y: f32, step: Pointer) {
    use blitz_traits::events::{
        BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, PointerCoords,
    };
    let event = |buttons| BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: x,
            page_y: y,
            screen_x: x,
            screen_y: y,
            client_x: x,
            client_y: y,
        },
        button: MouseEventButton::Main,
        buttons,
        mods: Default::default(),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    };
    app.dispatch(match step {
        Pointer::Move => UiEvent::PointerMove(event(MouseEventButtons::None)),
        Pointer::Down => UiEvent::PointerDown(event(MouseEventButtons::Primary)),
        Pointer::Drag => UiEvent::PointerMove(event(MouseEventButtons::Primary)),
        Pointer::Up => UiEvent::PointerUp(event(MouseEventButtons::None)),
    });
}

/// A press and a release of the main button at a point of the page.
pub fn click_at(app: &App, x: f32, y: f32) {
    for step in [Pointer::Move, Pointer::Down, Pointer::Up] {
        pointer(app, x, y, step);
    }
}

/// The pointer moved to a point of the page: what the wheel then scrolls
/// is what is under it.
pub fn move_to(app: &App, x: f32, y: f32) {
    pointer(app, x, y, Pointer::Move);
}

/// An [`App`] as the document a shell shows: input goes through the
/// layer's handlers, and so through the signals' effects.
pub struct Live(pub App);

impl Document for Live {
    fn inner(&self) -> blitz_dom::DocGuard<'_> {
        blitz_dom::DocGuard::RefCell(self.0.document_cell().borrow())
    }

    fn inner_mut(&mut self) -> blitz_dom::DocGuardMut<'_> {
        blitz_dom::DocGuardMut::RefCell(self.0.document_cell().borrow_mut())
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        self.0.dispatch(event);
    }

    fn poll(&mut self, task_context: Option<std::task::Context>) -> bool {
        let Some(context) = task_context else {
            return false;
        };
        let polled = !self.0.shared.polled.borrow().is_empty();
        CURRENT.with(|current| *current.borrow_mut() = Some(self.0.shared.clone()));
        for work in self.0.shared.polled.borrow_mut().iter_mut() {
            work(context.waker());
        }
        self.0.run_effects();
        polled
    }
}
