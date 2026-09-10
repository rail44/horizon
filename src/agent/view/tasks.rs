//! Live background-`task` rows for the agent pane: one quiet line per
//! still-running child, with its elapsed time and last observed activity.
//!
//! The rows are the pane-facing projection of `AgentSession::tasks` — the
//! ephemeral `wire::AgentWireEvent::TaskProgress` fold (see
//! `contract::TaskProgress`). A child's completion retires its row there;
//! the durable record (the `TaskNotification` transcript message) needs no
//! help from this strip.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::*;

use horizon_agent::contract::TaskProgress;

use super::super::session::AgentSession;
use crate::theme;

pub(super) struct BackgroundTasks {
    rows: Vec<TaskProgress>,
    _session_subscription: Subscription,
}

impl BackgroundTasks {
    pub(super) fn new(session: Entity<AgentSession>, cx: &mut Context<Self>) -> Self {
        let rows = session.read(cx).tasks.clone();
        let subscription = cx.observe(&session, |tasks: &mut Self, session, cx| {
            let next = session.read(cx).tasks.clone();
            if tasks.rows != next {
                tasks.rows = next;
                cx.notify();
            }
        });
        // Elapsed time advances even when no progress event arrives: the
        // same 1 Hz ticker shape the transcript's running-turn clock uses,
        // gated on having anything to show.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |tasks, cx| {
                if !tasks.rows.is_empty() {
                    cx.notify();
                }
            });
            if alive.is_err() {
                return;
            }
        })
        .detach();
        Self {
            rows,
            _session_subscription: subscription,
        }
    }
}

impl Render for BackgroundTasks {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return Empty.into_any_element();
        }
        let now_ms = unix_epoch_ms();
        let mut strip = div().flex().flex_col();
        for row in &self.rows {
            strip = strip.child(render_task_row(row, now_ms));
        }
        strip.into_any_element()
    }
}

/// One row: `task · {description} · {elapsed}` with the child's current
/// activity as a fixed-width-averse trailing segment. The description
/// ellipsizes rather than wrapping (`rows.rs`'s overflow idiom) so a long
/// description can't push the row past the pane; the trailing segments
/// never shrink away.
fn render_task_row(row: &TaskProgress, now_ms: u64) -> AnyElement {
    let elapsed = format_elapsed(now_ms.saturating_sub(row.started_at_epoch_ms));
    let mut line = div()
        .px_2()
        .py_0p5()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child(format!("task · {} · {}", row.description, elapsed)),
        );
    if let Some(activity) = &row.activity {
        line = line.child(
            div()
                .flex_shrink_0()
                .text_size(px(11.0))
                .text_color(theme::text_muted())
                .child(activity.clone()),
        );
    }
    line.into_any_element()
}

/// `1h02m03s` / `2m03s` / `3s` — compact, sortable-by-eye, always showing
/// the two most significant units.
fn format_elapsed(ms: u64) -> String {
    let seconds = ms / 1000;
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes:02}m{secs:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::format_elapsed;

    #[test]
    fn elapsed_shows_the_two_most_significant_units() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(3_000), "3s");
        assert_eq!(format_elapsed(59_999), "59s");
        assert_eq!(format_elapsed(123_000), "2m03s");
        assert_eq!(format_elapsed(3_723_000), "1h02m03s");
    }
}
