//! The selection and hover treatment for a list row.

use horizon_ui::{Class, El, el};

mod styles {
    horizon_ui::css_module! {"
        .list-row { border: var(--line) solid transparent; border-radius: var(--radius); }
        .list-row:hover { background: var(--band); }
        .list-row.selected { border-color: var(--accent); }
    "}
}

/// The selected state of a [`list_row`], independent of keyboard focus.
pub const ROW_SELECTED: Class = styles::SELECTED;

/// Builds a row with a border of `--line` in either state so selection
/// doesn't shift its contents. The caller supplies layout and behavior.
pub fn list_row() -> El {
    el("div").class(styles::LIST_ROW)
}
