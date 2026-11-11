//! The layer, without a window: a signal's write reaches the document, an
//! input event reaches a handler, and a keyed list keeps the nodes of the
//! items that stay.

use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set, Update};
use ui_foundation_spike::blitz_dom::NodeId;
use ui_foundation_spike::{App, El, click_at, el};

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

impl ui_foundation_spike::blitz_traits::shell::ShellProvider for Areas {
    fn set_ime_cursor_area(&self, x: f32, y: f32, width: f32, height: f32) {
        self.0.lock().unwrap().push((x, y, width, height));
    }
}

#[test]
fn the_input_methods_area_follows_what_is_typed() {
    use ui_foundation_spike::blitz_traits::events::{BlitzImeEvent, UiEvent};

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
}
