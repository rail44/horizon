//! Where the grid's rows are, in device pixels: a row is a whole number of
//! them tall, so every row starts and ends on the display's pixels
//! (`docs/terminal-row-texture-design.md` §3.1).

/// How many device pixels tall a row is for a look's `line_height` (CSS
/// pixels) on a display of `scale`.
pub(super) fn row_px(line_height: f64, scale: f64) -> f64 {
    (line_height * scale).round().max(1.)
}

/// A row's height in CSS pixels for a look's `line_height` on a display
/// of `scale`: [`row_px`] device pixels.
pub(super) fn row_height(line_height: f64, scale: f64) -> f64 {
    row_px(line_height, scale) / scale
}

/// How far up, in whole device pixels, the rows of history are drawn for a
/// view `into` (a part of a row) its top row, rows being `row_px` tall. The
/// position scrolled to stays exact; only what is drawn is rounded, by
/// less than a device pixel.
pub(super) fn scroll_shift(into: f32, row_px: f64) -> f64 {
    (into as f64 * row_px).round()
}

#[cfg(test)]
mod tests;
