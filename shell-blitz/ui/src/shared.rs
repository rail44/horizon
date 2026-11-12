//! What the builders, the effects and the frame reach: the document, and
//! what the layer keeps by node.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName, ns};
use blitz_traits::events::DomEvent;

pub(crate) type Handler = Rc<dyn Fn(&DomEvent)>;
pub(crate) type Handlers = RefCell<HashMap<NodeId, Vec<(&'static str, Handler)>>>;
pub(crate) type Deferred = Box<dyn FnOnce(&mut BaseDocument)>;
/// Where an element's text is being written: x, y, width and height in
/// the element's own box, or nowhere to show.
pub(crate) type TextArea = Box<dyn Fn() -> Option<(f32, f32, f32, f32)>>;
pub(crate) type PasteTaker = Rc<dyn Fn(&str)>;
pub(crate) type PollWork = Box<dyn FnMut(&std::task::Waker)>;
/// Work done after every layout, under the id it is taken back out by.
pub(crate) type LayoutWork = (u64, Box<dyn FnMut(&mut BaseDocument)>);

pub(crate) struct Shared {
    pub(crate) document: Rc<RefCell<BaseDocument>>,
    pub(crate) handlers: Handlers,
    /// The handlers that hear an event on its way in, before its target.
    pub(crate) captures: Handlers,
    /// Changes to the document asked for while it was busy, made once it
    /// is free.
    pub(crate) later: RefCell<Vec<Deferred>>,
    /// What handlers asked of the document as it was when they ran.
    pub(crate) dispatched: RefCell<Vec<Deferred>>,
    /// What is to have the keyboard focus once the document is next laid
    /// out, and whether that is the element itself or the first thing
    /// inside it that takes focus.
    pub(crate) focus: Cell<Option<(NodeId, bool)>>,
    /// Whether work done after a layout asked for another.
    pub(crate) relayout: Cell<bool>,
    /// The elements that take text without being text fields.
    pub(crate) text_targets: RefCell<HashMap<NodeId, TextArea>>,
    /// The element the input method was last turned on for from here.
    pub(crate) composing_for: Cell<Option<NodeId>>,
    /// What takes a paste when the focus is on an element that is not a
    /// text field.
    pub(crate) pastes: RefCell<HashMap<NodeId, PasteTaker>>,
    /// What runs after every layout, with the boxes that layout made.
    pub(crate) laid_out: RefCell<Vec<LayoutWork>>,
    /// The ids of the work whose owner has gone, to be dropped from
    /// `laid_out` before it next runs.
    pub(crate) retired: RefCell<HashSet<u64>>,
    /// What runs when the shell polls the document.
    pub(crate) polled: RefCell<Vec<PollWork>>,
}

impl Shared {
    pub(crate) fn new(document: BaseDocument) -> Self {
        Self {
            document: Rc::new(RefCell::new(document)),
            handlers: RefCell::default(),
            captures: RefCell::default(),
            later: RefCell::default(),
            dispatched: RefCell::default(),
            focus: Cell::new(None),
            relayout: Cell::new(false),
            text_targets: RefCell::default(),
            composing_for: Cell::new(None),
            pastes: RefCell::default(),
            laid_out: RefCell::default(),
            retired: RefCell::default(),
            polled: RefCell::default(),
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Rc<Shared>>> = const { RefCell::new(None) };
}

/// Makes `shared` the app that builders and effects on this thread reach.
pub(crate) fn enter(shared: &Rc<Shared>) {
    CURRENT.with(|current| *current.borrow_mut() = Some(shared.clone()));
}

pub(crate) fn current() -> Option<Rc<Shared>> {
    CURRENT.with(|current| current.borrow().clone())
}

pub(crate) fn shared() -> Rc<Shared> {
    current().expect("inside an App")
}

/// An id no other piece of work on any app has: its owner's cleanup may
/// run while another app is the current one.
pub(crate) fn fresh_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn with_document<R>(work: impl FnOnce(&mut BaseDocument) -> R) -> R {
    let shared = shared();
    let mut document = shared.document.borrow_mut();
    work(&mut document)
}

/// Changes the document: now if it is free, and otherwise (from a handler
/// the document is running, or from work done after a layout) once it is.
pub(crate) fn write(work: impl FnOnce(&mut BaseDocument) + 'static) {
    let shared = shared();
    let Ok(mut document) = shared.document.try_borrow_mut() else {
        shared.later.borrow_mut().push(Box::new(work));
        return;
    };
    work(&mut document);
}

pub(crate) fn name(local: &str) -> QualName {
    QualName::new(None, ns!(), LocalName::from(local))
}

/// Takes a node out of the document for good with everything inside it,
/// and with it what the layer kept for those nodes.
pub(crate) fn drop_node(shared: &Shared, document: &mut BaseDocument, node: NodeId) {
    if document.get_node(node).is_none() {
        return;
    }
    let mut pending = vec![node];
    while let Some(id) = pending.pop() {
        if let Some(node) = document.get_node(id) {
            pending.extend(node.children.iter().copied());
        }
        shared.handlers.borrow_mut().remove(&id);
        shared.captures.borrow_mut().remove(&id);
        shared.text_targets.borrow_mut().remove(&id);
        shared.pastes.borrow_mut().remove(&id);
    }
    document.mutate().remove_and_drop_node(node);
}
