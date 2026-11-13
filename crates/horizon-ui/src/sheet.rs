//! A style sheet that is an element's own: its rules reach what is under
//! the element and nothing else.
//!
//! A view writes plain CSS, with `:host` for the element the sheet was
//! given to, as a sheet in a shadow tree is written. How the rules are kept
//! to that element is this module's alone: today the sheet is wrapped in
//! CSS's `@scope`, from the element down to the next element with a sheet
//! of its own. A view placed inside another is not reached by the outer
//! one's rules; its root is, so that the outer view can size and place it.
//!
//! A sheet can also be provided: its rules reach everything under the
//! element, the views placed there included. That is how a parent or the
//! shell hands down what is shared (the theme's variables, a vocabulary of
//! utility classes).

use std::hash::{DefaultHasher, Hash, Hasher};

use blitz_dom::{BaseDocument, LocalName, NodeId, QualName, ns};

use crate::shared::{name, shared};

/// On an element with a sheet of its own: the sheet's name.
const OWN: &str = "data-sheet";
/// On an element that provides a sheet: the sheet's name.
const PROVIDED: &str = "data-provides";

/// `:host` and `:host(…)` as `:scope` and `:scope…`: the element the
/// sheet was given to, as `@scope` names it.
fn host_as_scope(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find(":host") {
        out += &rest[..at];
        rest = &rest[at + ":host".len()..];
        out += ":scope";
        // `:host(.selected)` is the host when it is also `.selected`.
        if let Some(inner) = rest.strip_prefix('(') {
            let mut depth = 1;
            let end = inner.char_indices().find(|(_, character)| {
                match character {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                depth == 0
            });
            if let Some((end, _)) = end {
                out += &inner[..end];
                rest = &inner[end + 1..];
            }
        }
    }
    out + rest
}

/// The sheet as the document takes it.
fn wrapped(css: &str, sheet: &str, provided: bool) -> String {
    let rules = host_as_scope(css);
    match provided {
        true => format!("@scope ([{PROVIDED}=\"{sheet}\"]) {{\n{rules}\n}}"),
        // Down to what is inside the next element with a sheet of its
        // own, and not that element itself.
        false => format!("@scope ([{OWN}=\"{sheet}\"]) to ([{OWN}] > *) {{\n{rules}\n}}"),
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
    let attribute = if provided { PROVIDED } else { OWN };
    document
        .mutate()
        .set_attribute(node, name(attribute), &sheet);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_the_scope_s_root() {
        assert_eq!(
            host_as_scope(":host { a: b } :host(:focus) .row, :host(.x:not(.y)) > .z { c: d }"),
            ":scope { a: b } :scope:focus .row, :scope.x:not(.y) > .z { c: d }"
        );
        assert_eq!(host_as_scope(".row { a: b }"), ".row { a: b }");
    }
}
