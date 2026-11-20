//! A file's media type, as what an extension takes is said in: by the
//! extension of the file's name (`mime_guess`'s table), not its contents,
//! so nothing is read to tell it.
//!
//! What is taken is said as types (`text/markdown`), a whole kind of them
//! (`image/*`), or any (`*/*`), whatever their case.

use std::path::Path;

/// Whether `path` is of one of `types`. A file whose name says no type is
/// of none.
pub fn is_of(path: &Path, types: &[String]) -> bool {
    (mime_guess::from_path(path).iter_raw())
        .any(|named| (types.iter()).any(|wanted| matches(wanted, named)))
}

/// Whether the type `named` is the one `wanted` says.
fn matches(wanted: &str, named: &str) -> bool {
    let split = |text: &str| -> Option<(String, String)> {
        let (kind, sub) = text.split_once('/')?;
        Some((
            kind.trim().to_ascii_lowercase(),
            sub.trim().to_ascii_lowercase(),
        ))
    };
    let (Some((kind, sub)), Some((named_kind, named_sub))) = (split(wanted), split(named)) else {
        return false;
    };
    (kind == "*" || kind == named_kind) && (sub == "*" || sub == named_sub)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(path: &str, types: &[&str]) -> bool {
        let types: Vec<String> = types.iter().map(|text| text.to_string()).collect();
        is_of(Path::new(path), &types)
    }

    #[test]
    fn a_type_is_matched_whole_by_kind_or_any() {
        assert!(of("shot.png", &["image/png"]));
        assert!(of("shot.png", &["image/*"]));
        assert!(of("shot.png", &["*/*"]));
        assert!(!of("shot.png", &["image/jpeg"]));
        assert!(!of("shot.png", &["text/*"]));
    }

    #[test]
    fn the_name_s_case_and_the_type_s_do_not_matter() {
        assert!(of("SHOT.PNG", &["image/png"]));
        assert!(of("notes.MD", &["Text/Markdown"]));
        assert!(of("notes.markdown", &["text/markdown"]));
    }

    #[test]
    fn a_name_that_says_no_type_is_of_none() {
        assert!(!of("Makefile", &["*/*"]));
        assert!(!of("shot.png", &[]));
        assert!(!of("shot.png", &["image"]));
    }
}
