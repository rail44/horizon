//! A style sheet that is an element's own, beside the page's. A rule that
//! applies here gives its box a width, which is what is read.

use horizon_ui::{App, El, el};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set};

fn width(app: &App, element: El) -> f32 {
    let document = app.document();
    let document = document.borrow();
    let node = document.get_node(element.0).unwrap();
    node.final_layout().size.width
}

/// An outer view with a row of its own and an inner view with a row of
/// its own, beside a row that is in neither.
struct Page {
    app: App,
    outside: El,
    outer: El,
    outer_row: El,
    inner: El,
    inner_row: El,
}

fn page(outer_css: &'static str, inner_css: &'static str, chosen: Option<RwSignal<bool>>) -> Page {
    let mut made = None;
    let app = App::new(600, 400, "", || {
        let outside = el("div").class("row");
        let outer_row = match chosen {
            Some(chosen) => el("div").class_dyn(move || match chosen.get() {
                true => "row chosen".to_string(),
                false => "row".to_string(),
            }),
            None => el("div").class("row"),
        };
        let inner_row = el("div").class("row");
        let inner = el("div").class("inner").styled(inner_css).child(inner_row);
        let outer = el("div")
            .class("outer")
            .styled(outer_css)
            .child(outer_row)
            .child(inner);
        made = Some((outside, outer, outer_row, inner, inner_row));
        el("div").child(outside).child(outer)
    });
    app.resolve();
    let (outside, outer, outer_row, inner, inner_row) = made.unwrap();
    Page {
        app,
        outside,
        outer,
        outer_row,
        inner,
        inner_row,
    }
}

#[test]
fn a_view_s_sheet_reaches_what_is_its_own_and_stops_at_a_view_inside_it() {
    let page = page(".row { width: 100px; }", ".row { width: 50px; }", None);
    assert_eq!(width(&page.app, page.outside), 600.);
    assert_eq!(width(&page.app, page.outer_row), 100.);
    assert_eq!(width(&page.app, page.inner_row), 50.);
}

#[test]
fn a_view_names_its_own_root_and_the_outer_view_can_size_the_inner_one() {
    let page = page(
        ".outer { width: 400px; } .inner { width: 200px; } .row { width: 100px; }",
        ".inner { height: 30px; } .row { width: 50px; }",
        None,
    );
    assert_eq!(width(&page.app, page.outer), 400.);
    // The inner view's root is the outer view's to place; what is inside
    // it is not.
    assert_eq!(width(&page.app, page.inner), 200.);
    assert_eq!(width(&page.app, page.inner_row), 50.);
    let document = page.app.document();
    let document = document.borrow();
    let inner = document.get_node(page.inner.0).unwrap();
    assert_eq!(inner.final_layout().size.height, 30.);
}

#[test]
fn a_class_that_changes_is_followed_by_the_sheet_s_rules() {
    let chosen = RwSignal::new(false);
    let page = page(
        ".row { width: 100px; } .row.chosen { width: 150px; }",
        "",
        Some(chosen),
    );
    assert_eq!(width(&page.app, page.outer_row), 100.);
    chosen.set(true);
    page.app.run_effects();
    page.app.resolve();
    assert_eq!(width(&page.app, page.outer_row), 150.);
    chosen.set(false);
    page.app.run_effects();
    page.app.resolve();
    assert_eq!(width(&page.app, page.outer_row), 100.);
}

#[test]
fn a_view_put_on_the_page_later_has_its_sheet() {
    let shown = RwSignal::new(false);
    let app = App::new(600, 400, "", || {
        el("div").when(
            move || shown.get(),
            || el("div").class("late").styled(".late { width: 222px; }"),
        )
    });
    app.resolve();
    shown.set(true);
    app.run_effects();
    app.resolve();
    let document = app.document();
    let document = document.borrow();
    let mut widths = Vec::new();
    let mut pending = vec![document.root_element().id];
    while let Some(id) = pending.pop() {
        let node = document.get_node(id).unwrap();
        if node.attr(horizon_ui::blitz_dom::local_name!("class")) == Some("late") {
            widths.push(node.final_layout().size.width);
        }
        pending.extend(node.children.iter().copied());
    }
    assert_eq!(widths, [222.]);
}

#[test]
fn the_page_s_sheet_reaches_into_every_view_and_a_view_s_own_rule_is_over_it() {
    let mut made = None;
    let app = App::new(600, 400, ".w-80 { width: 80px; }", || {
        let shared = el("div").class("w-80");
        let own = el("div").class("w-80 own");
        made = Some((shared, own));
        el("div").child(
            el("div").styled(".x { width: 1px; }").child(
                el("div")
                    .styled(".own { width: 40px; }")
                    .child(shared)
                    .child(own),
            ),
        )
    });
    app.resolve();
    let (shared, own) = made.unwrap();
    assert_eq!(width(&app, shared), 80.);
    assert_eq!(width(&app, own), 40.);
}

#[test]
fn a_variable_a_view_sets_is_read_inside_a_view_placed_in_it() {
    let page = page(
        ".outer { --row-width: 70px; }",
        ".row { width: var(--row-width, 50px); }",
        None,
    );
    assert_eq!(width(&page.app, page.inner_row), 70.);
}

#[test]
fn a_view_s_sheet_can_order_its_rules_in_layers() {
    let page = page(
        "@layer base, over;
         @layer over { .row { width: 100px; } }
         @layer base { .row { width: 20px; } }",
        "",
        None,
    );
    assert_eq!(width(&page.app, page.outer_row), 100.);
}

#[test]
fn a_view_s_own_rule_for_its_root_is_over_the_rule_of_the_view_that_places_it() {
    let page = page(".inner { width: 200px; }", ".inner { width: 300px; }", None);
    assert_eq!(width(&page.app, page.inner), 300.);
}

#[test]
fn a_sheet_many_elements_have_is_in_the_document_once() {
    let app = App::new(600, 400, "", || {
        let column = el("div");
        for _ in 0..5 {
            column.child(el("div").styled(".row { width: 100px; }"));
        }
        column
    });
    app.resolve();
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
