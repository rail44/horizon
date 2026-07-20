//! Synthesizes realistic `TerminalFrame` sequences: mixed styled spans
//! (named / indexed / RGB colors, italic, underline), a moving cursor,
//! scrolling content that changes every frame — the shape a busy TUI
//! (build log, `htop`, editor redraw) produces on the sessiond wire.

use horizon_terminal_core::{
    NamedColor, TerminalColor, TerminalCursor, TerminalCursorShape, TerminalFrame, TerminalLine,
    TerminalSpan, TerminalUnderline,
};

/// Deterministic tiny PRNG (xorshift) so frame content is reproducible.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn pick(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

const WORDS: &[&str] = &[
    "cargo",
    "build",
    "warning",
    "error",
    "src/main.rs",
    "Compiling",
    "remoc",
    "v0.18.3",
    "horizon",
    "sessiond",
    "frame",
    "|",
    "->",
    "OK",
    "FAIL",
    "test",
    "running",
    "3.14s",
    "/home/user/project",
    "████",
    "▁▂▃▄▅▆▇",
    "λ",
    "…",
    "日本語テキスト",
];

fn color(rng: &mut Rng) -> TerminalColor {
    match rng.pick(10) {
        0..=4 => TerminalColor::Named(match rng.pick(6) {
            0 => NamedColor::Foreground,
            1 => NamedColor::Green,
            2 => NamedColor::BrightBlue,
            3 => NamedColor::Red,
            4 => NamedColor::Yellow,
            _ => NamedColor::Cyan,
        }),
        5..=7 => TerminalColor::Indexed(rng.pick(256) as u8),
        _ => TerminalColor::Rgb([rng.next_u64() as u8, rng.next_u64() as u8, rng.next_u64() as u8]),
    }
}

fn line(rng: &mut Rng, cols: usize) -> TerminalLine {
    let mut spans = Vec::new();
    let mut used = 0usize;
    while used < cols {
        let word = WORDS[rng.pick(WORDS.len() as u64) as usize];
        let width: usize = word.chars().count().max(1);
        if used + width + 1 > cols {
            break;
        }
        let styled = rng.pick(4) == 0;
        spans.push(TerminalSpan {
            text: format!("{word} "),
            columns: width + 1,
            fg: color(rng),
            bg: if rng.pick(8) == 0 {
                color(rng)
            } else {
                TerminalColor::Named(NamedColor::Background)
            },
            italic: styled && rng.pick(2) == 0,
            strikethrough: false,
            underline: if styled && rng.pick(3) == 0 {
                TerminalUnderline::Single
            } else {
                TerminalUnderline::None
            },
            underline_color: None,
        });
        used += width + 1;
    }
    TerminalLine { spans }
}

/// Builds `count` frames of `cols` x `rows`, scrolling by one line per
/// frame (every frame differs from its predecessor in every row index,
/// like real scrolling output does).
pub fn synth_frames(cols: usize, rows: usize, count: usize) -> Vec<TerminalFrame> {
    let mut rng = Rng::new(0x8f3a_11c7);
    // Pre-generate a rolling buffer of lines; frame f shows lines f..f+rows.
    let lines: Vec<TerminalLine> = (0..count + rows).map(|_| line(&mut rng, cols)).collect();
    (0..count)
        .map(|f| TerminalFrame {
            lines: lines[f..f + rows].to_vec(),
            cursor: Some(TerminalCursor {
                row: rows - 1,
                col: f % cols,
                shape: TerminalCursorShape::Block,
            }),
            selection: None,
            mouse_reporting: f % 7 == 0,
            keys_as_escape_codes: false,
            palette_overrides: if f % 5 == 0 {
                vec![(4, [0x28, 0x2c, 0x34]), (256, [0xab, 0xb2, 0xbf])]
            } else {
                Vec::new()
            },
        })
        .collect()
}
