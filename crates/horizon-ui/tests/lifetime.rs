//! What the layer keeps for an element ends with the element: its
//! handlers, and the work it asked for after each layout.

use std::cell::Cell;
use std::rc::Rc;

use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set};
use horizon_ui::windowed::windowed;
use horizon_ui::{App, each_layout, el};

const CSS: &str = "
    .list { height: 300px; overflow-y: scroll; }
    .row { height: 40px; }
";

#[test]
fn a_dropped_rows_handlers_and_layout_work_go_with_it() {
    let rows = RwSignal::new(vec![1u32, 2, 3]);
    let layouts = Rc::new(Cell::new(0));
    let counted = layouts.clone();
    let app = App::new(400, 300, CSS, move || {
        el("div").each(
            move || rows.get(),
            |row| *row,
            move |_| {
                let counted = counted.clone();
                // Work a row asks for after each layout, as a view of its
                // own would.
                each_layout(move |_| counted.set(counted.get() + 1));
                el("div")
                    .class("row")
                    .on("click", |_| {})
                    .on_capture("keydown", |_| {})
                    .on_paste(|_| {})
                    .takes_text(|| None)
            },
        )
    });
    app.resolve();
    assert_eq!(app.kept(), 12);
    assert_eq!(layouts.get(), 3);

    rows.set(vec![2]);
    app.run_effects();
    assert_eq!(app.kept(), 4);
    layouts.set(0);
    app.resolve();
    assert_eq!(layouts.get(), 1);

    rows.set(Vec::new());
    app.run_effects();
    app.resolve();
    assert_eq!(app.kept(), 0);
    layouts.set(0);
    app.resolve();
    assert_eq!(layouts.get(), 0);
}

#[test]
fn a_long_list_builds_the_rows_near_the_view_and_lays_out_again_within_a_frame() {
    let built = RwSignal::new((0, 0));
    let mut list = None;
    let app = App::new(400, 300, CSS, || {
        let scroller = el("div").class("list");
        windowed(
            scroller,
            || (0..2000u32).collect::<Vec<_>>(),
            |row| *row,
            |row| el("div").class("row").text(&format!("row {row}")),
            false,
            built,
        );
        list = Some(scroller);
        scroller
    });
    let list = list.unwrap();
    // One call lays out as many times as the range needs to settle: the
    // rows in view and the 600 px kept built past each edge.
    app.resolve();
    let (start, end) = horizon_ui::reactive_graph::traits::GetUntracked::get_untracked(&built);
    assert_eq!(start, 0);
    assert!((22..=24).contains(&end), "{end}");
    let rows = |app: &App| {
        let document = app.document();
        let document = document.borrow();
        // The rows, between the blank above and the blank below (and a
        // marker the list keeps its place by).
        document.get_node(list.0).unwrap().children.len() - 3
    };
    assert_eq!(rows(&app), end);

    // Scrolled far down: the rows built are the ones there, as few.
    list.scroll_to(0., 40_000.);
    app.run_effects();
    app.resolve();
    let (start, end) = horizon_ui::reactive_graph::traits::GetUntracked::get_untracked(&built);
    assert!((984..=986).contains(&start), "{start}");
    assert!((1022..=1024).contains(&end), "{end}");
    assert_eq!(rows(&app), end - start);
}

#[test]
fn a_signal_made_after_a_page_has_gone_is_not_that_pages_to_take_along() {
    use horizon_ui::reactive_graph::effect::Effect;
    use horizon_ui::reactive_graph::traits::GetUntracked;

    // A page that has gone, with something of it still holding its owner
    // (work kept for after a layout, as a list keeps its rows' owner).
    let earlier = App::new(400, 300, "", || {
        let held = horizon_ui::reactive_graph::owner::Owner::current();
        each_layout(move |_| {
            let _ = &held;
        });
        el("div")
    });
    earlier.resolve();
    drop(earlier);
    // A signal made now, for the next page to follow.
    let given = RwSignal::new(0);
    let runs = Rc::new(Cell::new(0));
    let counted = runs.clone();
    let app = App::new(400, 300, "", move || {
        Effect::new(move |_| {
            given.get();
            counted.set(counted.get() + 1);
        });
        el("div")
    });
    app.run_effects();
    assert_eq!(given.try_get_untracked(), Some(0));
    let before = runs.get();
    given.set(1);
    app.run_effects();
    assert_eq!(runs.get(), before + 1);
}
