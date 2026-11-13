//! A field to type in: an `input` or a `textarea` of a view's own, drawn
//! as part of the text around it, with a prompt over it while it is
//! empty.
//!
//! The field is the view's: it made it, hears its events and gives it
//! the keys. `Field` is the box it sits in. blitz-dom does not draw a
//! field's `placeholder`, so the prompt is an element in the same place,
//! there while whoever placed it says the field is empty.

use horizon_ui::reactive_graph::traits::Get;
use horizon_ui::reactive_graph::wrappers::read::Signal;
use horizon_ui::{Component, El, el};

mod styles {
    horizon_ui::css_module! {"
        .field { display: grid; }
        .input, .prompt { grid-area: 1 / 1; min-width: 0; }
        .input { width: 100%; box-sizing: border-box; margin: 0; padding: 0; border: none;
                 outline: none; resize: none; background: transparent; color: var(--text);
                 font-family: var(--font-family); font-size: inherit; line-height: inherit; }
        .prompt { color: var(--text-muted); pointer-events: none; }
    "}
}

/// The box `input` is typed in, at the size and in the type of the text
/// around it.
pub struct Field {
    /// The `input` or `textarea`.
    pub input: El,
    pub prompt: Option<Prompt>,
}

/// What a field says while it is empty.
pub struct Prompt {
    pub text: Signal<String>,
    /// Whether the field is empty: the prompt is there while it is.
    pub shown: Signal<bool>,
}

impl Component for Field {
    fn view(self) -> El {
        let field = el("div")
            .class(styles::FIELD)
            .child(self.input.class(styles::INPUT));
        let Some(Prompt { text, shown }) = self.prompt else {
            return field;
        };
        field.when(
            move || shown.get(),
            move || el("div").class(styles::PROMPT).text_dyn(move || text.get()),
        )
    }
}
