//! An icon: a small picture in the color of the text around it, in place
//! of a word or beside one.
//!
//! The pictures are Phosphor's (<https://phosphoricons.com>, MIT), in its
//! regular weight, each the file the set gives out (`icons/phosphor`, with
//! the set's license). One of Phosphor's is one path filled with
//! `currentColor` on a square of 256, so an icon here is that path on an
//! `svg` element of the page.

use horizon_ui::{Component, El, el};

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
    /// Two rows of three dots: a grip, to take something and move it.
    DotsSix,
}

impl Icon {
    /// The set's file.
    fn file(self) -> &'static str {
        match self {
            Self::DotsThree => include_str!("../icons/phosphor/dots-three.svg"),
            Self::DotsSix => include_str!("../icons/phosphor/dots-six.svg"),
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
        el("svg")
            .class(styles::ICON)
            .attr("viewBox", "0 0 256 256")
            .attr("fill", "currentColor")
            .child(el("path").attr("d", self.path()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_icon_s_file_has_the_one_path_it_draws() {
        for icon in [Icon::DotsThree, Icon::DotsSix] {
            let path = icon.path();
            assert!(path.starts_with('M') && path.ends_with('Z'), "{path}");
            assert_eq!(icon.file().matches("<path").count(), 1);
        }
    }
}
