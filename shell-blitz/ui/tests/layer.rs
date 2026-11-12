//! The layer, without a window: a signal's write reaches the document, an
//! input event reaches a handler, and a keyed list keeps the nodes of the
//! items that stay.

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::testing::click_at;
use horizon_ui::{App, El, el};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set, Update};

const CSS: &str = "
    body { margin: 0; font-size: 16px; }
    .column { display: flex; flex-direction: column; gap: 4px; padding: 8px; }
    .button { padding: 4px 8px; background: #356df3; color: white; }
    .row { display: flex; gap: 8px; }
";

/// The text of every text node, in document order.
fn text(app: &App) -> String {
    let document = app.document();
    let document = document.borrow();
    document.root_element().text_content()
}

fn children(app: &App, parent: El) -> Vec<NodeId> {
    let document = app.document();
    let document = document.borrow();
    document.get_node(parent.0).unwrap().children.to_vec()
}

#[test]
fn a_click_writes_a_signal_and_one_text_node_follows() {
    let count = RwSignal::new(0);
    let app = App::new(800, 600, CSS, || {
        el("div").class("column").child(
            el("div")
                .class("button")
                .on("click", move |_| count.update(|count| *count += 1))
                .text_dyn(move || format!("{} clicks", count.get())),
        )
    });
    app.resolve();
    assert_eq!(text(&app), "0 clicks");

    // A real pointer press and release over the button, through the
    // document's own hit testing and dispatch.
    click_at(&app, 20., 20.);
    app.resolve();
    assert_eq!(text(&app), "1 clicks");

    // Outside it, nothing.
    click_at(&app, 20., 400.);
    assert_eq!(text(&app), "1 clicks");
}

#[test]
fn a_keyed_list_keeps_the_nodes_of_the_items_that_stay() {
    let items = RwSignal::new(vec![1u32, 2, 3]);
    let mut list = None;
    let app = App::new(800, 600, CSS, || {
        let column = el("div")
            .class("column")
            .child(el("div").text("before"))
            .each(
                move || items.get(),
                |item| *item,
                |item| el("div").class("row").text(&format!("item {item}")),
            )
            .child(el("div").text("after"));
        list = Some(column);
        column
    });
    let list = list.unwrap();
    app.resolve();
    assert_eq!(text(&app), "beforeitem 1item 2item 3after");
    // The list's own nodes: between what is before it and its marker.
    let rows = |app: &App| children(app, list)[1..4].to_vec();
    let before = rows(&app);

    // One taken out of the middle, one added at the end.
    items.set(vec![1, 3, 4]);
    app.run_effects();
    app.resolve();
    assert_eq!(text(&app), "beforeitem 1item 3item 4after");
    let after = rows(&app);
    assert_eq!(after[..2], [before[0], before[2]]);
    assert!(!before.contains(&after[2]));

    // Put in another order: the same nodes, moved.
    items.set(vec![4, 1, 3]);
    app.run_effects();
    app.resolve();
    assert_eq!(text(&app), "beforeitem 4item 1item 3after");
    assert_eq!(rows(&app), [after[2], after[0], after[1]]);
}

/// A shell that only remembers where it was told the edited text is.
#[derive(Default)]
struct Areas(std::sync::Mutex<Vec<(f32, f32, f32, f32)>>);

impl horizon_ui::blitz_traits::shell::ShellProvider for Areas {
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.0.lock().unwrap().push((x, y, width, height));
    }
}

#[test]
fn the_input_methods_area_follows_what_is_typed() {
    use horizon_ui::blitz_traits::events::{BlitzImeEvent, UiEvent};

    let app = App::new(800, 600, CSS, || {
        el("div")
            .class("column")
            .child(el("input").attr("type", "text"))
    });
    let areas = std::sync::Arc::new(Areas::default());
    (app.document().borrow_mut()).set_shell_provider(areas.clone());
    app.resolve();

    // Focus the input, then commit text twice, as an input method does.
    click_at(&app, 40., 20.);
    app.resolve();
    let last = |areas: &Areas| *areas.0.lock().unwrap().last().expect("an area");
    let focused = last(&areas);
    app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit("日本語".to_owned())));
    app.resolve();
    let once = last(&areas);
    app.dispatch(UiEvent::Ime(BlitzImeEvent::Commit("入力".to_owned())));
    app.resolve();
    let twice = last(&areas);

    // The caret moved right each time, on the same line.
    assert!(once.0 > focused.0, "{focused:?} then {once:?}");
    assert!(twice.0 > once.0, "{once:?} then {twice:?}");
    assert_eq!(once.1, twice.1);

    // And it is inside the input's own box on the page.
    let document = app.document();
    let document = document.borrow();
    let input = document
        .find_element_by_tag_name(&horizon_ui::blitz_dom::LocalName::from("input"))
        .expect("the input");
    let bounds = document.get_client_bounding_rect(input.id).expect("a box");
    let (x, y, _, height) = twice;
    // The line of text is a little taller than the box this style sheet
    // gives the input, so the area may end just below it.
    let top_left_inside = (bounds.x..bounds.x + bounds.width).contains(&(x as f64))
        && (bounds.y..bounds.y + bounds.height).contains(&(y as f64));
    let ends_near = (y + height) as f64 <= bounds.y + bounds.height + 4.;
    assert!(
        top_left_inside && ends_near,
        "{twice:?} in {:?}",
        (bounds.x, bounds.y, bounds.width, bounds.height)
    );
}

#[test]
fn thin_borders_survive_a_fractional_display_scale() {
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};

    const BORDERED: &str = "
        body { margin: 0; font-size: 15px; }
        .column { display: flex; flex-direction: column; gap: 4px; padding: 16px; }
        .row { padding: 4px 8px; border: 1px solid #d0d0d0; }
    ";
    let mut rows = Vec::new();
    let app = App::new(1800, 1350, BORDERED, || {
        let mut column = el("div").class("column");
        for row in 0..12 {
            let element = el("div").class("row").text(&format!("row {row}"));
            rows.push(element);
            column = column.child(element);
        }
        column
    });
    // 150 %, as a desktop set to it reports.
    (app.document().borrow_mut()).set_viewport(Viewport::new(1800, 1350, 1.5, ColorScheme::Light));
    app.resolve();

    let document = app.document();
    let document = document.borrow();
    for (index, row) in rows.iter().enumerate() {
        let border = document.get_node(row.0).unwrap().final_layout().border;
        let sides = [border.top, border.right, border.bottom, border.left];
        assert!(
            sides.iter().all(|side| *side > 0.),
            "row {index} has a side without a border: {sides:?}"
        );
    }
}

#[test]
fn rows_scrolled_out_of_a_list_do_not_take_the_clicks_above_it() {
    use horizon_ui::blitz_dom::ScrollBehavior;

    const SCROLLING: &str = "
        body { margin: 0; }
        .bar { height: 40px; }
        .list { height: 100px; overflow-y: scroll; }
        .row { height: 20px; }
    ";
    let (bar, row) = (RwSignal::new(0u32), RwSignal::new(0u32));
    let mut list = None;
    let app = App::new(400, 300, SCROLLING, || {
        let rows = el("div").class("list").each(
            || (0..100u32).collect::<Vec<_>>(),
            |index| *index,
            move |_| {
                el("div")
                    .class("row")
                    .on("click", move |_| row.update(|count| *count += 1))
            },
        );
        list = Some(rows);
        el("div")
            .child(
                el("div")
                    .class("bar")
                    .on("click", move |_| bar.update(|count| *count += 1)),
            )
            .child(rows)
    });
    app.resolve();
    (app.document().borrow_mut()).scroll_to(list.unwrap().0, 0., 1000., ScrollBehavior::Instant);
    app.resolve();

    // Over the bar: the rows scrolled up behind it are clipped away.
    click_at(&app, 100., 20.);
    assert_eq!((bar.get(), row.get()), (1, 0));
    // Inside the list: a row.
    click_at(&app, 100., 90.);
    assert_eq!((bar.get(), row.get()), (1, 1));
}
