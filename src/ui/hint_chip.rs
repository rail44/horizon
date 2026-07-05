use floem::prelude::*;

use crate::ui::fonts::font_family;
use crate::ui::theme;

/// A small "key -> action" hint chip: a bordered keycap label (e.g. `[y]`)
/// beside its bound action in muted text (e.g. `approve`).
///
/// This is the crush-inspired (charmbracelet's TUI) principle Horizon's
/// tool-approval banner follows: every interaction request should visibly
/// explain which key does what, rather than relying on the user to already
/// know. Domain-neutral and reusable outside the approval banner
/// (`workspace::view::agent_controls::agent_approval_banner`, its first
/// caller) -- any future interactive prompt (delegation-era) that wants to
/// spell out its own keybindings inline can reuse this.
pub(crate) fn key_hint(key: &'static str, action: &'static str) -> impl IntoView {
    h_stack((
        label(move || format!("[{key}]")).style(|s| {
            s.font_family(font_family().to_string())
                .font_size(11)
                .padding_horiz(4)
                .color(theme::text_primary())
                .border(1.0)
                .border_color(theme::border_default())
        }),
        label(move || action.to_string()).style(|s| {
            s.font_family(font_family().to_string())
                .font_size(11)
                .color(theme::text_muted())
        }),
    ))
    .style(|s| s.items_center().gap(5))
}
