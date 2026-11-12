//! What a host of plugins does to the document for them: the parts of
//! `El` that a view never names because `El` does them inside (a text
//! node, a list's marker, putting nodes in order), and the page lent for
//! as long as a plugin's questions about it are sound.
//!
//! A plugin's own `El` and `Page` (`guest`) are these, one call each.

use std::cell::Cell;

use blitz_dom::{BaseDocument, NodeId};

use crate::page::Page;
use crate::shared::{drop_node, shared, with_document, write};

pub fn create_text(text: &str) -> NodeId {
    with_document(|document| document.mutate().create_text_node(text))
}

pub fn create_comment(text: &str) -> NodeId {
    with_document(|document| document.mutate().create_comment_node(text))
}

pub fn set_text(node: NodeId, text: &str) {
    with_document(|document| document.mutate().set_node_text(node, text));
}

pub fn set_style(node: NodeId, property: &str, value: &str) {
    with_document(|document| (document.mutate()).set_style_property(node, property, value));
}

pub fn append(parent: NodeId, child: NodeId) {
    with_document(|document| document.mutate().append_children(parent, &[child]));
}

/// Puts `nodes`, in order, just before `marker` among its parent's.
pub fn insert_before(marker: NodeId, nodes: &[NodeId]) {
    with_document(|document| document.mutate().insert_nodes_before(marker, nodes));
}

/// Takes a node out for good with everything inside it, now if the
/// document is free and otherwise once it is: the nodes that go.
pub fn remove(node: NodeId) -> Vec<NodeId> {
    let shared = shared();
    let mut gone = Vec::new();
    {
        // (Read through whichever borrow there is: the tree is not
        // changing under a handler or under work after a layout.)
        let subtree = |document: &BaseDocument, gone: &mut Vec<NodeId>| {
            let mut pending = vec![node];
            while let Some(id) = pending.pop() {
                if let Some(found) = document.get_node(id) {
                    gone.push(id);
                    pending.extend(found.children.iter().copied());
                }
            }
        };
        match shared.document.try_borrow() {
            Ok(document) => subtree(&document, &mut gone),
            Err(_) => {
                if let Some(page) = PAGE.get() {
                    // SAFETY: see `lent`.
                    subtree(unsafe { &*page }, &mut gone);
                }
            }
        }
    }
    write(move |document| drop_node(&crate::shared::shared(), document, node));
    gone
}

/// Whether the document can be changed now: not while it runs work after
/// a layout or after an event, when a change (other than the ones the
/// layer leaves for later itself: a scroll, a field's value, taking a
/// node out) has to wait for an effect.
pub fn free() -> bool {
    shared().document.try_borrow_mut().is_ok()
}

thread_local! {
    static PAGE: Cell<Option<*const BaseDocument>> = const { Cell::new(None) };
}

/// Lends `page` for as long as `work` runs: what [`lent`] answers from.
pub fn lend<R>(page: &Page, work: impl FnOnce() -> R) -> R {
    struct Restore(Option<*const BaseDocument>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PAGE.set(self.0);
        }
    }
    let _restore = Restore(PAGE.replace(Some(page.0 as *const BaseDocument)));
    work()
}

/// Asks the page lent just now, if one is.
pub fn lent<R>(ask: impl FnOnce(&Page) -> R) -> Option<R> {
    let page = PAGE.get()?;
    // SAFETY: the pointer is set only by `lend`, from a reference that
    // outlives `lend`'s call, and unset when that call ends, also by a
    // panic; this runs inside it, on the same thread, and `ask` cannot
    // keep what it is handed.
    Some(ask(&Page(unsafe { &*page })))
}
