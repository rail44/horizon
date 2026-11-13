//! A button: a word or two to press, plain or as the one thing to do
//! next, at the size of a line of actions or small enough to sit in a
//! line of text.
//!
//! It is a `button` element: the keys reach it (Tab), a press is a click
//! or Enter or Space on it, and it is a button to whoever asks what is on
//! the page. Pressing it with the pointer leaves the keys where they
//! were.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::{Callable, Callback, Component, El, el, prevent_default, stop_propagation};
use keyboard_types::{Key, NamedKey};

mod styles {
    horizon_ui::css_module! {"
        :where(.button) { flex: none; }
        .button { margin: 0; padding: 0 var(--u12); border: none; border-radius: var(--radius);
                  background: transparent; font-family: var(--font-family);
                  font-size: var(--meta-size); line-height: var(--u28); color: var(--text);
                  cursor: pointer; outline: none; }
        .button:hover { background: var(--hover); }
        .button:focus-visible { outline: var(--line-strong) solid var(--focus);
                                outline-offset: 1px; }
        .button.small { padding: 0 var(--u8); line-height: calc(var(--u4) * 5); }
        .button.primary { background: var(--accent); color: var(--on-accent); }
        .button.primary:hover { background: var(--accent); opacity: 0.9; }
    "}
}

/// What a button is among the ones beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    #[default]
    Plain,
    /// The one thing to do next.
    Primary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    #[default]
    Regular,
    /// To sit in a line of text.
    Small,
}

/// When the pointer has pressed a button.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Press {
    /// Once it has gone down and come up on it: a click.
    #[default]
    Click,
    /// As it goes down: for what acts on a selection the click itself
    /// would take away.
    Down,
}

/// A button saying `label`.
#[derive(Default)]
pub struct Button<'a> {
    pub label: &'a str,
    pub kind: ButtonKind,
    pub size: ButtonSize,
    pub press: Press,
    /// Told of a press: by the pointer, or Enter or Space on the button.
    pub on_press: Option<Callback<()>>,
}

impl Component for Button<'_> {
    fn view(self) -> El {
        let kind = match self.kind {
            ButtonKind::Plain => None,
            ButtonKind::Primary => Some(styles::PRIMARY),
        };
        let size = match self.size {
            ButtonSize::Regular => None,
            ButtonSize::Small => Some(styles::SMALL),
        };
        let button = el("button")
            .class((styles::BUTTON, kind, size))
            .text(self.label);
        let Some(on_press) = self.on_press else {
            return button;
        };
        let by_pointer = match self.press {
            Press::Click => "click",
            Press::Down => "pointerdown",
        };
        button
            .on(by_pointer, move |_| on_press.run(()))
            // A click on what the keys can reach moves the keys to it,
            // unless that is prevented.
            .on("click", |_| prevent_default())
            // blitz-dom does not press a button for a key.
            .on("keydown", move |event| {
                let DomEventData::KeyDown(key) = &event.data else {
                    return;
                };
                let pressed = match &key.key {
                    Key::Named(NamedKey::Enter) => true,
                    Key::Character(character) => character == " ",
                    _ => false,
                };
                if pressed {
                    on_press.run(());
                    prevent_default();
                    stop_propagation();
                }
            })
    }
}
