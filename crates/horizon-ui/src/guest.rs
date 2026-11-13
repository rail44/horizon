//! The layer as a view built into a wasm plugin has it: the same `El`
//! and `Page`, over the host's document instead of one of its own.
//!
//! A plugin has no document. Building and changing an element is a call
//! to the host, which does it in its own document, in the place it gave
//! the plugin; reading the layout is a call too, answered while the host
//! runs the plugin's work after a layout or after an event. Signals,
//! effects and handlers are the plugin's own: the host tells it an event
//! reached a node it listens at, and when to run its effects.
//!
//! A view's code is the same on both sides (`plugin.wit` is the whole
//! vocabulary). What a view cannot have here is what does not cross: a
//! paint widget, the input method for an element that is not a field,
//! pastes and the clipboard, the page's selection.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::marker::PhantomData;
use std::rc::Rc;
use std::str::FromStr;

use any_spawner::Executor;
use blitz_traits::events::{
    BlitzInputEvent, BlitzKeyEvent, BlitzPointerEvent, BlitzPointerId, DomEvent, DomEventData,
    KeyState, MouseEventButton, MouseEventButtons, PointerCoords,
};
use blitz_traits::node_id::NodeId;
use keyboard_types::{Code, Key, Location, Modifiers, NamedKey};
use reactive_graph::effect::Effect;
use reactive_graph::graph::untrack;
use reactive_graph::owner::{Owner, on_cleanup};

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "export_plugin",
    });
}

use bindings::horizon::ui::document as host;
use bindings::{EventData, Handled};

type Handler = Rc<dyn Fn(&DomEvent)>;
/// The handlers at a node, by whether they hear an event on its way in.
type Handlers = HashMap<(NodeId, bool), Vec<(&'static str, Handler)>>;
/// Work done after every layout, under the id it is taken back out by.
type LayoutWork = (u64, Box<dyn FnMut(&Page)>);
type DispatchedWork = Box<dyn FnOnce(&Page)>;

/// What the layer keeps on the plugin's side.
#[derive(Default)]
struct Kept {
    handlers: RefCell<Handlers>,
    laid_out: RefCell<Vec<LayoutWork>>,
    retired: RefCell<HashSet<u64>>,
    dispatched: RefCell<Vec<DispatchedWork>>,
    next_id: Cell<u64>,
    /// What the mounted view was made under.
    owner: RefCell<Option<Owner>>,
}

thread_local! {
    static KEPT: Kept = Kept::default();
    static PREVENTED: Cell<bool> = const { Cell::new(false) };
    static STOPPED: Cell<bool> = const { Cell::new(false) };
}

/// From a handler: the document does not do what it would have done with
/// the event itself.
pub fn prevent_default() {
    PREVENTED.set(true);
}

/// From a handler: the event goes to no element further out.
pub fn stop_propagation() {
    STOPPED.set(true);
}

/// An element being built: a node of the host's document already.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct El(pub NodeId);

fn id(element: El) -> u64 {
    element.0.as_u64()
}

fn node(number: u64) -> NodeId {
    NodeId::from_u64(number)
}

pub fn el(tag: &str) -> El {
    El(node(host::create_element(tag)))
}

/// Takes a node out for good, and with it the handlers kept for what
/// went.
fn drop_node(dropped: NodeId) {
    let gone = host::remove(dropped.as_u64());
    KEPT.with(|kept| {
        let mut handlers = kept.handlers.borrow_mut();
        for gone in gone {
            handlers.remove(&(node(gone), false));
            handlers.remove(&(node(gone), true));
        }
    });
}

impl El {
    pub fn attr(self, attribute: &str, value: &str) -> Self {
        host::set_attribute(id(self), attribute, value);
        self
    }

    /// An attribute that follows the signals `value` reads.
    pub fn attr_dyn(self, attribute: &'static str, value: impl Fn() -> String + 'static) -> Self {
        Effect::new(move |_| host::set_attribute(id(self), attribute, &value()));
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
        Effect::new(move |_| host::set_style(id(self), property, &value()));
        self
    }

    /// Gives the element a style sheet of its own: plain CSS whose rules
    /// reach the element and what is under it, and stop at what is inside
    /// a view placed there.
    pub fn styled(self, css: &str) -> Self {
        host::adopt_style(id(self), css);
        self
    }

    pub fn child(self, child: El) -> Self {
        host::append(id(self), id(child));
        self
    }

    pub fn text(self, text: &str) -> Self {
        host::append(id(self), host::create_text(text));
        self
    }

    /// A text node that follows the signals `text` reads.
    pub fn text_dyn(self, text: impl Fn() -> String + 'static) -> Self {
        let text_node = host::create_text("");
        host::append(id(self), text_node);
        Effect::new(move |_| host::set_text(text_node, &text()));
        self
    }

    /// Scrolls a scrolling element to a place in its content.
    pub fn scroll_to(self, x: f64, y: f64) -> Self {
        host::scroll_to(id(self), x, y);
        self
    }

    /// Scrolls a scrolling element by an amount.
    pub fn scroll_by(self, x: f64, y: f64) -> Self {
        host::scroll_by(id(self), x, y);
        self
    }

    /// Replaces what a text field holds.
    pub fn set_value(self, text: &str) -> Self {
        host::set_value(id(self), text);
        self
    }

    /// Takes the element out of where it is, whole, to be put somewhere
    /// else.
    pub fn detach(self) -> Self {
        host::detach(id(self));
        self
    }

    /// Takes the element out for good, with everything inside it.
    pub fn remove(self) {
        drop_node(self.0);
    }

    fn listen(self, event: &'static str, capture: bool, handler: Handler) -> Self {
        KEPT.with(|kept| {
            let mut handlers = kept.handlers.borrow_mut();
            let at = handlers.entry((self.0, capture)).or_default();
            // The host is told once for each event at a node.
            if !at.iter().any(|(known, _)| *known == event) {
                host::listen(id(self), event, capture);
            }
            at.push((event, handler));
        });
        self
    }

    /// Runs `handler` for the DOM event `event` that reaches this element.
    pub fn on(self, event: &'static str, handler: impl Fn(&DomEvent) + 'static) -> Self {
        self.listen(event, false, Rc::new(handler))
    }

    /// Runs `handler` for the DOM event `event` on its way in to an
    /// element inside this one, before that element's own handlers.
    pub fn on_capture(self, event: &'static str, handler: impl Fn(&DomEvent) + 'static) -> Self {
        self.listen(event, true, Rc::new(handler))
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

    /// Moves keyboard focus here once the document is next laid out.
    pub fn focus(self) -> Self {
        host::focus(id(self), false);
        self
    }

    /// [`focus`](Self::focus) for the first thing inside this element that
    /// takes focus.
    pub fn focus_within(self) -> Self {
        host::focus(id(self), true);
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
        let marker = host::create_comment("each");
        host::append(id(self), marker);
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
                    let owner = outer.child();
                    let node = owner.with(|| untrack(|| view(item))).0;
                    created.push(node);
                    (node, owner)
                });
                next.push((key, node, owner));
            }
            let order: Vec<NodeId> = next.iter().map(|(_, node, _)| *node).collect();
            let stayed = |nodes: &[NodeId], other: &[NodeId]| -> Vec<NodeId> {
                let other: HashSet<NodeId> = other.iter().copied().collect();
                (nodes.iter().copied())
                    .filter(|node| other.contains(node))
                    .collect()
            };
            let moved = stayed(&kept_before, &order) != stayed(&order, &kept_before);
            let appended = order.ends_with(&created);
            for (node, owner) in old.into_values() {
                owner.cleanup();
                drop_node(node);
            }
            let numbers =
                |nodes: &[NodeId]| -> Vec<u64> { nodes.iter().map(|node| node.as_u64()).collect() };
            if moved || !appended {
                host::insert_before(marker, &numbers(&order));
            } else if !created.is_empty() {
                host::insert_before(marker, &numbers(&created));
            }
            next
        });
        self
    }
}

/// A box on the page, in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The page as it was last laid out, to ask things of: the host's, while
/// it runs this plugin's work after a layout or after an event.
pub struct Page<'a>(PhantomData<&'a ()>);

impl Page<'_> {
    /// Where an element's box is on the page.
    pub fn bounds(&self, element: El) -> Option<Bounds> {
        host::bounds_of(id(element)).map(|bounds| Bounds {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        })
    }

    /// An element's width and height.
    pub fn size(&self, element: El) -> Option<(f64, f64)> {
        host::size_of(id(element))
    }

    /// The plugin's elements drawn at a point of the page, innermost
    /// first.
    pub fn under(&self, x: f32, y: f32) -> Vec<El> {
        (host::under(x, y).into_iter())
            .map(|under| El(node(under)))
            .collect()
    }

    /// How far a scrolling element is scrolled down, and how far it can be.
    pub fn scrolled(&self, element: El) -> Option<(f64, f64)> {
        host::scrolled(id(element))
    }

    /// How many lines a text field's text takes, wrapped ones counted.
    pub fn lines(&self, field: El) -> Option<usize> {
        host::lines(id(field)).map(|lines| lines as usize)
    }

    /// Whether the keys go to this element.
    pub fn has_focus(&self, element: El) -> bool {
        host::has_focus(id(element))
    }

    /// Whether the keys go to this element or to something inside it.
    pub fn has_focus_within(&self, element: El) -> bool {
        host::has_focus_within(id(element))
    }
}

/// Runs `work` after every layout from now on, for as long as what it was
/// made under does.
pub fn each_layout(work: impl FnMut(&Page) + 'static) {
    KEPT.with(|kept| {
        let id = kept.next_id.get();
        kept.next_id.set(id + 1);
        kept.laid_out.borrow_mut().push((id, Box::new(work)));
        if Owner::current().is_some() {
            on_cleanup(move || {
                KEPT.with(|kept| {
                    kept.retired.borrow_mut().insert(id);
                });
            });
        }
    });
}

/// Asks `work`'s questions of the page as soon as the event being handled
/// has been dispatched, before any effect runs.
pub fn dispatched(work: impl FnOnce(&Page) + 'static) {
    KEPT.with(|kept| kept.dispatched.borrow_mut().push(Box::new(work)));
}

/// From [`each_layout`] work: the document is laid out again before it is
/// painted.
pub fn relayout() {
    host::relayout();
}

// -- what the host calls ----------------------------------------------------

fn key_event(event: &bindings::KeyEvent) -> BlitzKeyEvent {
    let key = match (&event.named, &event.character) {
        (Some(named), _) => {
            NamedKey::from_str(named).map_or(Key::Named(NamedKey::Unidentified), Key::Named)
        }
        (None, Some(character)) => Key::Character(character.clone()),
        (None, None) => Key::Named(NamedKey::Unidentified),
    };
    BlitzKeyEvent {
        key,
        code: Code::Unidentified,
        modifiers: Modifiers::from_bits_truncate(event.modifiers),
        location: Location::Standard,
        is_auto_repeating: event.repeat,
        is_composing: event.composing,
        state: KeyState::Pressed,
        text: event.text.as_deref().map(Into::into),
    }
}

fn pointer_event(event: &bindings::PointerEvent) -> BlitzPointerEvent {
    BlitzPointerEvent {
        id: BlitzPointerId::Mouse,
        is_primary: true,
        coords: PointerCoords {
            page_x: event.page_x,
            page_y: event.page_y,
            screen_x: event.page_x,
            screen_y: event.page_y,
            client_x: event.client_x,
            client_y: event.client_y,
        },
        button: match event.button {
            1 => MouseEventButton::Auxiliary,
            2 => MouseEventButton::Secondary,
            3 => MouseEventButton::Fourth,
            4 => MouseEventButton::Fifth,
            _ => MouseEventButton::Main,
        },
        buttons: MouseEventButtons::from_bits_truncate(event.buttons),
        mods: Modifiers::from_bits_truncate(event.modifiers),
        details: Default::default(),
        element: Default::default(),
        active_pointers: Default::default(),
    }
}

/// The event `name` with what crossed of its payload, if the layer has
/// that event here.
fn event(name: &str, target: NodeId, data: &EventData) -> Option<DomEvent> {
    let data = match (name, data) {
        ("keydown", EventData::Key(key)) => DomEventData::KeyDown(key_event(key)),
        ("keyup", EventData::Key(key)) => DomEventData::KeyUp(key_event(key)),
        ("keypress", EventData::Key(key)) => DomEventData::KeyPress(key_event(key)),
        ("input", EventData::Input(value)) => DomEventData::Input(BlitzInputEvent {
            value: value.clone(),
        }),
        (_, EventData::Pointer(pointer)) => {
            let pointer = pointer_event(pointer);
            match name {
                "pointerdown" => DomEventData::PointerDown(pointer),
                "pointermove" => DomEventData::PointerMove(pointer),
                "pointerup" => DomEventData::PointerUp(pointer),
                "mousedown" => DomEventData::MouseDown(pointer),
                "mousemove" => DomEventData::MouseMove(pointer),
                "mouseup" => DomEventData::MouseUp(pointer),
                "click" => DomEventData::Click(pointer),
                "dblclick" => DomEventData::DoubleClick(pointer),
                "contextmenu" => DomEventData::ContextMenu(pointer),
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(DomEvent::new(target, data))
}

/// Builds the preview `view` makes, under an owner that is the plugin's
/// for as long as it is loaded.
pub fn mount(view: impl FnOnce() -> Option<El>) -> Option<u64> {
    let _ = Executor::init_futures_executor();
    let owner = Owner::new();
    owner.set();
    let root = view();
    KEPT.with(|kept| *kept.owner.borrow_mut() = Some(owner));
    root.map(id)
}

/// Runs the handlers at `node` for an event the host says reached it.
pub fn handle(at: u64, target: u64, name: &str, capture: bool, data: &EventData) -> Handled {
    // Cloned out: a handler may add handlers (a row made in it).
    let handlers: Vec<Handler> = KEPT.with(|kept| {
        (kept
            .handlers
            .borrow()
            .get(&(node(at), capture))
            .into_iter()
            .flatten())
        .filter(|(event, _)| *event == name)
        .map(|(_, handler)| handler.clone())
        .collect()
    });
    if let Some(event) = event(name, node(target), data) {
        for handler in handlers {
            handler(&event);
        }
    }
    Handled {
        prevent_default: PREVENTED.replace(false),
        stop_propagation: STOPPED.replace(false),
        asks_page: KEPT.with(|kept| !kept.dispatched.borrow().is_empty()),
    }
}

/// Runs what handlers left to ask the page.
pub fn after_dispatch() {
    let work = KEPT.with(|kept| std::mem::take(&mut *kept.dispatched.borrow_mut()));
    let page = Page(PhantomData);
    for work in work {
        work(&page);
    }
}

/// Runs the work that follows a layout.
pub fn after_layout() {
    // Taken out while it runs: work may register more, or retire some.
    let mut work = KEPT.with(|kept| {
        let mut work = std::mem::take(&mut *kept.laid_out.borrow_mut());
        work.retain(|(id, _)| !kept.retired.borrow_mut().remove(id));
        work
    });
    let page = Page(PhantomData);
    for (_, work) in &mut work {
        work(&page);
    }
    KEPT.with(|kept| {
        work.append(&mut kept.laid_out.borrow_mut());
        *kept.laid_out.borrow_mut() = work;
    });
}

/// Runs the effects of the signals written since the last call.
pub fn run_effects() {
    Executor::poll_local();
}

/// Makes this crate a plugin: `$names` lists its previews, `$css` is
/// their style sheet and `$view` builds one by name.
#[macro_export]
macro_rules! plugin {
    (names: $names:expr, css: $css:expr, view: $view:expr $(,)?) => {
        struct __HorizonPlugin;

        impl $crate::guest::bindings::Guest for __HorizonPlugin {
            fn names() -> Vec<String> {
                $names
            }

            fn css() -> String {
                $css
            }

            fn mount(name: String) -> Option<u64> {
                $crate::guest::mount(|| $view(&name))
            }

            fn handle(
                node: u64,
                target: u64,
                event: String,
                capture: bool,
                data: $crate::guest::bindings::EventData,
            ) -> $crate::guest::bindings::Handled {
                $crate::guest::handle(node, target, &event, capture, &data)
            }

            fn after_dispatch() {
                $crate::guest::after_dispatch()
            }

            fn after_layout() {
                $crate::guest::after_layout()
            }

            fn run_effects() {
                $crate::guest::run_effects()
            }
        }

        $crate::guest::bindings::export_plugin!(__HorizonPlugin with_types_in $crate::guest::bindings);
    };
}
