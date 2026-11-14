//! Which of a list of paths what was typed finds, and in what order.
//!
//! The typed words are matched against the whole path the way fzf's path
//! scheme does (`nucleo-matcher`): a letter after a `/` or at another
//! boundary counts for more, every word has to be found, and case matters
//! only once a capital is typed. Among equals the shorter path comes
//! first, then the one listed first.

use std::cmp::Reverse;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// A path found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub path: String,
    /// Which of its characters were typed, in order.
    pub hits: Vec<u32>,
}

/// The best of what was found, and how many were found in all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    pub found: Vec<Found>,
    pub matched: usize,
}

/// The `most` best of `files` for `query`.
pub fn find(files: &[String], query: &str, most: usize) -> Results {
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut buffer = Vec::new();
    let mut scored: Vec<(u32, usize, usize)> = (files.iter().enumerate())
        .filter_map(|(index, file)| {
            let score = pattern.score(Utf32Str::new(file, &mut buffer), &mut matcher)?;
            Some((score, file.len(), index))
        })
        .collect();
    let matched = scored.len();
    scored.sort_unstable_by_key(|&(score, length, index)| (Reverse(score), length, index));
    scored.truncate(most);
    let found = (scored.into_iter())
        .map(|(.., index)| {
            let mut hits = Vec::new();
            let path = Utf32Str::new(&files[index], &mut buffer);
            pattern.indices(path, &mut matcher, &mut hits);
            hits.sort_unstable();
            hits.dedup();
            Found {
                path: files[index].clone(),
                hits,
            }
        })
        .collect();
    Results { found, matched }
}

#[cfg(test)]
mod tests {
    use super::find;

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    fn paths(files: &[String], query: &str) -> Vec<String> {
        (find(files, query, 100).found.into_iter())
            .map(|found| found.path)
            .collect()
    }

    #[test]
    fn nothing_typed_lists_every_file_the_shortest_path_first() {
        let files = files(&["docs/roadmap.md", "README.md", "docs/a.md"]);
        let results = find(&files, "", 100);
        assert_eq!(results.matched, 3);
        assert_eq!(
            paths(&files, ""),
            ["README.md", "docs/a.md", "docs/roadmap.md"]
        );
    }

    #[test]
    fn a_letter_after_a_slash_counts_for_more_than_one_inside_a_word() {
        let files = files(&["docs/architecture.md", "docs/research/map.md"]);
        assert_eq!(paths(&files, "rm")[0], "docs/research/map.md");
    }

    #[test]
    fn every_word_typed_has_to_be_found() {
        let files = files(&["docs/roadmap.md", "docs/research/roads.md", "README.md"]);
        assert_eq!(paths(&files, "road research"), ["docs/research/roads.md"]);
    }

    #[test]
    fn case_matters_once_a_capital_is_typed() {
        let files = files(&["README.md", "docs/readme-notes.md"]);
        assert_eq!(paths(&files, "readme").len(), 2);
        assert_eq!(paths(&files, "README"), ["README.md"]);
    }

    #[test]
    fn the_characters_typed_are_told_in_order() {
        let files = files(&["docs/roadmap.md"]);
        let found = &find(&files, "dr", 100).found[0];
        assert_eq!(found.hits, [0, 5]);
    }

    #[test]
    fn no_more_are_kept_than_asked_for_and_all_are_counted() {
        let files = files(&["a.md", "b.md", "c.md"]);
        let results = find(&files, "md", 2);
        assert_eq!((results.found.len(), results.matched), (2, 3));
    }
}
