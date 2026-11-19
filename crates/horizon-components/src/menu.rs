//! A menu button: a button that opens a list of things to do, each with
//! the key that does it where there is one, so what can be done is on the
//! page rather than known.
//!
//! The button opens the list on a click, or on Enter, Space or the down
//! arrow; open, the arrows, Home and End move along it, Enter or Space
//! does the item the keys are on, and Escape shuts it. Another key shuts
//! it and goes on to the view, so an item's own key does what it says.
//! What placed it may open it too -- on a right-click, say -- and the
//! keys come to it as it opens.

use std::cell::Cell;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{
    Callable, Callback, Component, El, each_layout, el, prevent_default, relayout, stop_propagation,
};
use keyboard_types::{Key, NamedKey};

mod styles {
    horizon_ui::css_module! {"
    .menu { position: relative; flex: none; align-self: flex-start; outline: none;
            font-size: var(--meta-size);
            line-height: var(--meta-line);
        .trigger { padding: 0 var(--u4); border-radius: var(--radius); color: var(--text-muted);
                   cursor: pointer; }
        .trigger:hover { background: var(--hover); color: var(--text); }
        .items { position: absolute; z-index: 60; right: 0; top: 100%; margin-top: var(--u4);
                 padding: var(--u4); border-radius: var(--radius-large);
                 border: var(--line) solid var(--border); background: var(--background);
                 box-shadow: var(--shadow-popup); }
        .items.above { top: auto; bottom: 100%; margin-top: 0; margin-bottom: var(--u4); }
        .item { display: flex; align-items: baseline; justify-content: space-between;
                gap: var(--u24); padding: var(--u4) var(--u8); border-radius: var(--radius);
                white-space: nowrap; color: var(--text); cursor: pointer; }
        .item.active { background: var(--surface-selected); }
        .item.danger { color: var(--danger); }
        .key { color: var(--text-muted); }
    }
    .menu.open .trigger { background: var(--hover); color: var(--text); }
"}
}

/// One thing a menu does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    /// The key that does the same where the menu is, shown beside it.
    pub key: Option<String>,
    /// Whether it ends or takes away something: said in the danger color.
    pub danger: bool,
}

/// A button saying `label` that opens a list of `items`.
pub struct MenuButton<'a> {
    /// What the button says: a word, or `⋯` for what more there is.
    pub label: &'a str,
    pub items: Vec<MenuItem>,
    /// Whether the list is open. Set to open it from elsewhere; the keys
    /// come to it as it opens.
    pub open: RwSignal<bool>,
    /// Told of each item chosen, by its place among the items.
    pub on_select: Callback<usize>,
    /// What the list stays inside, where the button is in a scrolled
    /// element: the list opens upward where it would pass that element's
    /// end below. With none it always opens downward.
    pub within: Option<El>,
}

impl Component for MenuButton<'_> {
    fn view(self) -> El {
        let MenuButton {
            label,
            items,
            open,
            on_select,
            within,
        } = self;
        let count = items.len();
        // The item the keyboard is on while the list is open.
        let active = RwSignal::new(0usize);
        let above = RwSignal::new(false);
        let listed: Rc<Cell<Option<El>>> = Rc::default();
        // Whether it has opened and the keys it was given have not come to
        // it yet: they come only before the page is painted, after however
        // many layouts the frame has.
        let opening = Rc::new(Cell::new(false));
        let opened = opening.clone();
        let root = el("div")
            .class(styles::MENU)
            .class_dyn(move || open.get().then_some(styles::OPEN))
            .attr("tabindex", "0");
        Effect::new(move |_| {
            if open.get() {
                active.set(0);
                above.set(false);
                opened.set(true);
                root.focus();
            }
        });
        let choose = move |index: usize| {
            open.set(false);
            on_select.run(index);
        };
        // The list shuts when the keys go elsewhere: a press outside it.
        // Open, it turns upward where it would pass the end of what it is
        // in.
        let measured = listed.clone();
        each_layout(move |page| {
            if !open.get_untracked() {
                return;
            }
            let focused = page.has_focus_within(root);
            if focused {
                opening.set(false);
            }
            if !focused && !opening.get() {
                open.set(false);
                relayout();
                return;
            }
            let bounds = (measured.get().and_then(|list| page.bounds(list)))
                .zip(within.and_then(|within| page.bounds(within)));
            if let Some((list, within)) = bounds
                && !above.get_untracked()
                && list.y + list.height > within.y + within.height
            {
                above.set(true);
                relayout();
            }
        });
        let chosen_by_key = choose;
        root.on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            let space = matches!(&key.key, Key::Character(character) if character == " ");
            let at = active.get_untracked();
            match (&key.key, open.get_untracked()) {
                (Key::Named(NamedKey::Enter | NamedKey::ArrowDown), false) => open.set(true),
                (_, false) if space => open.set(true),
                (Key::Named(NamedKey::ArrowDown), true) => active.set((at + 1).min(count - 1)),
                (Key::Named(NamedKey::ArrowUp), true) => active.set(at.saturating_sub(1)),
                (Key::Named(NamedKey::Home), true) => active.set(0),
                (Key::Named(NamedKey::End), true) => active.set(count - 1),
                (Key::Named(NamedKey::Enter), true) => chosen_by_key(at),
                (_, true) if space => chosen_by_key(at),
                (Key::Named(NamedKey::Escape), true) => open.set(false),
                // (Another key shuts it, and is the view's.)
                (_, true) => {
                    open.set(false);
                    return;
                }
                _ => return,
            }
            prevent_default();
            // (An Escape that shut the list is not the view's to act on.)
            stop_propagation();
        })
        .child(
            el("div")
                .class(styles::TRIGGER)
                .text(label)
                // (A press on the button is not one on what it is in.)
                .on("click", move |_| {
                    open.set(!open.get_untracked());
                    stop_propagation();
                }),
        )
        .when(
            move || open.get(),
            move || {
                let list = el("div")
                    .class(styles::ITEMS)
                    .class_dyn(move || above.get().then_some(styles::ABOVE))
                    .children((items.iter().enumerate()).map(|(index, item)| {
                        el("div")
                            .class((styles::ITEM, item.danger.then_some(styles::DANGER)))
                            .class_dyn(move || (active.get() == index).then_some(styles::ACTIVE))
                            .child(el("span").text(&item.label))
                            .child(
                                el("span")
                                    .class(styles::KEY)
                                    .text(item.key.as_deref().unwrap_or_default()),
                            )
                            .on("pointermove", move |_| active.set(index))
                            .on("click", move |_| {
                                choose(index);
                                stop_propagation();
                            })
                    }));
                listed.set(Some(list));
                list
            },
        )
    }
}
