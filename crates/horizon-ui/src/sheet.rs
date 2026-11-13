//! An element's classes as the document has them, and the sheets of the
//! CSS modules they are from.
//!
//! A module's sheet is put in the document when one of its classes is
//! first given to an element, once. Its rules are kept to the view by the
//! names alone: no other sheet has them (`class.rs`).

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName, local_name, ns};

use crate::class::Classes;
use crate::shared::shared;

/// Puts the sheet `id` in the document, unless it is there.
pub(crate) fn adopt(document: &mut BaseDocument, id: &str, css: &str) {
    if shared().sheets.borrow().contains(id) {
        return;
    }
    shared().sheets.borrow_mut().insert(id.to_string());
    let mut mutator = document.mutate();
    let style = mutator.create_element(
        QualName::new(None, ns!(html), LocalName::from("style")),
        Vec::new(),
    );
    let text = mutator.create_text_node(css);
    mutator.append_children(style, &[text]);
    drop(mutator);
    // In the head, so that the page's words are the body's alone.
    let top = document.root_element();
    let head = (top.children.iter().copied())
        .find(|child| {
            let element = document
                .get_node(*child)
                .and_then(|node| node.element_data());
            element.is_some_and(|element| &*element.name.local == "head")
        })
        .unwrap_or(top.id);
    document.mutate().append_children(head, &[style]);
}

/// Takes `remove` out of `node`'s classes and puts `add` in, leaving the
/// others as they are.
pub(crate) fn change_named(
    document: &mut BaseDocument,
    node: NodeId,
    remove: &[String],
    add: &[String],
) {
    let Some(found) = document.get_node(node) else {
        return;
    };
    let before = found.attr(local_name!("class")).unwrap_or_default();
    let mut classes: Vec<&str> = before
        .split_whitespace()
        .filter(|class| !remove.iter().any(|removed| removed == class))
        .collect();
    for class in add {
        if !classes.contains(&class.as_str()) {
            classes.push(class);
        }
    }
    let after = classes.join(" ");
    if after != before {
        (document.mutate()).set_attribute(node, crate::shared::name("class"), &after);
    }
}

/// Takes `remove` out of `node`'s classes and puts `add` in, with the
/// sheets `add`'s classes are from: the classes put in.
pub(crate) fn change(
    document: &mut BaseDocument,
    node: NodeId,
    remove: &[String],
    add: &impl Classes,
) -> Vec<String> {
    let mut added = Vec::new();
    add.each(&mut |class, sheet| {
        if let Some(sheet) = sheet {
            adopt(document, sheet.id, sheet.css);
        }
        added.push(class.to_string());
    });
    change_named(document, node, remove, &added);
    added
}
