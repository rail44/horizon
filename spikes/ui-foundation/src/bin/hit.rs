//! Whether a style sheet can keep a scrolled list's hidden rows from
//! taking the clicks meant for what is drawn above and below the list.
//!
//!     cargo run --release --bin hit

use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Update};
use ui_foundation_spike::blitz_dom::ScrollBehavior;
use ui_foundation_spike::{App, El, click_at, el};

const CSS: &str = "
    body { margin: 0; font-size: 15px; }
    .bar { height: 40px; }
    .list { height: 100px; overflow-y: scroll; }
    .row { height: 20px; }
    .foot { height: 40px; }
";

/// Clicks the bar above the list and the foot below it, the list scrolled
/// by `scroll`, and says who heard each click.
fn try_with(name: &str, extra: &str, scroll: f64) {
    let (bar, foot, row) = (
        RwSignal::new(0u32),
        RwSignal::new(0u32),
        RwSignal::new(0u32),
    );
    let mut list: Option<El> = None;
    let css = format!("{CSS}{extra}");
    let app = App::new(400, 300, &css, || {
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
            .child(
                el("div")
                    .class("foot")
                    .on("click", move |_| foot.update(|count| *count += 1)),
            )
    });
    app.resolve();
    (app.document().borrow_mut()).scroll_to(list.unwrap().0, 0., scroll, ScrollBehavior::Instant);
    app.resolve();
    // The bar is at 0..40, the list at 40..140, the foot at 140..180.
    click_at(&app, 100., 20.);
    let above = (bar.get(), row.get());
    click_at(&app, 100., 160.);
    let below = (foot.get(), row.get() - above.1);
    click_at(&app, 100., 90.);
    let inside = row.get() - above.1 - below.1;
    let who = |own: u32, rows: u32| match (own, rows) {
        (1, 0) => "itself",
        (0, 1) => "A HIDDEN ROW",
        _ => "nobody",
    };
    println!(
        "{name:44} scroll {scroll:5}: click on the bar -> {:12} | on the foot -> {:12} | in the list -> {}",
        who(above.0, above.1),
        who(below.0, below.1),
        if inside == 1 { "a row" } else { "nobody" }
    );
}

fn main() {
    for scroll in [0., 1000.] {
        try_with("as it is", "", scroll);
        try_with(
            "bar and foot raised (relative, z-index 1)",
            ".bar, .foot { position: relative; z-index: 1; }",
            scroll,
        );
        try_with(
            "list its own stacking context (isolate)",
            ".list { isolation: isolate; }",
            scroll,
        );
        try_with(
            "list positioned (relative, z-index 0)",
            ".list { position: relative; z-index: 0; }",
            scroll,
        );
        try_with("list contain: paint", ".list { contain: paint; }", scroll);
    }
}
