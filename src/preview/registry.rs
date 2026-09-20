//! The named previews a preview plugin carries.
//!
//! A preview is a name plus a constructor that builds the view with sample
//! data. Adding one is adding an entry to [`PREVIEWS`] next to the view it
//! shows; the plugin crate compiles all of them and the host picks one by
//! name. This module compiles for both targets: the shell reads the names,
//! the guest builds the views.

use gpui::{
    div, rgb, AnyView, App, Context, IntoElement, ParentElement as _, Render, Styled as _, Window,
};

use crate::preview::sample;

/// One entry: the name the shell selects by, and the constructor that builds
/// the view with whatever sample data it needs.
pub struct Preview {
    pub name: &'static str,
    pub build: fn(&mut Window, &mut App) -> AnyView,
}

/// Every preview in this build.
const PREVIEWS: &[Preview] = &[Preview {
    name: sample::NAME,
    build: sample::build,
}];

pub fn previews() -> &'static [Preview] {
    PREVIEWS
}

pub fn preview(name: &str) -> Option<&'static Preview> {
    PREVIEWS.iter().find(|preview| preview.name == name)
}

/// What the guest actually mounts on a surface: the selected preview's view
/// on the theme's own background, so a preview fills the pane rather than
/// painting over whatever the host drew there.
pub struct PreviewRoot {
    child: AnyView,
}

impl PreviewRoot {
    pub fn new(child: AnyView) -> Self {
        Self { child }
    }
}

impl Render for PreviewRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(crate::theme::background()))
            .text_color(crate::theme::text_primary())
            .child(self.child.clone())
    }
}
