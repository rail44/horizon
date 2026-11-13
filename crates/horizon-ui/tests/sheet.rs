//! A view's CSS as a CSS module, beside the page's own sheet. A rule that
//! applies here gives its box a width, which is what is read.

use horizon_ui::{App, El, el};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set};

fn size(app: &App, element: El) -> (f32, f32) {
    let document = app.document();
    let document = document.borrow();
    let size = document.get_node(element.0).unwrap().final_layout().size;
    (size.width, size.height)
}

fn width(app: &App, element: El) -> f32 {
    size(app, element).0
}

fn classes(app: &App, element: El) -> String {
    let document = app.document();
    let document = document.borrow();
    let node = document.get_node(element.0).unwrap();
    (node.attr(horizon_ui::blitz_dom::local_name!("class")))
        .unwrap_or_default()
        .to_string()
}

mod outer {
    horizon_ui::css_module! {"
        .view { width: 400px;
            .row { width: 100px; }
            .row.chosen { width: 150px; }
            > * { height: 7px; }
        }
        .placed { width: 300px; }
    "}
}

mod inner {
    horizon_ui::css_module! {"
        :where(.view) { width: 200px; }
        .row { width: 50px; }
        .status-dot { width: 8px; }
    "}
}

/// A view that holds what it is given, in an element of its own.
fn holder(held: El) -> El {
    el("div").class(inner::VIEW).child(held)
}

/// Builds `view` as the page, and resolves it.
fn page<T>(sheet: &str, view: impl FnOnce() -> (El, T)) -> (App, T) {
    let mut made = None;
    let app = App::new(600, 400, sheet, || {
        let (root, kept) = view();
        made = Some(kept);
        root
    });
    app.resolve();
    (app, made.unwrap())
}

#[test]
fn two_modules_with_a_class_of_the_same_name_do_not_reach_each_other_s() {
    let (app, (outside, ours, theirs)) = page("", || {
        let outside = el("div").class("row");
        let ours = el("div").class(outer::ROW);
        let theirs = el("div").class(inner::ROW);
        let root = el("div").child(outside).child(
            el("div")
                .class(outer::VIEW)
                .child(ours)
                .child(holder(theirs)),
        );
        (root, (outside, ours, theirs))
    });
    assert_eq!(width(&app, outside), 600.);
    assert_eq!(width(&app, ours), 100.);
    assert_eq!(width(&app, theirs), 50.);
    assert_ne!(outer::ROW.name(), inner::ROW.name());
    assert!(outer::ROW.name().starts_with("row--"));
}

#[test]
fn what_a_view_hands_to_another_keeps_the_style_of_the_view_that_made_it() {
    let (app, handed) = page("", || {
        let handed = el("div").class(outer::ROW);
        let root = el("div").class(outer::VIEW).child(holder(handed));
        (root, handed)
    });
    assert_eq!(width(&app, handed), 100.);
}

#[test]
fn the_view_that_places_another_sizes_its_root_over_its_default() {
    let (app, (default, placed)) = page("", || {
        let default = holder(el("div"));
        let placed = holder(el("div")).class(outer::PLACED);
        let root = el("div").child(default).child(placed);
        (root, (default, placed))
    });
    assert_eq!(width(&app, default), 200.);
    assert_eq!(width(&app, placed), 300.);
}

#[test]
fn a_view_lays_out_what_it_holds_whatever_that_is() {
    let (app, held) = page("", || {
        let held = holder(el("div"));
        (el("div").class(outer::VIEW).child(held), held)
    });
    assert_eq!(size(&app, held), (200., 7.));
}

#[test]
fn a_class_that_changes_is_followed_and_the_other_classes_stay() {
    let chosen = RwSignal::new(false);
    let (app, row) = page("", || {
        let row = el("div")
            .class(outer::ROW)
            .class_dyn(move || chosen.get().then_some(outer::CHOSEN));
        (el("div").class(outer::VIEW).child(row), row)
    });
    assert_eq!(width(&app, row), 100.);
    chosen.set(true);
    app.run_effects();
    app.resolve();
    assert_eq!(width(&app, row), 150.);
    assert_eq!(
        classes(&app, row),
        format!("{} {}", outer::ROW.name(), outer::CHOSEN.name())
    );
    chosen.set(false);
    app.run_effects();
    app.resolve();
    assert_eq!(width(&app, row), 100.);
    assert_eq!(classes(&app, row), outer::ROW.name());
}

#[test]
fn a_view_put_on_the_page_later_has_its_sheet() {
    let shown = RwSignal::new(false);
    let late = RwSignal::new(None);
    let (app, ()) = page("", || {
        let root = el("div").when(
            move || shown.get(),
            move || {
                let row = el("div").class(inner::STATUS_DOT);
                late.set(Some(row));
                row
            },
        );
        (root, ())
    });
    shown.set(true);
    app.run_effects();
    app.resolve();
    assert_eq!(width(&app, late.get().unwrap()), 8.);
}

#[test]
fn the_page_s_sheet_reaches_every_view_and_a_module_s_rule_is_over_it() {
    let (app, (shared, own)) = page(".w-80 { width: 80px; }", || {
        let shared = el("div").class("w-80");
        let own = el("div").class(("w-80", inner::ROW));
        (holder(el("div").child(shared).child(own)), (shared, own))
    });
    assert_eq!(width(&app, shared), 80.);
    assert_eq!(width(&app, own), 50.);
}

#[test]
fn a_sheet_many_elements_have_is_in_the_document_once() {
    let (app, ()) = page("", || {
        let column = el("div");
        for _ in 0..5 {
            column.child(el("div").class(inner::ROW));
        }
        (column, ())
    });
    let document = app.document();
    let document = document.borrow();
    let mut styles = 0;
    let mut pending = vec![document.root_element().id];
    while let Some(id) = pending.pop() {
        let node = document.get_node(id).unwrap();
        let element = node.element_data();
        styles += usize::from(element.is_some_and(|element| &*element.name.local == "style"));
        pending.extend(node.children.iter().copied());
    }
    assert_eq!(styles, 1);
}
