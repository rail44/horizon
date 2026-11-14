//! A file found by typing: a field, and under it the files of a list
//! whose paths hold what was typed, the best first ([`matching`]). The
//! arrows and Tab move the choice, Enter takes it, Escape leaves.

use std::cell::Cell;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::computed::Memo;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, With, WithUntracked};
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Callable, Callback, Component, El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use crate::parts::{Field, Prompt};

mod matching;

pub use matching::{Found, Results, find};

mod styles {
    horizon_ui::css_module! {"
        .finder { width: 560px; max-height: 400px; display: flex; flex-direction: column;
                  overflow: hidden; background: var(--background);
                  border: var(--line) solid var(--border); border-radius: 8px;
                  box-shadow: var(--shadow-modal); }
        .search { flex: none; display: flex; align-items: center; gap: var(--u8);
                  height: 32px; padding: 0 var(--u12);
                  border-bottom: var(--line) solid var(--border); }
        .query { flex: 1; font-size: var(--body-size); line-height: calc(var(--u4) * 5); }
        .count { flex: none; font-size: var(--meta-size); line-height: var(--meta-line);
                 color: var(--text-muted); }
        .items { flex: 1; min-height: 0; overflow-y: auto; }
        .item { display: flex; align-items: baseline; height: 34px; box-sizing: border-box;
                padding: 7px var(--u12); cursor: pointer; white-space: nowrap;
                overflow: hidden; line-height: calc(var(--u4) * 5); }
        .item.chosen { background: var(--surface-selected); }
        .name { flex: none; }
        .hit { color: var(--accent); font-weight: 600; }
        .directory { min-width: 0; overflow: hidden; color: var(--text-muted); }
        .item.path .directory { flex: 0 1 auto; display: flex; justify-content: flex-end; }
        .item.path .directory > .whole { flex: none; }
        .item.named { gap: var(--u8); }
        .item.named .directory { flex: 1; text-overflow: ellipsis;
                                 font-size: var(--meta-size); }
    "}
}

/// How many of the files found are listed.
const LISTED: usize = 100;

/// A row's height and the list's, in CSS pixels (the sheet's).
const ROW: f64 = 34.;
const LIST: f64 = 366.;

/// How a file's row is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The path as it is written, its directories muted; a path too long
    /// loses its start.
    Path,
    /// The file's name, then its directory, smaller and muted; a
    /// directory too long loses its end.
    NameFirst,
}

pub struct Finder {
    /// The files to find among, as paths from where they were listed.
    pub files: Signal<Vec<String>>,
    /// Whether more files are still being listed.
    pub listing: Signal<bool>,
    pub layout: Layout,
    /// Told the path of the file taken.
    pub picked: Callback<String>,
    /// Told when Escape leaves.
    pub left: Callback<()>,
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
fn row(found: &Found, layout: Layout) -> El {
    let characters: Vec<char> = found.path.chars().collect();
    // (Where the name starts: after the last slash.)
    let name = (characters.iter().rposition(|character| *character == '/')).map_or(0, |at| at + 1);
    let named =
        el("div")
            .class(styles::NAME)
            .children(marked(&characters[name..], name, &found.hits));
    match layout {
        Layout::Path => {
            let whole = el("div").class(styles::WHOLE).children(marked(
                &characters[..name],
                0,
                &found.hits,
            ));
            el("div")
                .class((styles::ITEM, styles::PATH))
                .child(el("div").class(styles::DIRECTORY).child(whole))
                .child(named)
        }
        Layout::NameFirst => {
            let directory = &characters[..name.saturating_sub(1)];
            el("div")
                .class((styles::ITEM, styles::NAMED))
                .child(named)
                .child(el("div").class(styles::DIRECTORY).children(marked(
                    directory,
                    0,
                    &found.hits,
                )))
        }
    }
}

impl Component for Finder {
    fn view(self) -> El {
        let Finder {
            files,
            listing,
            layout,
            picked,
            left,
        } = self;
        let query = RwSignal::new(String::new());
        let chosen = RwSignal::new(0usize);
        let results = Memo::new(move |_| files.with(|files| find(files, &query.get(), LISTED)));
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
                }
            })
            .focus();
        let items = el("div").class(styles::ITEMS).each(
            move || results.with(|results| results.found.iter().cloned().enumerate().collect()),
            |(index, found): &(usize, Found)| (*index, found.clone().path, found.hits.clone()),
            move |(index, found)| {
                let index = *index;
                row(found, layout)
                    .class_dyn(move || (chosen.get() == index).then_some(styles::CHOSEN))
                    .on("click", move |_| take(index))
            },
        );
        // The chosen row is kept in sight.
        let scrolled = Rc::new(Cell::new(0f64));
        Effect::new(move |_| {
            let top = chosen.get() as f64 * ROW;
            let now = scrolled.get();
            let to = if top < now {
                top
            } else if top + ROW > now + LIST {
                top + ROW - LIST
            } else {
                return;
            };
            scrolled.set(to);
            items.scroll_to(0., to);
        });
        let count = move || {
            let matched = results.with(|results| results.matched);
            let all = files.with(Vec::len);
            let more = if listing.get() { "+" } else { "" };
            format!("{matched}/{all}{more}")
        };
        el("div")
            .class(styles::FINDER)
            .on("pointerdown", |_| stop_propagation())
            .on("keydown", move |event| {
                let DomEventData::KeyDown(key) = &event.data else {
                    return;
                };
                let count = results.with_untracked(|results| results.found.len());
                let last = count.saturating_sub(1);
                let now = chosen.get_untracked().min(last);
                let (down, up) = ((now + 1).min(last), now.saturating_sub(1));
                match &key.key {
                    Key::Named(NamedKey::ArrowDown) => chosen.set(down),
                    Key::Named(NamedKey::ArrowUp) => chosen.set(up),
                    Key::Named(NamedKey::Tab) if key.modifiers.shift() => chosen.set(up),
                    Key::Named(NamedKey::Tab) => chosen.set(down),
                    Key::Named(NamedKey::Escape) => left.run(()),
                    Key::Named(NamedKey::Enter) => take(now),
                    // Anything else is the field's.
                    _ => return,
                }
                prevent_default();
                stop_propagation();
            })
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
