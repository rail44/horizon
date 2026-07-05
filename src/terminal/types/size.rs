use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSize {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// Grid width in pixels (`cols * cell_width`), forwarded to the PTY's
    /// `ws_xpixel` field so every process reading `TIOCGWINSZ` — not just
    /// Horizon's own renderer — sees real geometry instead of zeros. Derived
    /// in `terminal::view` from measured font metrics; `0` means "not yet
    /// known" (e.g. before the view has laid out once).
    pub(crate) pixel_width: u16,
    /// Grid height in pixels (`rows * line_height`); see `pixel_width`.
    pub(crate) pixel_height: u16,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[cfg(test)]
impl TerminalSize {
    /// Character-cell geometry only, pixel dimensions left at `0`. Used by
    /// tests that only care about `cols`/`rows`; production pixel geometry
    /// is derived in `terminal::view` from measured font metrics.
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
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
