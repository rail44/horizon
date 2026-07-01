use crate::app::commands::close_tab;
use crate::session::Registry;
use crate::workspace::Workspace;
use floem::prelude::*;

use super::chrome::chrome_close_button;

pub fn tab_strip(workspace: RwSignal<Workspace>, sessions: RwSignal<Registry>) -> impl IntoView {
    h_stack((
        tab_chip(workspace, sessions, 0),
        tab_chip(workspace, sessions, 1),
        tab_chip(workspace, sessions, 2),
        tab_chip(workspace, sessions, 3),
        tab_chip(workspace, sessions, 4),
        tab_chip(workspace, sessions, 5),
    ))
    .style(|s| {
        s.width_full()
            .height(35)
            .items_center()
            .gap(6)
            .padding_horiz(10)
            .background(floem::peniko::Color::rgb8(21, 24, 30))
    })
}

fn tab_chip(
    workspace: RwSignal<Workspace>,
    sessions: RwSignal<Registry>,
    index: usize,
) -> impl IntoView {
    let exists = move || workspace.with(|ws| ws.tab_summaries().get(index).is_some());
    let active = move || {
        workspace.with(|ws| {
            ws.tab_summaries()
                .get(index)
                .is_some_and(|summary| summary.active)
        })
    };
    let title = move || {
        workspace.with(|ws| {
            ws.tab_summaries()
                .get(index)
                .map(|summary| {
                    format!(
                        "{}: {} [{}]",
                        summary.index + 1,
                        summary.title,
                        summary.pane_count
                    )
                })
                .unwrap_or_default()
        })
    };
    let closeable = move || workspace.with(|ws| ws.tab_count() > 1);

    h_stack((
        label(title).style(|s| {
            s.max_width(170)
                .font_size(12)
                .color(floem::peniko::Color::rgb8(233, 236, 242))
        }),
        chrome_close_button(
            move || closeable(),
            move || close_tab(workspace, sessions, index),
        ),
    ))
    .on_click_stop(move |_| {
        workspace.update(|ws| {
            ws.activate_tab_index(index);
        });
    })
    .style(move |s| {
        if !exists() {
            return s.hide();
        }

        let background = if active() {
            floem::peniko::Color::rgb8(39, 44, 54)
        } else {
            floem::peniko::Color::rgb8(21, 24, 30)
        };
        let border = if active() {
            floem::peniko::Color::rgb8(132, 220, 198)
        } else {
            floem::peniko::Color::rgb8(42, 46, 55)
        };
        s.height(27)
            .min_width(0.0)
            .items_center()
            .gap(7)
            .padding_left(10)
            .padding_right(3)
            .background(background)
            .border(1.0)
            .border_color(border)
    })
}
