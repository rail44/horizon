//! A field to type in: an `input` or a `textarea` of a view's own, drawn
//! as part of the text around it, with a prompt over it while it is
//! empty.
//!
//! The field is the view's: it made it, hears its events and gives it
//! the keys. `field` is the box it sits in. blitz-dom does not draw a
//! field's `placeholder`, so the prompt is an element in the same place,
//! there while whoever placed it says the field is empty:
//!
//! ```ignore
//! field(input).when(move || typed.with(String::is_empty), || prompt("Search..."))
//! ```

use horizon_ui::{El, el};

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
pub fn field(input: El) -> El {
    el("div")
        .class(styles::FIELD)
        .child(input.class(styles::INPUT))
}

/// What a field says while it is empty: placed in the field's box.
pub fn prompt(text: &str) -> El {
    prompt_of().text(text)
}

/// A prompt whose words whoever asked puts in it.
pub fn prompt_of() -> El {
    el("div").class(styles::PROMPT)
}
