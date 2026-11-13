//! A style sheet that is an element's own: its rules reach the element
//! and what is under it, and nothing else.
//!
//! A view writes plain CSS, and names its own root as it names anything
//! (`.board-list { … }`). How the rules are kept to that element is this
//! module's alone. Today the sheet is wrapped in CSS's `@scope`. The scope
//! starts at the element that holds the view's root, so that the root is
//! in it as any of its own elements is, and leaves out the other things
//! that element holds. It ends at what is inside a view placed in this
//! one: that view's root is reached, so that this one can size and place
//! it, and what is under it is not.
//!
//! A sheet can also be provided: its rules reach the element and
//! everything under it, the views placed there included. That is how a
//! parent or the shell hands down what is shared (the theme's variables,
//! a vocabulary of utility classes).

use std::hash::{DefaultHasher, Hash, Hasher};

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName, ns};

use crate::shared::{name, shared};

/// On an element with a sheet of its own: the sheet's name.
const OWN: &str = "data-sheet";
/// On an element that provides a sheet: the sheet's name.
const PROVIDED: &str = "data-provides";
/// On an element that holds one with a sheet of its own, or one that
/// provides a sheet: those sheets' names, between spaces.
const HOLDS: &str = "data-holds";
const HOLDS_PROVIDED: &str = "data-holds-provided";

/// The sheet as the document takes it.
fn wrapped(css: &str, sheet: &str, provided: bool) -> String {
    match provided {
        true => format!(
            "@scope ([{HOLDS_PROVIDED}~=\"{sheet}\"]) \
             to (:scope > :not([{PROVIDED}=\"{sheet}\"])) {{\n{css}\n}}"
        ),
        // Not the other things the holder holds, and not what is inside a
        // view placed in this one (that view's root is two elements with
        // sheets down from the holder).
        false => format!(
            "@scope ([{HOLDS}~=\"{sheet}\"]) \
             to (:scope > :not([{OWN}=\"{sheet}\"]), [{OWN}] [{OWN}] > *) {{\n{css}\n}}"
        ),
    }
}

fn attribute(document: &BaseDocument, node: NodeId, attribute: &str) -> Option<String> {
    let node = document.get_node(node)?;
    node.attr(LocalName::from(attribute)).map(str::to_string)
}

/// Notes on `holder` that it holds `child`, if `child` has a sheet: called
/// wherever an element is put in another.
pub(crate) fn held(document: &mut BaseDocument, holder: NodeId, child: NodeId) {
    for (of_child, of_holder) in [(OWN, HOLDS), (PROVIDED, HOLDS_PROVIDED)] {
        let Some(sheet) = attribute(document, child, of_child) else {
            continue;
        };
        let noted = attribute(document, holder, of_holder).unwrap_or_default();
        if noted.split(' ').any(|noted| noted == sheet) {
            continue;
        }
        let noted = match noted.is_empty() {
            true => sheet,
            false => format!("{noted} {sheet}"),
        };
        (document.mutate()).set_attribute(holder, name(of_holder), &noted);
    }
}

/// Gives `node` the sheet `css`: its own, or provided to all under it. A
/// sheet's text is put in the document once, however many elements have
/// it.
pub(crate) fn adopt(document: &mut BaseDocument, node: NodeId, css: &str, provided: bool) {
    if css.trim().is_empty() {
        return;
    }
    let mut hasher = DefaultHasher::new();
    (css, provided).hash(&mut hasher);
    let sheet = format!("{:x}", hasher.finish());
    let own = if provided { PROVIDED } else { OWN };
    document.mutate().set_attribute(node, name(own), &sheet);
    // (Given its sheet after it was put somewhere.)
    if let Some(holder) = document.get_node(node).and_then(|node| node.parent) {
        held(document, holder, node);
    }
    if !shared().sheets.borrow_mut().insert(sheet.clone()) {
        return;
    }
    let mut mutator = document.mutate();
    let style = mutator.create_element(
        QualName::new(None, ns!(html), LocalName::from("style")),
        Vec::new(),
    );
    let text = mutator.create_text_node(&wrapped(css, &sheet, provided));
    mutator.append_children(style, &[text]);
    drop(mutator);
    let top = document.root_element().id;
    document.mutate().append_children(top, &[style]);
}
