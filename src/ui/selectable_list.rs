use floem::prelude::*;
use floem::reactive::create_effect;
use floem::ViewId;

/// A vertically scrolling list of selectable rows.
///
/// Renders one row per item with `dyn_stack` and keeps the row at `selection`
/// visible via floem's `scroll_to_view`. This replaces hand-unrolled fixed rows
/// plus manual viewport windowing: `count` and `selection` are reactive, and
/// the list scrolls once its content exceeds `viewport_height`.
///
/// `row` builds the view for a given index; it should read its own item and
/// selection state reactively so rows update in place without rebuilding the
/// whole list.
pub(crate) fn selectable_list<V>(
    count: impl Fn() -> usize + 'static,
    selection: impl Fn() -> usize + Copy + 'static,
    row: impl Fn(usize) -> V + 'static,
    viewport_height: f64,
) -> impl IntoView
where
    V: IntoView + 'static,
{
    let selected_view = RwSignal::new(None::<ViewId>);

    let content = dyn_stack(
        move || (0..count()).collect::<Vec<usize>>(),
        |index| *index,
        move |index| {
            let view = row(index).into_view();
            let id = view.id();
            // Report this row's id whenever it becomes the selection so the
            // scroll container can pan the minimum distance to reveal it. Runs
            // in its own effect, so changing the selection never rebuilds rows.
            create_effect(move |_| {
                if selection() == index {
                    selected_view.set(Some(id));
                }
            });
            view
        },
    )
    .style(|s| s.width_full().flex_col());

    scroll(content)
        .scroll_to_view(move || selected_view.get())
        .style(move |s| s.width_full().height(viewport_height))
}
