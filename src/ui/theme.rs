use floem::peniko::Color;

pub(crate) fn text_primary() -> Color {
    Color::rgb8(233, 236, 242)
}

pub(crate) fn text_muted() -> Color {
    Color::rgb8(178, 185, 198)
}

pub(crate) fn text_subtle() -> Color {
    Color::rgb8(115, 122, 136)
}

pub(crate) fn accent() -> Color {
    Color::rgb8(132, 220, 198)
}

/// The app's one destructive/danger accent — the same red used for the
/// agent pane's "Deny" approval action (`workspace/view/agent_controls.rs`).
/// Reused here for destructive command styling (`ui/list_row.rs`) so both
/// "reject this" and "this ends something" read as the same kind of
/// warning.
pub(crate) fn danger() -> Color {
    Color::rgb8(246, 137, 146)
}

pub(crate) fn surface_base() -> Color {
    Color::rgb8(22, 24, 29)
}

pub(crate) fn surface_panel() -> Color {
    Color::rgb8(24, 27, 32)
}

pub(crate) fn surface_raised() -> Color {
    Color::rgb8(31, 34, 41)
}

pub(crate) fn surface_chrome() -> Color {
    Color::rgb8(25, 28, 34)
}

pub(crate) fn surface_selected() -> Color {
    Color::rgb8(54, 59, 70)
}

pub(crate) fn border_default() -> Color {
    Color::rgb8(54, 59, 70)
}

pub(crate) fn border_subtle() -> Color {
    Color::rgb8(42, 46, 55)
}
