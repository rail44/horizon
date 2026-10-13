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

use crate::agent::next::previews as agent;
use crate::board::previews as board;
use crate::preview::{animation, sample};

/// One entry: the name the shell selects by, and the constructor that builds
/// the view with whatever sample data it needs.
pub struct Preview {
    pub name: &'static str,
    pub build: fn(&mut Window, &mut App) -> AnyView,
}

/// Every preview in this build.
const PREVIEWS: &[Preview] = &[
    Preview {
        name: sample::NAME,
        build: sample::build,
    },
    Preview {
        name: board::LIST,
        build: board::build_list,
    },
    Preview {
        name: board::LIST_EMPTY,
        build: board::build_list_empty,
    },
    Preview {
        name: board::THREAD,
        build: board::build_thread,
    },
    Preview {
        name: board::THREAD_LONG,
        build: board::build_thread_long,
    },
    Preview {
        name: agent::RECEIPT_FIRST,
        build: agent::build_receipt_first,
    },
    Preview {
        name: agent::RECEIPT_FIRST_IDLE,
        build: agent::build_receipt_first_idle,
    },
    Preview {
        name: agent::NAMED_TARGETS,
        build: agent::build_named_targets,
    },
    Preview {
        name: agent::NAMED_TARGETS_IDLE,
        build: agent::build_named_targets_idle,
    },
    Preview {
        name: agent::PLAIN_NOTICES,
        build: agent::build_plain_notices,
    },
    Preview {
        name: agent::PLAIN_NOTICES_IDLE,
        build: agent::build_plain_notices_idle,
    },
    Preview {
        name: agent::INLINE_RUNNING,
        build: agent::build_inline_running,
    },
    Preview {
        name: agent::INLINE_RUNNING_IDLE,
        build: agent::build_inline_running_idle,
    },
    Preview {
        name: animation::NAME,
        build: animation::build,
    },
];

pub fn previews() -> &'static [Preview] {
    PREVIEWS
}

pub fn preview(name: &str) -> Option<&'static Preview> {
    PREVIEWS.iter().find(|preview| preview.name == name)
}

/// What the guest mounts on a surface: the selected preview's view filling
/// the pane, on the theme's own background and text color, so a preview that
/// styles only its own widgets still reads as part of the shell.
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
