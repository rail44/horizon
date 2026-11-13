//! A chip: a word or two on a tinted ground, in a tone.

use horizon_ui::{Component, El, el};

mod styles {
    horizon_ui::css_module! {"
        :where(.chip) { flex: none; }
        .chip { padding: 0 var(--u4); border-radius: 2px; font-size: var(--micro-size);
                line-height: var(--micro-line); }
        .chip.muted { background: var(--chip-muted-surface); color: var(--chip-muted-text); }
        .chip.accent { background: var(--chip-accent-surface); color: var(--chip-accent-text); }
        .chip.success { background: var(--chip-success-surface);
                        color: var(--chip-success-text); }
        .chip.info { background: var(--chip-info-surface); color: var(--chip-info-text); }
        .chip.warning { background: var(--chip-warning-surface);
                        color: var(--chip-warning-text); }
        .chip.danger { background: var(--chip-danger-surface); color: var(--chip-danger-text); }
    "}
}

/// What a chip, or a status's dot, is drawn in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Muted,
    Accent,
    Success,
    Info,
    Warning,
    Danger,
}

/// A chip saying `text`, in `tone`. With no text, its words are whoever
/// placed it's to put in.
#[derive(Default)]
pub struct Chip<'a> {
    pub tone: Tone,
    pub text: &'a str,
}

impl Component for Chip<'_> {
    fn view(self) -> El {
        let tone = match self.tone {
            Tone::Muted => styles::MUTED,
            Tone::Accent => styles::ACCENT,
            Tone::Success => styles::SUCCESS,
            Tone::Info => styles::INFO,
            Tone::Warning => styles::WARNING,
            Tone::Danger => styles::DANGER,
        };
        let chip = el("span").class((styles::CHIP, tone));
        match self.text.is_empty() {
            true => chip,
            false => chip.text(self.text),
        }
    }
}
