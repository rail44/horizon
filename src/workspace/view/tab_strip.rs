use crate::app::command_actions::{execute_command, CommandActionState, CommandInvocation};
use crate::ui::spacing;
use crate::ui::theme;
use floem::prelude::*;

use super::chrome::chrome_close_button;

pub(crate) fn tab_strip(command_state: CommandActionState) -> impl IntoView {
    h_stack((
        tab_chip(command_state.clone(), 0),
        tab_chip(command_state.clone(), 1),
        tab_chip(command_state.clone(), 2),
        tab_chip(command_state.clone(), 3),
        tab_chip(command_state.clone(), 4),
        tab_chip(command_state, 5),
    ))
    .style(|s| {
        s.width_full()
            .height(35)
            .items_center()
            .gap(6)
            .padding_horiz(spacing::SPACING_SM)
            .background(theme::surface_base())
    })
}

fn tab_chip(command_state: CommandActionState, index: usize) -> impl IntoView {
    let workspace = command_state.workspace();
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
        label(title).style(|s| s.max_width(170).font_size(12).color(theme::text_primary())),
        chrome_close_button(closeable, {
            let command_state = command_state.clone();
            move || {
                execute_command(CommandInvocation::CloseTab { index }, command_state.clone());
            }
        }),
    ))
    .on_click_stop(move |_| {
        execute_command(
            CommandInvocation::ActivateTab { index },
            command_state.clone(),
        );
    })
    .style(move |s| {
        if !exists() {
            return s.hide();
        }

        let background = if active() {
            theme::surface_selected()
        } else {
            theme::surface_base()
        };
        let border = if active() {
            theme::accent()
        } else {
            theme::border_subtle()
        };
        s.height(27)
            .min_width(0.0)
            .items_center()
            .gap(7)
            .padding_left(spacing::SPACING_SM)
            .padding_right(3)
            .background(background)
            .border(1.0)
            .border_color(border)
    })
}
