use crate::terminal::{TerminalCommand, TerminalFrame};
use crate::ui::style::StyleExt;
use floem::peniko::kurbo::{Point, Size};
use floem::prelude::*;

use crate::terminal::view as terminal_view;

pub(super) fn terminal_output(
    output: impl Fn() -> TerminalFrame + Copy + 'static,
    preedit: impl Fn() -> Option<String> + 'static,
    terminal_tx: Option<crossbeam_channel::Sender<TerminalCommand>>,
    ime_cursor_area: RwSignal<(Point, Size)>,
    visible: impl Fn() -> bool + 'static,
) -> impl IntoView {
    let terminal_origin = RwSignal::new(floem::peniko::kurbo::Point::ZERO);
    terminal_view::terminal_text_view(
        output,
        preedit,
        terminal_tx,
        move || terminal_origin.get(),
        move |position, size| ime_cursor_area.set((position, size)),
    )
    .on_move(move |origin| terminal_origin.set(origin))
    .style(move |s| {
        s.absolute()
            .inset_left(0.0)
            .inset_right(0.0)
            .inset_top(34.0)
            .inset_bottom(0.0)
            .width_full()
            .height_full()
            .min_width(0.0)
            .min_height(0.0)
            .flex_basis(0.0)
            .flex_grow(1.0)
            .shown(visible())
    })
}
