//! An inline link whose destination is handled by the view.

use horizon_ui::{El, el};

mod styles {
    horizon_ui::css_module! {"
        .link { color: var(--accent); cursor: pointer; }
        .link:hover { text-decoration: underline; }
    "}
}

/// Builds a link. The caller adds its inline content and handles activation.
pub fn link(on_click: impl Fn() + 'static) -> El {
    el("span")
        .class(styles::LINK)
        .on("click", move |_| on_click())
}
