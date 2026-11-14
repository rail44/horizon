//! What the builders, the effects and the frame reach: the document, and
//! what the layer keeps by node.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName, ns};
use blitz_traits::events::DomEvent;

pub(crate) type Handler = Rc<dyn Fn(&DomEvent)>;
pub(crate) type Handlers = RefCell<HashMap<NodeId, Vec<(&'static str, Handler)>>>;
pub(crate) type Deferred = Box<dyn FnOnce(&mut BaseDocument)>;
/// Where an element's text is being written: x, y, width and height in
/// the element's own box, or nowhere to show.
pub(crate) type TextArea = Box<dyn Fn() -> Option<(f32, f32, f32, f32)>>;
pub(crate) type PasteTaker = Rc<dyn Fn(&str)>;
/// Makes the widget that paints an element.
pub(crate) type WidgetMaker = Rc<dyn Fn() -> Box<dyn blitz_dom::Widget>>;
pub(crate) type PollWork = Box<dyn FnMut(&std::task::Waker)>;
/// Work done after every layout, under the id it is taken back out by.
pub(crate) type LayoutWork = (u64, Box<dyn FnMut(&mut BaseDocument)>);

/// Work done whenever the signals' effects run, under the id it is taken
/// back out by.
pub(crate) type EffectsWork = (u64, Box<dyn FnMut()>);

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
    /// Whether a handler of the input event being dispatched asked the
    /// document not to do what it would have done with it.
    pub(crate) prevented: Cell<bool>,
    /// Whether work done after a layout asked for another.
    pub(crate) relayout: Cell<bool>,
    /// The elements that take text without being text fields.
    pub(crate) text_targets: RefCell<HashMap<NodeId, TextArea>>,
    /// The element the input method was last turned on for from here.
    pub(crate) composing_for: Cell<Option<NodeId>>,
    /// What takes a paste when the focus is on an element that is not a
    /// text field.
    pub(crate) pastes: RefCell<HashMap<NodeId, PasteTaker>>,
    /// What makes the widget of each element that has one: blitz-dom lets
    /// go of a node's widget when the node is taken out of its parent,
    /// also to be put somewhere else.
    pub(crate) widgets: RefCell<HashMap<NodeId, WidgetMaker>>,
    /// What runs after every layout, with the boxes that layout made.
    pub(crate) laid_out: RefCell<Vec<LayoutWork>>,
    /// The ids of the work whose owner has gone, to be dropped from
    /// `laid_out` before it next runs.
    pub(crate) retired: RefCell<HashSet<u64>>,
    /// What runs when the shell polls the document.
    pub(crate) polled: RefCell<Vec<PollWork>>,
    /// Whether the document was written to since the shell last asked.
    pub(crate) written: Cell<bool>,
    /// The style sheets already in the document, by name (`sheet.rs`).
    pub(crate) sheets: RefCell<HashSet<String>>,
    /// Whether something woke the page since the shell last asked: a
    /// session's thread with a frame, say. The page as first built counts.
    pub(crate) woken: Arc<AtomicBool>,
    /// The waker handed to what runs on a poll, and the shell's that it
    /// is around.
    pub(crate) waker: RefCell<Option<(std::task::Waker, std::task::Waker)>>,
    /// What runs whenever the signals' effects do, under the id it is
    /// taken back out by: the effects of a graph that is not this
    /// thread's own.
    pub(crate) effects: RefCell<Vec<EffectsWork>>,
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
            prevented: Cell::new(false),
            relayout: Cell::new(false),
            text_targets: RefCell::default(),
            composing_for: Cell::new(None),
            pastes: RefCell::default(),
            widgets: RefCell::default(),
            laid_out: RefCell::default(),
            retired: RefCell::default(),
            polled: RefCell::default(),
            written: Cell::new(false),
            sheets: RefCell::default(),
            woken: Arc::new(AtomicBool::new(true)),
            waker: RefCell::default(),
            effects: RefCell::default(),
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
    shared.written.set(true);
    let mut document = shared.document.borrow_mut();
    work(&mut document)
}

/// The shell's waker with a note taken of its being woken: what a
/// session's thread wakes when it has something for the page.
struct Noting {
    woken: Arc<AtomicBool>,
    shell: std::task::Waker,
}

impl std::task::Wake for Noting {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Relaxed);
        self.shell.wake_by_ref();
    }
}

impl Shared {
    /// The waker for what runs on a poll: the shell's, noting in `woken`
    /// that it was woken.
    pub(crate) fn waker(&self, shell: &std::task::Waker) -> std::task::Waker {
        let mut kept = self.waker.borrow_mut();
        match kept.as_ref() {
            Some((around, noting)) if around.will_wake(shell) => noting.clone(),
            _ => {
                let noting = std::task::Waker::from(Arc::new(Noting {
                    woken: self.woken.clone(),
                    shell: shell.clone(),
                }));
                *kept = Some((shell.clone(), noting.clone()));
                noting
            }
        }
    }
}

/// Changes the document: now if it is free, and otherwise (from a handler
/// the document is running, or from work done after a layout) once it is.
pub(crate) fn write(work: impl FnOnce(&mut BaseDocument) + 'static) {
    let shared = shared();
    shared.written.set(true);
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
        shared.widgets.borrow_mut().remove(&id);
    }
    document.mutate().remove_and_drop_node(node);
}
