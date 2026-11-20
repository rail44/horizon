//! What the document fetches: an image named by a `file://` URL is read
//! and drawn at its own size once it is in.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use horizon_ui::{App, each_layout, el};

/// A PNG three pixels wide and two high.
const PNG: [u8; 74] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 3, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 157, 116, 102, 26, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31,
    134, 25, 144, 57, 0, 155, 126, 11, 245, 114, 176, 185, 60, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
    96, 130,
];

/// Long enough for a file to be read and decoded.
const WHILE: Duration = Duration::from_secs(5);

/// The size the image is laid out at, once `src` is set on it, until it
/// is `wanted` or `within` has gone by.
fn laid_out(src: &str, wanted: (f64, f64), within: Duration) -> (f64, f64) {
    let size = Rc::new(Cell::new((0., 0.)));
    let seen = size.clone();
    let src = src.to_string();
    let app = App::new(400, 300, "Test", "img { display: block; }", move || {
        let image = el("img").attr("src", &src);
        each_layout(move |page| seen.set(page.size(image).unwrap_or_default()));
        el("div").child(image)
    });
    let started = Instant::now();
    loop {
        app.resolve();
        if size.get() == wanted || started.elapsed() > within {
            return size.get();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_image_named_by_a_file_url_is_drawn_at_its_own_size() {
    let directory = std::env::temp_dir().join(format!("hz-ui-files-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("a shot.png");
    std::fs::write(&file, PNG).unwrap();
    let url = format!("file://{}", file.display()).replace(' ', "%20");
    assert_eq!(laid_out(&url, (3., 2.), WHILE), (3., 2.));
    // What follows the path is not the file's.
    assert_eq!(laid_out(&format!("{url}?t=1#x"), (3., 2.), WHILE), (3., 2.));
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_file_that_is_not_there_is_drawn_as_nothing() {
    let missing = std::env::temp_dir().join("hz-ui-files-missing/none.png");
    // (Nothing comes: a short while is long enough to see that.)
    let within = Duration::from_millis(300);
    let size = laid_out(&format!("file://{}", missing.display()), (3., 2.), within);
    assert_eq!(size, (0., 0.));
}
