//! A chip: a word or two on a tinted ground, in a tone.

use horizon_ui::{El, el};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Accent,
    Success,
    Info,
    Warning,
    Danger,
}

/// A chip saying `text`, in `tone`.
pub fn chip(tone: Tone, text: &str) -> El {
    chip_of(tone).text(text)
}

/// A chip in `tone` whose words whoever asked puts in it.
pub fn chip_of(tone: Tone) -> El {
    let tone = match tone {
        Tone::Muted => styles::MUTED,
        Tone::Accent => styles::ACCENT,
        Tone::Success => styles::SUCCESS,
        Tone::Info => styles::INFO,
        Tone::Warning => styles::WARNING,
        Tone::Danger => styles::DANGER,
    };
    el("span").class((styles::CHIP, tone))
}
