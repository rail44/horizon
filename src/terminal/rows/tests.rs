use super::*;

#[test]
fn a_row_is_a_whole_number_of_device_pixels_at_any_scale() {
    for line_height in [16., 18., 19., 25.] {
        for scale in [1., 1.25, 1.5, 1.75, 2., 2.25] {
            let px = row_px(line_height, scale);
            assert_eq!(px.fract(), 0., "{line_height} at {scale}: {px}");
            // Every row boundary lands on a device pixel.
            let css = row_height(line_height, scale);
            for row in 0..60 {
                let edge = row as f64 * css * scale;
                assert!(
                    (edge - edge.round()).abs() < 1e-6,
                    "row {row} of {line_height} at {scale}: {edge}"
                );
            }
        }
    }
}

#[test]
fn history_is_drawn_shifted_by_whole_device_pixels() {
    assert_eq!(scroll_shift(0., 27.), 0.);
    assert_eq!(scroll_shift(0.5, 18.), 9.);
    // Half of 27 is 13.5: drawn at 14, a third of a pixel from exact.
    assert_eq!(scroll_shift(0.5, 27.), 14.);
    for step in 0..100 {
        let into = step as f32 / 100.;
        let shift = scroll_shift(into, 23.);
        assert_eq!(shift.fract(), 0.);
        assert!((shift - into as f64 * 23.).abs() <= 0.5, "{into}: {shift}");
    }
}

#[test]
fn a_row_is_the_line_height_where_that_is_whole_already() {
    assert_eq!(row_px(18., 1.), 18.);
    assert_eq!(row_px(18., 1.5), 27.);
    assert_eq!(row_height(18., 1.5), 18.);
    // 22.5 device pixels is not a row: it is 23, a little over 18 CSS pixels.
    assert_eq!(row_px(18., 1.25), 23.);
    assert!((row_height(18., 1.25) - 18.4).abs() < 1e-9);
}
