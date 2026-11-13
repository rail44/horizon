//! A document with a view built into it, and its way in for a shell.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use any_spawner::Executor;
use blitz_dom::{BaseDocument, Document, DocumentConfig, EventDriver, UserAgentStylesheet};
use blitz_traits::events::UiEvent;
use blitz_traits::shell::{ColorScheme, Viewport};
use reactive_graph::owner::Owner;

use crate::dispatch::Dispatch;
use crate::el::{El, el};
use crate::frame::{frame_of, run_effects};
use crate::input_method::ime_cursor_area;
use crate::shared::{Shared, enter, with_document};

/// A document, the view built into it, and the signals' effects.
pub struct App {
    pub(crate) shared: Rc<Shared>,
    owner: Owner,
}

impl Drop for App {
    /// Ends what the view made: its effects, and what it kept (a session's
    /// attachment, a connection). The thread goes on holding the owner as
    /// its current one, so letting go of it here would end nothing.
    fn drop(&mut self) {
        self.owner.cleanup();
    }
}

impl App {
    /// Builds `view` once into a new document of `width` by `height`,
    /// styled by `css`.
    pub fn new(width: u32, height: u32, css: &str, view: impl FnOnce() -> El) -> Self {
        // One executor a thread; a second `App` on it finds it set.
        let _ = Executor::init_futures_executor();
        // A root of its own: not a child of whatever owner the thread had
        // last, an earlier app's.
        if let Some(earlier) = Owner::current() {
            earlier.unset();
        }
        let owner = Owner::new();
        owner.set();
        let mut document = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(width, height, 1.0, ColorScheme::Light)),
            ..DocumentConfig::default()
        });
        // The page fills the window and has no margin of its own; a view's
        // rules come after and are about the view.
        // A view's own style sheet is kept to the view with `@scope`
        // (`sheet.rs`), which Stylo has and leaves off.
        stylo_static_prefs::set_pref!("layout.css.at-scope.enabled", true);
        let css = format!("html, body {{ margin: 0; height: 100%; }}\n{css}");
        document.add_user_agent_stylesheet(UserAgentStylesheet::parse(&css));
        let shared = Rc::new(Shared::new(document));
        enter(&shared);
        let body = el("body");
        let html = el("html").child(body);
        with_document(|document| {
            let root = document.root_node().id;
            document.mutate().append_children(root, &[html.0]);
        });
        let view = view();
        body.child(view);
        let app = Self { shared, owner };
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
        run_effects(&self.shared);
    }

    /// Restyles and lays out what the writes since the last call touched.
    pub fn resolve(&self) {
        self.resolve_at(0.0);
    }

    /// [`resolve`](Self::resolve) with the clock CSS animations run on at
    /// `seconds`.
    pub fn resolve_at(&self, seconds: f64) {
        frame_of(&self.shared, seconds);
    }

    /// How many things the layer keeps by element (handlers, and what
    /// takes text and pastes): for a test that what it keeps for an
    /// element goes with the element.
    pub fn kept(&self) -> usize {
        let handlers = |kept: &crate::shared::Handlers| -> usize {
            kept.borrow().values().map(Vec::len).sum()
        };
        handlers(&self.shared.handlers)
            + handlers(&self.shared.captures)
            + self.shared.text_targets.borrow().len()
            + self.shared.pastes.borrow().len()
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
        if let UiEvent::Paste(text) = &event {
            let focused = self.shared.document.borrow().get_focussed_node_id();
            let taker = focused.and_then(|id| self.shared.pastes.borrow().get(&id).cloned());
            if let Some(taker) = taker {
                enter(&self.shared);
                taker(text);
                self.run_effects();
                return;
            }
        }
        let mut document = self.shared.document.clone();
        let handler = Dispatch(self.shared.clone());
        EventDriver::new(&mut document as &mut dyn Document, handler).handle_ui_event(event);
        if let Some((x, y)) = pressed {
            self.focus_from(x, y);
        }
        enter(&self.shared);
        let dispatched = std::mem::take(&mut *self.shared.dispatched.borrow_mut());
        for work in dispatched {
            work(&mut self.shared.document.borrow_mut());
        }
        self.run_effects();
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
        ime_cursor_area(&self.shared.document.borrow())
    }
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

    /// Whether there is something to draw: the document was written to,
    /// or something woke the page (a session with a frame), since the
    /// shell last asked. The shell asks after every event of the window,
    /// a redraw among them, and redraws on a yes: a yes with nothing
    /// behind it would have it redraw without end.
    fn poll(&mut self, task_context: Option<std::task::Context>) -> bool {
        let Some(context) = task_context else {
            return false;
        };
        let shared = &self.0.shared;
        let waker = shared.waker(context.waker());
        enter(shared);
        for work in shared.polled.borrow_mut().iter_mut() {
            work(&waker);
        }
        self.0.run_effects();
        let woken = shared.woken.swap(false, Ordering::Relaxed);
        shared.written.replace(false) || woken
    }
}
