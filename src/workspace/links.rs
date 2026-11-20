//! What a link followed in a view opens: a web page with the desktop, and
//! a file on this machine with the extension that opens files of its
//! media type (`horizon:ui/opener`), the first of them in the order the
//! extensions are read. Anything else is left alone.

use std::path::{Path, PathBuf};

use horizon_shell_runtime::terminal_links::is_openable_url;
use horizon_ui::blitz_traits::net::Url;

use super::{Shell, plugins};

impl Shell {
    /// Follows `link`, a URI a view was asked to open: what a terminal
    /// pane does with a link clicked with the platform's key held.
    pub fn follow_link(&self, link: &str) {
        if is_openable_url(link) {
            (self.kept().open_url)(link);
            return;
        }
        let host = gethostname::gethostname();
        if let Some(path) = local_file(link, &host.to_string_lossy()) {
            self.open_file(&path);
        }
    }

    /// Opens the file at `path` with the first extension that opens files
    /// of its media type; `false` when none does.
    pub(super) fn open_file(&self, path: &Path) -> bool {
        let Some(extension) = (plugins::extensions().into_iter()).find(|found| found.opens(path))
        else {
            return false;
        };
        let path = path.to_path_buf();
        self.run_for_extension("open", move |asks, wake, failed| {
            extension.open(&path, asks, wake, failed)
        });
        true
    }
}

/// The file on this machine a `file://` URI names: one with no host, or
/// with `localhost` or this machine's name `own` (a program on another
/// machine, over ssh, names its own files with its own name).
fn local_file(link: &str, own: &str) -> Option<PathBuf> {
    let url = Url::parse(link).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    match url.host_str() {
        None | Some("") | Some("localhost") => {}
        Some(host) if host.eq_ignore_ascii_case(own) => {}
        Some(_) => return None,
    }
    // (`to_file_path` takes a file URI with no host only.)
    let here = Url::parse(&format!("file://{}", url.path())).ok()?;
    here.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_uri_names_a_file_here_without_a_host_or_with_this_machine_s() {
        let here = |link| local_file(link, "box");
        assert_eq!(here("file:///tmp/a.png"), Some(PathBuf::from("/tmp/a.png")));
        assert_eq!(
            here("file://localhost/tmp/a.png"),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(
            here("file://box/tmp/a.png"),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(here("file://elsewhere/tmp/a.png"), None);
    }

    #[test]
    fn a_file_uri_s_path_is_decoded() {
        assert_eq!(
            local_file("file:///tmp/a%20shot%E3%81%82.png", "box"),
            Some(PathBuf::from("/tmp/a shotあ.png"))
        );
    }

    #[test]
    fn what_is_not_a_file_uri_names_no_file() {
        assert_eq!(local_file("https://example.com/a.png", "box"), None);
        assert_eq!(local_file("/tmp/a.png", "box"), None);
        assert_eq!(local_file("vscode://file/tmp/a.png", "box"), None);
    }
}
