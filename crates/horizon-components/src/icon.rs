//! An icon: a small picture in the color of the text around it, in place
//! of a word or beside one.
//!
//! The pictures are Phosphor's (<https://phosphoricons.com>, MIT), in its
//! regular weight, each the file the set gives out (`icons/phosphor`, with
//! the set's license). One of Phosphor's is one path filled with
//! `currentColor` on a square of 256, so an icon here is that path on an
//! `svg` element of the page.

use horizon_ui::{Component, El, svg};

mod styles {
    horizon_ui::css_module! {"
        .icon { flex: none; width: var(--u16); height: var(--u16); }
    "}
}

/// The icons there are, by Phosphor's names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Three dots in a row: what more there is to do.
    DotsThree,
    /// Two columns of three dots: a grip, to take something and move it.
    DotsSixVertical,
    /// A terminal's window.
    TerminalWindow,
    /// A robot: an agent.
    Robot,
    /// A warning: something waits on its owner.
    Warning,
    /// A cross in a circle: it failed.
    XCircle,
    /// A circle with a notch: at work.
    CircleNotch,
    /// A circle.
    Circle,
}

impl Icon {
    /// The set's file.
    fn file(self) -> &'static str {
        match self {
            Self::DotsThree => include_str!("../icons/phosphor/dots-three.svg"),
            Self::DotsSixVertical => include_str!("../icons/phosphor/dots-six-vertical.svg"),
            Self::TerminalWindow => include_str!("../icons/phosphor/terminal-window.svg"),
            Self::Robot => include_str!("../icons/phosphor/robot.svg"),
            Self::Warning => include_str!("../icons/phosphor/warning.svg"),
            Self::XCircle => include_str!("../icons/phosphor/x-circle.svg"),
            Self::CircleNotch => include_str!("../icons/phosphor/circle-notch.svg"),
            Self::Circle => include_str!("../icons/phosphor/circle.svg"),
        }
    }

    /// The path the file draws.
    fn path(self) -> &'static str {
        let file = self.file();
        (file.split_once(" d=\""))
            .and_then(|(_, rest)| rest.split_once('"'))
            .map_or("", |(path, _)| path)
    }
}

impl Component for Icon {
    fn view(self) -> El {
        svg("svg")
            .class(styles::ICON)
            .attr("viewBox", "0 0 256 256")
            .attr("fill", "currentColor")
            .child(svg("path").attr("d", self.path()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_icon_s_file_has_the_one_path_it_draws() {
        for icon in [
            Icon::DotsThree,
            Icon::DotsSixVertical,
            Icon::TerminalWindow,
            Icon::Robot,
            Icon::Warning,
            Icon::XCircle,
            Icon::CircleNotch,
            Icon::Circle,
        ] {
            let path = icon.path();
            assert!(path.starts_with('M') && path.ends_with('Z'), "{path}");
            assert_eq!(icon.file().matches("<path").count(), 1);
        }
    }
}
