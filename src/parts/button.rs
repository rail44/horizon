//! A button: a word or two to press, plain or as the one thing to do
//! next, at the size of a line of actions or small enough to sit in a
//! line of text.

use horizon_ui::{Callable, Callback, Component, El, el};

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

/// A button saying `label`.
#[derive(Default)]
pub struct Button<'a> {
    pub label: &'a str,
    pub kind: ButtonKind,
    pub size: ButtonSize,
    /// Told of a press: a click on the button.
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
        let button = el("div")
            .class((styles::BUTTON, kind, size))
            .text(self.label);
        if let Some(on_press) = self.on_press {
            button.on("click", move |_| on_press.run(()));
        }
        button
    }
}
