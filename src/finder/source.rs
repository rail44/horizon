//! The files a finder finds among, and which of them what was typed
//! finds: `nucleo`, which matches on threads of its own as files are
//! handed to it and as the typed text changes, and wakes the page when it
//! has something new.
//!
//! The typed words are matched against the whole path the way fzf's path
//! scheme does: a letter after a `/` or at another boundary counts for
//! more, every word has to be found, and case matters only once a capital
//! is typed. Among equals the shorter path comes first, then the one
//! handed over first; with nothing typed the files are in the order they
//! were handed over.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use horizon_ui::reactive_graph::owner::on_cleanup;
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Matcher, Nucleo};

use super::listing::walk;
use crate::wake::Wake;

/// A path found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub path: String,
    /// Which of its characters were typed, in order.
    pub hits: Vec<u32>,
}

/// The best of what was found, and how many there are.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Results {
    pub found: Vec<Found>,
    /// How many files were found, and how many there are to find among.
    pub matched: usize,
    pub all: usize,
    /// Whether files are still being listed or matched.
    pub more: bool,
}

pub struct Source {
    nucleo: RefCell<Nucleo<String>>,
    /// What marks the characters typed in the paths shown.
    marker: RefCell<Matcher>,
    typed: RefCell<String>,
    /// Whether files are still being handed over.
    listing: Arc<AtomicBool>,
    /// Whether the last look found the matching still under way.
    matching: Cell<bool>,
}

impl Source {
    fn new(wake: Wake, listing: bool) -> Self {
        let config = Config::DEFAULT.match_paths();
        let woken = Arc::new(move || wake.wake());
        Self {
            nucleo: RefCell::new(Nucleo::new(config.clone(), woken, None, 1)),
            marker: RefCell::new(Matcher::new(config)),
            typed: RefCell::default(),
            listing: Arc::new(AtomicBool::new(listing)),
            matching: Cell::new(false),
        }
    }

    /// `files`, as paths with `/` between their parts.
    pub fn of(files: Vec<String>, wake: Wake) -> Rc<Self> {
        let source = Self::new(wake, false);
        let injector = source.nucleo.borrow().injector();
        for file in files {
            injector.push(file, |file, columns| columns[0] = file.as_str().into());
        }
        Rc::new(source)
    }

    /// The files under `root` with one of `extensions` (any file when
    /// there are none), listed on a thread of its own and handed over as
    /// they are found. The listing stops when what it was asked under
    /// ends.
    pub fn under(root: PathBuf, extensions: Vec<String>, wake: Wake) -> Rc<Self> {
        let source = Self::new(wake.clone(), true);
        let injector = source.nucleo.borrow().injector();
        let listing = source.listing.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        on_cleanup(move || stop.store(true, Ordering::Relaxed));
        std::thread::spawn(move || {
            walk(&root, &extensions, &stopped, |batch| {
                for file in batch {
                    injector.push(file, |file, columns| columns[0] = file.as_str().into());
                }
                wake.wake();
            });
            listing.store(false, Ordering::Relaxed);
            wake.wake();
        });
        Rc::new(source)
    }

    /// What is typed is now `query`.
    pub fn type_in(&self, query: &str) {
        let mut typed = self.typed.borrow_mut();
        // (Typed on to what was there, the files found before are the
        // only ones to look among.)
        let on = query.starts_with(typed.as_str());
        self.nucleo.borrow_mut().pattern.reparse(
            0,
            query,
            CaseMatching::Smart,
            Normalization::Smart,
            on,
        );
        *typed = query.to_string();
    }

    /// Takes in what has been listed and matched since the last look,
    /// waiting up to `wait` milliseconds for the matching under way.
    /// Whether there is anything new.
    pub fn look(&self, wait: u64) -> bool {
        let status = self.nucleo.borrow_mut().tick(wait);
        let was = self.matching.replace(status.running);
        status.changed || was != status.running
    }

    /// The `most` best of what was found, as of the last look.
    pub fn results(&self, most: usize) -> Results {
        let nucleo = self.nucleo.borrow();
        let snapshot = nucleo.snapshot();
        let mut marker = self.marker.borrow_mut();
        let pattern = snapshot.pattern().column_pattern(0);
        let matched = snapshot.matched_item_count();
        let found = (snapshot.matched_items(..matched.min(most as u32)))
            .map(|item| {
                let mut hits = Vec::new();
                pattern.indices(item.matcher_columns[0].slice(..), &mut marker, &mut hits);
                hits.sort_unstable();
                hits.dedup();
                Found {
                    path: item.data.clone(),
                    hits,
                }
            })
            .collect();
        Results {
            found,
            matched: matched as usize,
            all: snapshot.item_count() as usize,
            more: self.matching.get() || self.listing.load(Ordering::Relaxed),
        }
    }
}
