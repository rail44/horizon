//! The image over the workspace: at its own size, or made smaller, its
//! shape kept, to fit the window. The sheet around it is the shell's,
//! which a press outside or Escape takes away.

use std::time::{SystemTime, UNIX_EPOCH};

use horizon_ui::guest::{El, el};

mod styles {
    horizon_ui::css_module! {"
    .over { display: block; outline: none; }
    .image { display: block; max-width: 90vw; max-height: calc(100vh - 160px); }
    "}
}

/// The view of the image in the file at `path`.
pub(crate) fn over(path: &str) -> El {
    let image = el("img").class(styles::IMAGE).attr("src", &fetched(path));
    // (With the keys, for the sheet's Escape.)
    el("div")
        .class(styles::OVER)
        .attr("tabindex", "0")
        .child(image)
        .focus()
}

/// The URL the document fetches the file at `path` from.
///
/// blitz-dom keeps each image it fetched by its whole URL for as long as
/// the document lasts, and takes a URL it has from there without reading
/// it again: a file written again at the same path (a screenshot taken
/// again) would show as it was. Upstream has no way to let one go. The
/// query, the time it is opened, makes each opening a URL of its own; the
/// host reads the file without it (`horizon-ui`'s `files.rs`). What each
/// opening decoded stays in memory until the shell ends.
fn fetched(path: &str) -> String {
    let now = (SystemTime::now().duration_since(UNIX_EPOCH)).map_or(0, |since| since.as_nanos());
    format!("file://{}?opened={now}", encoded(path))
}

/// `path` as a URL's path: each byte but the unreserved ones and `/` as
/// `%XX`.
fn encoded(path: &str) -> String {
    let mut written = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                written.push(byte as char);
            }
            other => written.push_str(&format!("%{other:02X}")),
        }
    }
    written
}
