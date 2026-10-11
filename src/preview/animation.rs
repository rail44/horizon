//! A preview that never stops animating: gpui-component's `Spinner` next to
//! a bar whose width loops, both repeating `gpui::Animation`s.
//!
//! It exists to look at frame pacing in a real pane — the guest should
//! advance once per host frame and no faster (see
//! `docs/preview-pane-design.md`, "Frame pacing"). The bar is there because
//! `embedded_gpui` paints sprite transformations untransformed, so the
//! spinner's rotation does not show; a changing width does.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, Animation, AnimationExt as _, AnyView, App, Context, IntoElement, Render, Window,
};
use gpui_component::spinner::Spinner;
use gpui_component::{h_flex, v_flex, Sizable as _};

/// The name the host selects this preview by.
pub const NAME: &str = "animation";

/// The heading this preview paints.
pub const LABEL: &str = "Repeating animation";

pub fn build(_window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|_| AnimationPreview).into()
}

struct AnimationPreview;

impl Render for AnimationPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .gap(px(12.))
            .p(px(12.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(Spinner::new().large())
                    .child(div().child(LABEL)),
            )
            .child(
                div()
                    .h(px(8.))
                    .w(px(40.))
                    .bg(crate::theme::accent())
                    .with_animation(
                        "pulse-bar",
                        Animation::new(Duration::from_millis(1200)).repeat(),
                        |bar, delta| bar.w(px(40. + 200. * delta)),
                    ),
            )
    }
}
