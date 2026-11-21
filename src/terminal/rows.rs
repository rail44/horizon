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

#[cfg(test)]
mod tests;
