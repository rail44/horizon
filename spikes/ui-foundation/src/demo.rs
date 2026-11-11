//! The view the window example shows, apart from the window: what is seen
//! there can then be looked into without one.

use blitz_traits::events::DomEventData;
use reactive_graph::signal::RwSignal;
use reactive_graph::traits::{Get, Set, Update};

use crate::{El, el};

pub const CSS: &str = "
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

pub fn view() -> El {
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
