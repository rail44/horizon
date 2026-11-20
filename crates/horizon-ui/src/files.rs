//! What the document fetches: a file on this machine, named by a
//! `file://` URL (an `<img>`'s `src`, a `background-image`), read on a
//! thread of its own. blitz-dom decodes what it is handed on that thread
//! too, and takes the result in at its next layout.
//!
//! The URL's query and fragment are not the file's: a view may put
//! something there to have the same file fetched again, since blitz-dom
//! keeps what it fetched by the whole URL.

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
// (Only its formats: blitz-dom decodes with it, and they are turned on
// here.)
use image as _;

/// Reads what a `file://` URL names, and nothing else.
pub(crate) struct Files;

impl NetProvider for Files {
    fn fetch(&self, _document: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url;
        let named = url.to_string();
        if url.scheme() != "file" {
            handler.error(named, format!("not a file: {}", url.scheme()));
            return;
        }
        std::thread::spawn(move || {
            let mut file = url.clone();
            file.set_query(None);
            file.set_fragment(None);
            let read = (file.to_file_path())
                .map_err(|()| "not a file on this machine".to_string())
                .and_then(|path| std::fs::read(&path).map_err(|error| error.to_string()));
            match read {
                Ok(bytes) => handler.bytes(named, Bytes::from(bytes)),
                Err(error) => handler.error(named, error),
            }
        });
    }
}
