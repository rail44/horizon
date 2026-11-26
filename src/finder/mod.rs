//! A file found by typing: a field, and under it the files of a list
//! whose paths hold what was typed, the best first ([`Source`]). The
//! arrows and Tab move the choice, and Enter takes it. Leaving is whoever
//! shows it's.
//!
//! A row is the file's path as it is written, its directories muted. A
//! path too long for the row loses its start, so the file's name stays.

use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::keys::Key;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, With, WithUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callable, Callback, Component, El, dispatched, each_layout, el, relayout};

use horizon_components::{Field, Prompt, ROW_SELECTED, list_row};

mod listing;
mod source;

pub use source::{Found, Results, Source};

mod styles {
    horizon_ui::css_module! {"
        .finder { width: 560px; max-height: 400px; display: flex; flex-direction: column;
                  overflow: hidden; background: var(--background); }
        .search { flex: none; display: flex; align-items: center; gap: var(--u8);
                  height: var(--u32); padding: 0 var(--u12);
                  border-bottom: var(--line) solid var(--border); }
        .query { flex: 1; font-size: var(--body-size); line-height: calc(var(--u4) * 5); }
        .count { flex: none; font-size: var(--meta-size); line-height: var(--meta-line);
                 color: var(--text-muted); }
        .items { flex: 1; min-height: 0; overflow-y: auto; }
        /* Keep the content inset the same after adding the row border. */
        .item { display: flex; align-items: baseline; height: 34px; box-sizing: border-box;
                padding: calc(7px - var(--line)) calc(var(--u12) - var(--line));
                cursor: pointer; white-space: nowrap;
                overflow: hidden; line-height: calc(var(--u4) * 5); }
        .name { flex: none; }
        .hit { color: var(--accent); font-weight: 600; }
        .cut { display: none; flex: none; margin-right: var(--u2); color: var(--text-muted); }
        .cut.shown { display: block; }
        .directory { flex: 0 1 auto; min-width: 0; overflow: hidden; display: flex;
                     justify-content: flex-end; color: var(--text-muted); }
        .whole { flex: none; }
    "}
}

/// How many of the files found are listed.
const LISTED: usize = 100;

/// A row's height, in CSS pixels (the sheet's).
const ROW: f64 = 34.;

pub struct Finder {
    /// The files to find among.
    pub source: Rc<Source>,
    /// Told the path of the file taken.
    pub picked: Callback<String>,
}

/// `characters` as text, those among `hits` (counted from `first`) marked.
fn marked(characters: &[char], first: usize, hits: &[u32]) -> Vec<El> {
    let hit = |index: usize| hits.binary_search(&((first + index) as u32)).is_ok();
    let mut runs: Vec<(bool, String)> = Vec::new();
    for (index, character) in characters.iter().enumerate() {
        match runs.last_mut() {
            Some((marked, run)) if *marked == hit(index) => run.push(*character),
            _ => runs.push((hit(index), character.to_string())),
        }
    }
    (runs.into_iter())
        .map(|(marked, run)| {
            let run = el("span").text(&run);
            if marked { run.class(styles::HIT) } else { run }
        })
        .collect()
}

/// The row of a file found.
fn row(found: &Found) -> El {
    let characters: Vec<char> = found.path.chars().collect();
    // (Where the name starts: after the last slash.)
    let name = (characters.iter().rposition(|character| *character == '/')).map_or(0, |at| at + 1);
    let whole =
        el("div")
            .class(styles::WHOLE)
            .children(marked(&characters[..name], 0, &found.hits));
    let directory = el("div").class(styles::DIRECTORY).child(whole);
    // Whether the directories do not fit: they are then cut at the start,
    // and the row says so.
    let cut = RwSignal::new(false);
    each_layout(move |page| {
        let (Some((whole, _)), Some((room, _))) = (page.size(whole), page.size(directory)) else {
            return;
        };
        let now = whole > room + 0.5;
        if cut.get_untracked() != now {
            cut.set(now);
            relayout();
        }
    });
    list_row()
        .class(styles::ITEM)
        .child(
            el("div")
                .class(styles::CUT)
                .class_dyn(move || cut.get().then_some(styles::SHOWN))
                .text("\u{2026}"),
        )
        .child(directory)
        .child(el("div").class(styles::NAME).children(marked(
            &characters[name..],
            name,
            &found.hits,
        )))
}

impl Component for Finder {
    fn view(self) -> El {
        let Finder { source, picked } = self;
        let query = RwSignal::new(String::new());
        let chosen = RwSignal::new(0usize);
        let results = RwSignal::new(Results::default());
        // What the source has found is taken in when the page's effects
        // run, which the source wakes the page for; what was typed waits
        // a moment for it, so that the list changes with the field.
        let look = {
            let source = source.clone();
            move |wait: u64| {
                if source.look(wait) {
                    let now = source.results(LISTED);
                    if results.with_untracked(|results| *results != now) {
                        results.set(now);
                    }
                }
            }
        };
        look(10);
        horizon_ui::each_effects({
            let look = look.clone();
            move || look(0)
        });
        let take = move |index: usize| {
            let path = results.with_untracked(|results| {
                (results.found.get(index)).map(|found| found.path.clone())
            });
            if let Some(path) = path {
                picked.run(path);
            }
        };
        let input = el("input")
            .attr("type", "text")
            .on("input", move |event| {
                if let DomEventData::Input(input) = &event.data {
                    query.set(input.value.clone());
                    chosen.set(0);
                    source.type_in(&input.value);
                    look(10);
                }
            })
            .focus();
        let items = el("div").class(styles::ITEMS).each(
            move || results.with(|results| results.found.iter().cloned().enumerate().collect()),
            |(index, found): &(usize, Found)| (*index, found.path.clone(), found.hits.clone()),
            move |(index, found)| {
                let index = *index;
                row(found)
                    .class_dyn(move || (chosen.get() == index).then_some(ROW_SELECTED))
                    .on("click", move |_| take(index))
            },
        );
        // The row chosen with a key is kept in sight.
        let choose = move |index: usize| {
            chosen.set(index);
            dispatched(move |page| {
                let (Some((scrolled, _)), Some((_, height))) =
                    (page.scrolled(items), page.size(items))
                else {
                    return;
                };
                let top = index as f64 * ROW;
                if top < scrolled {
                    items.scroll_to(0., top);
                } else if top + ROW > scrolled + height {
                    items.scroll_to(0., top + ROW - height);
                }
            });
        };
        let count = move || {
            results.with(|results| {
                let more = if results.more { "+" } else { "" };
                format!("{}/{}{more}", results.matched, results.all)
            })
        };
        // The choice, among the files found now.
        let now = move || {
            let count = results.with_untracked(|results| results.found.len());
            chosen.get_untracked().min(count.saturating_sub(1))
        };
        let step = move |by: isize| {
            move || {
                let count = results.with_untracked(|results| results.found.len());
                let to = (now() as isize + by).clamp(0, count.saturating_sub(1) as isize);
                choose(to as usize);
            }
        };
        el("div")
            .class(styles::FINDER)
            // Anything else is the field's; Escape is the sheet's.
            .keys(vec![
                Key::group("↑/↓", "move")
                    .bind("down", step(1))
                    .bind("up", step(-1))
                    .bind("tab", step(1))
                    .bind("shift+tab", step(-1)),
                Key::new("enter", "open", move || take(now())),
                Key::when("esc", "close", || false),
            ])
            .child(
                el("div")
                    .class(styles::SEARCH)
                    .child(
                        Field {
                            input,
                            prompt: Some(Prompt {
                                text: "Find a file...".into(),
                                shown: Signal::derive(move || query.get().is_empty()),
                            }),
                        }
                        .class(styles::QUERY),
                    )
                    .child(el("div").class(styles::COUNT).text_dyn(count)),
            )
            .child(items)
    }
}
