//! A short notice in the theme's meta text.

use horizon_ui::{Component, El, el};

mod styles {
    horizon_ui::css_module! {"
        .notice { font-size: var(--meta-size); line-height: var(--meta-line);
                  color: var(--text-muted); }
        .notice.warning { color: var(--warning); }
        .notice.danger { color: var(--danger); }
    "}
}

/// A notice's tone. The view placing it supplies its text and spacing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Notice {
    #[default]
    Muted,
    Warning,
    Danger,
}

impl Component for Notice {
    fn view(self) -> El {
        let tone = match self {
            Self::Muted => None,
            Self::Warning => Some(styles::WARNING),
            Self::Danger => Some(styles::DANGER),
        };
        el("div").class((styles::NOTICE, tone))
    }
}
