//! The views, by name.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update};
use horizon_ui::{El, dispatched, each_layout, el, relayout, stop_propagation};
use keyboard_types::Key;

pub const NAMES: [&str; 5] = ["rows", "selection", "over", "stuck", "sessions"];

pub fn view(name: &str) -> Option<El> {
    match name {
        "rows" => Some(rows()),
        "selection" => Some(selection()),
        "over" => Some(over()),
        // The sessions, which the manifest does not say it uses.
        "sessions" => {
            let sessions = horizon_ui::guest::bindings::horizon::ui::sessions::all();
            Some(el("div").text(&format!("{} sessions", sessions.len())))
        }
        // Never comes back: what the host's limit on a call is for.
        "stuck" => {
            let mut turns = 0u64;
            loop {
                turns = std::hint::black_box(turns.wrapping_add(1));
            }
        }
        _ => None,
    }
}

mod styles {
    horizon_ui::css_module! {"
        .rows { display: flex; flex-direction: column; height: 100%; outline: none;
            .row { display: flex; padding: var(--u4) var(--u8);
                .title { flex: 1; }
                .tail { flex: none; color: var(--text-muted); }
            }
            .row.selected { background: var(--selection); }
            .row.carried { opacity: 0.5; }
            .row.under { background: var(--surface-raised); }
        }
    "}
}

/// Three rows: the keys and a click move which is selected, and a row
/// pressed and carried marks the row the pointer is over, which it asks
/// the page for. Its sheet comes with it: a row is one line, its tail at
/// the far end from its title.
fn rows() -> El {
    let selected = RwSignal::new(0usize);
    let carried = RwSignal::new(None::<usize>);
    let under = RwSignal::new(None::<usize>);
    let root = el("div").class(styles::ROWS).attr("tabindex", "0");
    let mut elements = Vec::new();
    for (index, title) in ["first", "second", "third"].into_iter().enumerate() {
        let row = el("div")
            .class(styles::ROW)
            .class_dyn(move || {
                (
                    (selected.get() == index).then_some(styles::SELECTED),
                    (carried.get() == Some(index)).then_some(styles::CARRIED),
                    (under.get() == Some(index)).then_some(styles::UNDER),
                )
            })
            .child(el("span").class(styles::TITLE).text(title))
            .child(el("span").class(styles::TAIL).text("tail"))
            .on("click", move |_| selected.set(index))
            .on("pointerdown", move |_| carried.set(Some(index)));
        elements.push(row);
        root.child(row);
    }
    let last = elements.len() - 1;
    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        match &key.key {
            Key::Character(character) if character == "j" => {
                selected.update(|selected| *selected = (*selected + 1).min(last));
            }
            Key::Character(character) if character == "k" => {
                selected.update(|selected| *selected = selected.saturating_sub(1));
            }
            _ => {}
        }
    })
    .on("pointermove", move |event| {
        let DomEventData::PointerMove(pointer) = &event.data else {
            return;
        };
        if carried.get_untracked().is_none() {
            return;
        }
        let (x, y) = (pointer.page_x(), pointer.page_y());
        let elements = elements.clone();
        // What is under the pointer, asked of the page once the event is
        // done with.
        dispatched(move |page| {
            let hit = page.under(x, y);
            under.set(elements.iter().position(|row| hit.contains(row)));
        });
    })
    .on("pointerup", move |_| {
        carried.set(None);
        under.set(None);
    })
}

/// Words, and what is selected of them said back: what a guest hears of
/// the page's selection.
fn selection() -> El {
    let said = RwSignal::new(String::new());
    each_layout(move |page| {
        let selected: String = (page.selection().into_iter())
            .filter_map(|(element, start, end)| {
                let text = page.inline_text(element)?;
                text.get(start..end).map(str::to_string)
            })
            .collect();
        if selected != said.get_untracked() {
            said.set(selected);
            relayout();
        }
    });
    let root = el("div");
    root.child(el("p").class("words").text("Some words to select"))
        .child(el("p").class("said").text_dyn(move || said.get()))
}

thread_local! {
    /// How many times a view put over the workspace was let go of, as the
    /// pane's view says it.
    static TAKEN: std::cell::Cell<Option<RwSignal<usize>>> = const { std::cell::Cell::new(None) };
}

/// Says how many times the view its pane's command puts over the
/// workspace ([`show_over`]) has been taken away.
fn over() -> El {
    let taken = RwSignal::new(0usize);
    TAKEN.set(Some(taken));
    el("div")
        .attr("tabindex", "0")
        .text_dyn(move || match taken.get() {
            0 => "nothing over".to_string(),
            taken => format!("taken away {taken}"),
        })
        .focus()
}

/// Puts a view over the workspace, which `c` takes away; let go of, it is
/// counted.
pub fn show_over() {
    let taken = TAKEN.get();
    horizon_ui::guest::modal(move |close| {
        if let Some(taken) = taken {
            horizon_ui::reactive_graph::owner::on_cleanup(move || {
                taken.update(|taken| *taken += 1)
            });
        }
        el("div")
            .attr("tabindex", "0")
            .text("over the workspace")
            .on("keydown", move |event| {
                if let DomEventData::KeyDown(key) = &event.data
                    && key.key == Key::Character("c".into())
                {
                    close.close();
                    stop_propagation();
                }
            })
            .focus()
    });
}
