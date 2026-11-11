//! Opens a window on the same layer, to try by hand what a test cannot:
//! typing, the input method, hover, scrolling.
//!
//!     cargo run --release --bin window

use anyrender_vello::VelloWindowRenderer;
use blitz_shell::{BlitzApplication, BlitzShellProxy, WindowConfig, create_default_event_loop};
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set, Update};
use ui_foundation_spike::blitz_dom::{DocGuard, DocGuardMut, Document};
use ui_foundation_spike::blitz_traits::events::{DomEventData, UiEvent};
use ui_foundation_spike::{App, el};

const CSS: &str = "
    html { background: #fafafa; }
    body { margin: 0; font-size: 15px; color: #202020; font-family: sans-serif; }
    .column { display: flex; flex-direction: column; gap: 10px; padding: 16px; }
    .title { font-size: 22px; font-weight: 600; }
    .button { align-self: flex-start; padding: 6px 14px; border-radius: 6px;
              background: #356df3; color: white; cursor: pointer; }
    .button:hover { background: #477df8; }
    .row { display: flex; gap: 8px; align-items: center; padding: 4px 8px;
           border: 1px solid #d0d0d0; border-radius: 4px; }
    .row:hover { background: #f0f0f0; }
    .label { flex: 1; }
    .muted { color: #808080; }
    .list { display: flex; flex-direction: column; gap: 4px; height: 220px;
            overflow-y: scroll; }
    input, textarea { font-size: 15px; padding: 6px 8px; border: 1px solid #b0b0b0;
                      border-radius: 4px; line-height: 1.5; }
    textarea { height: 80px; }
";

/// The layer's document as the shell's: input goes through the layer's
/// handlers, and a turn of the shell runs the signals' effects.
struct Live(App);

impl Document for Live {
    fn inner(&self) -> DocGuard<'_> {
        DocGuard::RefCell(self.0.document_cell().borrow())
    }

    fn inner_mut(&mut self) -> DocGuardMut<'_> {
        DocGuardMut::RefCell(self.0.document_cell().borrow_mut())
    }

    fn handle_ui_event(&mut self, event: UiEvent) {
        self.0.dispatch(event);
    }
}

fn view() -> ui_foundation_spike::El {
    let count = RwSignal::new(3u32);
    let typed = RwSignal::new(String::new());
    el("div")
        .class("column")
        .child(el("div").class("title").text("signal から blitz-dom へ"))
        .child(
            el("div")
                .class("button")
                .on("click", move |_| count.update(|count| *count += 1))
                .text_dyn(move || format!("{} 行 (クリックで追加)", count.get())),
        )
        .child(
            el("input")
                .attr("type", "text")
                .attr("placeholder", "ここに入力 (日本語入力も)")
                .on("input", move |event| {
                    if let DomEventData::Input(input) = &event.data {
                        typed.set(input.value.clone());
                    }
                }),
        )
        .child(
            el("div")
                .class("muted")
                .text_dyn(move || format!("入力された文字: {}", typed.get())),
        )
        .child(el("textarea").attr("placeholder", "複数行の入力"))
        .child(el("div").class("list").each(
            move || (0..count.get()).collect::<Vec<u32>>(),
            |row| *row,
            |row| {
                el("div")
                    .class("row")
                    .child(el("span").class("label").text(&format!("行 {row}")))
                    .child(el("span").class("muted").text("remove"))
            },
        ))
}

fn main() {
    let event_loop = create_default_event_loop();
    let (proxy, events) = BlitzShellProxy::new(event_loop.create_proxy());
    let mut application = BlitzApplication::new(proxy, events);
    let app = App::new(900, 640, CSS, view);
    application.add_window(WindowConfig::new(
        Box::new(Live(app)),
        VelloWindowRenderer::new(),
    ));
    event_loop.run_app(application).expect("the event loop");
}
