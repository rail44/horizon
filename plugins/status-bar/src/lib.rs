//! The status bar, an extension (board #84): the keys that work where the
//! keys are, each with what it does, at the bottom of the window, where
//! the shell shows the extension named `status-bar`.
//!
//! What it says is what the elements the focus is in keep of their keys
//! (`horizon:ui/keys`; `horizon_ui::keys::said`): the view's with the
//! focus, and the shell's own around it -- workspace mode's while it is
//! on, the chords the configuration binds otherwise. Each key is in a box
//! of its own, its words after it; the bar is one line, and what does not
//! fit is left out whole from the end. It is not pressed: what is pressed
//! is the keys.
#![cfg(target_family = "wasm")]

use horizon_ui::guest::bindings::horizon::ui::keys as host;
use horizon_ui::keys::{Kept, said};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set, WithUntracked};
use horizon_ui::{El, each_layout, el};

mod styles {
    horizon_ui::css_module! {"
    /* One line high: a line that wraps goes under its padding and out of
       sight, the hints that do not fit with it. A key's box has its edge
       drawn by a shadow, which takes no room, so the line stays a line
       high. */
    .status-bar { display: flex; flex-wrap: wrap; align-items: center;
                  gap: var(--u8) var(--u12); height: var(--micro-line); overflow: hidden;
                  padding: var(--u4) var(--u8); white-space: nowrap;
                  font-size: var(--micro-size); line-height: var(--micro-line);
                  color: var(--text-muted); background: var(--background);
                  border-top: var(--line) solid var(--border); }
    .status-hint { display: flex; align-items: center; gap: var(--u4); }
    /* As faint as a pane's tag: its words, and a ground a step off the
       bar's. */
    .status-keys { padding: 0 var(--u4); border-radius: var(--radius);
                   color: var(--text-muted); background: var(--chip-muted-surface);
                   box-shadow: inset 0 0 0 var(--line) var(--surface-chrome); }
    "}
}

#[horizon_ui::plugin(name = "status-bar", title = "Status Bar", uses = ["keys"])]
mod status_bar {
    #[status_bar]
    pub fn view() -> u64 {
        horizon_ui::guest::mount(|| Some(crate::bar())).unwrap_or_default()
    }
}

/// The bar, saying the keys of where the keys are after each layout: the
/// focus moves with no signal of its own to follow.
fn bar() -> El {
    let shown = RwSignal::new(Vec::<Kept>::new());
    each_layout(move |_| {
        let focused: Vec<(String, bool)> = (host::focused().into_iter())
            .map(|kept| (kept.keys, kept.alone))
            .collect();
        let now = said(&focused, host::typing());
        if shown.with_untracked(|was| *was != now) {
            shown.set(now);
        }
    });
    el("div").class(styles::STATUS_BAR).each(
        move || shown.get().into_iter().enumerate().collect::<Vec<_>>(),
        |(index, kept)| (*index, kept.clone()),
        |(_, kept)| {
            el("span")
                .class(styles::STATUS_HINT)
                .child(el("span").class(styles::STATUS_KEYS).text(&kept.spelled))
                .child(el("span").text(&kept.label))
        },
    )
}
