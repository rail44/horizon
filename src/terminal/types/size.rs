use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }

    fn last_column(&self) -> Column {
        Column(self.columns().saturating_sub(1))
    }

    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines().saturating_sub(1) as i32)
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
}
