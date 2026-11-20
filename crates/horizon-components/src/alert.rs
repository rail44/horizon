//! An alert dialog: a question to be answered before something that
//! cannot be taken back is done -- what it is, what it does, and two
//! buttons, one to do it and one to keep things as they are.
//!
//! Its keys follow Radix's alert dialog: it opens on the button that keeps
//! things as they are; Tab and Shift+Tab go between the two buttons, Enter
//! or Space presses the one the keys are on, and Escape keeps things as
//! they are. The action's own key, where it has one, does it too. Every
//! key stops here, so what is behind the dialog hears none.
//!
//! It is what is in the dialog: what placed it puts it where it is seen
//! (over the workspace, or over a view), and takes it away once answered.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{Callable, Callback, Component, El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

use super::{Button, ButtonKind};

mod styles {
    horizon_ui::css_module! {"
    .alert { box-sizing: border-box; width: calc(var(--u32) * 13); max-width: 100%;
             display: flex; flex-direction: column; gap: var(--u12); padding: var(--u24);
             outline: none; background: var(--background); color: var(--text);
             font-family: var(--font-family); font-size: var(--body-size);
             line-height: var(--body-line);
        .title { font-weight: 600; }
        .description { color: var(--text-muted); }
        .answers { display: flex; justify-content: flex-end; gap: var(--u8);
                   margin-top: var(--u8); }
        .answer { display: flex; border-radius: var(--radius); }
        .answer.at { outline: var(--line-strong) solid var(--focus); outline-offset: 1px; }
    }
"}
}

/// A question, and what is done with each answer.
pub struct AlertDialog<'a> {
    pub title: &'a str,
    /// What the action does, and what is not taken back.
    pub description: &'a str,
    /// What the button that does it says.
    pub action: &'a str,
    /// The key that does it from the keyboard, shown beside its button.
    pub action_key: Option<&'a str>,
    /// What the button that keeps things as they are says.
    pub cancel: &'a str,
    /// Whether the action ends or takes away something: its button is in
    /// the danger color.
    pub danger: bool,
    pub on_action: Callback<()>,
    /// Told when it is answered with the button that keeps things, or
    /// Escape.
    pub on_cancel: Callback<()>,
}

/// The button the keys are on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum At {
    Cancel,
    Action,
}

impl Component for AlertDialog<'_> {
    fn view(self) -> El {
        let AlertDialog {
            title,
            description,
            action,
            action_key,
            cancel,
            danger,
            on_action,
            on_cancel,
        } = self;
        let at = RwSignal::new(At::Cancel);
        let typed = action_key.map(str::to_string);
        let answer =
            |label: &str, key: Option<&str>, which: At, kind: ButtonKind, press: Callback<()>| {
                el("span")
                    .class(styles::ANSWER)
                    .class_dyn(move || (at.get() == which).then_some(styles::AT))
                    .child(
                        Button {
                            label,
                            key,
                            kind,
                            on_press: Some(press),
                            ..Default::default()
                        }
                        .view(),
                    )
            };
        let kind = match danger {
            true => ButtonKind::Danger,
            false => ButtonKind::Primary,
        };
        el("div")
            .class(styles::ALERT)
            .attr("tabindex", "0")
            .attr("role", "alertdialog")
            .on("keydown", move |event| {
                let DomEventData::KeyDown(key) = &event.data else {
                    return;
                };
                let character = match &key.key {
                    Key::Character(character) => Some(character.as_str()),
                    _ => None,
                };
                match &key.key {
                    Key::Named(NamedKey::Escape) => on_cancel.run(()),
                    Key::Named(NamedKey::Tab) => at.set(match at.get_untracked() {
                        At::Cancel => At::Action,
                        At::Action => At::Cancel,
                    }),
                    Key::Named(NamedKey::Enter) => match at.get_untracked() {
                        At::Cancel => on_cancel.run(()),
                        At::Action => on_action.run(()),
                    },
                    _ if character == Some(" ") => match at.get_untracked() {
                        At::Cancel => on_cancel.run(()),
                        At::Action => on_action.run(()),
                    },
                    _ if character.is_some() && character == typed.as_deref() => {
                        on_action.run(());
                    }
                    _ => {}
                }
                // (Every key is the dialog's: what is behind it hears none.)
                prevent_default();
                stop_propagation();
            })
            .child(el("div").class(styles::TITLE).text(title))
            .child(el("div").class(styles::DESCRIPTION).text(description))
            .child(
                el("div")
                    .class(styles::ANSWERS)
                    .child(answer(
                        cancel,
                        Some("Esc"),
                        At::Cancel,
                        ButtonKind::Plain,
                        on_cancel,
                    ))
                    .child(answer(action, action_key, At::Action, kind, on_action)),
            )
            .focus()
    }
}
