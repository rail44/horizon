//! A select: one of a few named options, shown on a button that opens
//! the list of them.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{El, each_layout, el, prevent_default, relayout, stop_propagation};
use keyboard_types::{Key, NamedKey};

pub(super) const CSS: &str = "
    .select { position: relative; outline: none; font-size: var(--meta-size);
              line-height: var(--meta-line);
        .trigger { display: flex; align-items: center; justify-content: space-between;
                   gap: var(--u8); padding: var(--u4) var(--u8); border-radius: 4px;
                   border: 1px solid var(--border); background: var(--background);
                   cursor: pointer; }
        .chevron { color: var(--text-muted); }
        .options { position: absolute; z-index: 60; left: 0; right: 0; top: 100%;
                   margin-top: 4px; padding: var(--u4); border-radius: 6px;
                   border: 1px solid var(--border); background: var(--background);
                   box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15); }
        .option { padding: var(--u4) var(--u8); border-radius: 4px; cursor: pointer; }
        .option.active { background: var(--surface-selected); }
        .option.chosen { font-weight: 600; }
    }
    .select:focus .trigger { border-color: var(--accent); }
";

/// Builds a select over `chosen`, the place of the chosen one among
/// `options`. `changed` hears each option a person chose.
pub fn select(
    options: Vec<String>,
    chosen: RwSignal<usize>,
    changed: impl Fn(usize) + Clone + 'static,
) -> El {
    let open = RwSignal::new(false);
    // The option the keyboard is on while the list is open.
    let active = RwSignal::new(0usize);
    let count = options.len();
    let root = el("div").class("select").attr("tabindex", "0");
    let choose = move |index: usize| {
        open.set(false);
        if index != chosen.get_untracked() {
            chosen.set(index);
            changed(index);
        }
    };
    let shown = options.clone();
    let listed = options.clone();
    let (chosen_by_key, chosen_by_click) = (choose.clone(), choose);
    // The list shuts when the keys go elsewhere: a press outside it.
    each_layout(move |page| {
        if open.get_untracked() && !page.has_focus_within(root) {
            open.set(false);
            relayout();
        }
    });
    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        let space = matches!(&key.key, Key::Character(character) if character == " ");
        let at = active.get_untracked();
        match (&key.key, open.get_untracked()) {
            (Key::Named(NamedKey::Enter | NamedKey::ArrowDown), false) => {
                active.set(chosen.get_untracked());
                open.set(true);
            }
            (_, false) if space => {
                active.set(chosen.get_untracked());
                open.set(true);
            }
            (Key::Named(NamedKey::ArrowDown), true) => active.set((at + 1).min(count - 1)),
            (Key::Named(NamedKey::ArrowUp), true) => active.set(at.saturating_sub(1)),
            (Key::Named(NamedKey::Home), true) => active.set(0),
            (Key::Named(NamedKey::End), true) => active.set(count - 1),
            (Key::Named(NamedKey::Enter), true) => chosen_by_key(at),
            (_, true) if space => chosen_by_key(at),
            (Key::Named(NamedKey::Escape), true) => open.set(false),
            _ => return,
        }
        prevent_default();
        // (An Escape that shut the list is not the view's to act on.)
        stop_propagation();
    })
    .child(
        el("div")
            .class("trigger")
            .child(
                el("span").text_dyn(move || shown.get(chosen.get()).cloned().unwrap_or_default()),
            )
            .child(el("span").class("chevron").text("▾"))
            .on("click", move |_| {
                active.set(chosen.get_untracked());
                open.set(!open.get_untracked());
            }),
    )
    .when(
        move || open.get(),
        move || {
            let list = el("div").class("options");
            for (index, option) in listed.iter().enumerate() {
                let choose = chosen_by_click.clone();
                list.child(
                    el("div")
                        .class_dyn(move || {
                            let mut class = "option".to_string();
                            if active.get() == index {
                                class += " active";
                            }
                            if chosen.get() == index {
                                class += " chosen";
                            }
                            class
                        })
                        .text(option)
                        .on("click", move |_| choose(index)),
                );
            }
            list
        },
    )
}
