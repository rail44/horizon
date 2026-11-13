//! A button: a word or two to press, plain or as the one thing to do
//! next, at the size of a line of actions or small enough to sit in a
//! line of text.
//!
//! What a press does is whoever placed it's to say (`.on("click", …)`).

use horizon_ui::{El, el};

mod styles {
    horizon_ui::css_module! {"
        :where(.button) { flex: none; }
        .button { padding: 0 var(--u12); border-radius: var(--radius);
                  font-size: var(--meta-size); line-height: var(--u28); color: var(--text);
                  cursor: pointer; }
        .button:hover { background: var(--hover); }
        .button.small { padding: 0 var(--u8); line-height: calc(var(--u4) * 5); }
        .button.primary { background: var(--accent); color: var(--on-accent); }
        .button.primary:hover { background: var(--accent); opacity: 0.9; }
    "}
}

/// What a button is among the ones beside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Plain,
    /// The one thing to do next.
    Primary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonSize {
    Regular,
    /// To sit in a line of text.
    Small,
}

/// A button saying `label`.
pub fn button(kind: ButtonKind, size: ButtonSize, label: &str) -> El {
    let kind = match kind {
        ButtonKind::Plain => None,
        ButtonKind::Primary => Some(styles::PRIMARY),
    };
    let size = match size {
        ButtonSize::Regular => None,
        ButtonSize::Small => Some(styles::SMALL),
    };
    el("div").class((styles::BUTTON, kind, size)).text(label)
}
