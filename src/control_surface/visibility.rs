use super::types::{OVERVIEW_VISIBLE_ROWS, PALETTE_VISIBLE_ROWS};

pub fn overview_visible_start(selection: usize, item_count: usize) -> usize {
    if item_count <= OVERVIEW_VISIBLE_ROWS {
        return 0;
    }

    selection
        .min(item_count - 1)
        .saturating_sub(OVERVIEW_VISIBLE_ROWS - 1)
}

pub fn palette_visible_start(selection: usize, item_count: usize) -> usize {
    if item_count <= PALETTE_VISIBLE_ROWS {
        return 0;
    }

    selection
        .min(item_count - 1)
        .saturating_sub(PALETTE_VISIBLE_ROWS - 1)
}
