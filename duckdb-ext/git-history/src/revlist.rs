//! The commits `git log [since..]HEAD` lists, in its order (`revision.c` of
//! git v2.55.0, default date order without pathspecs).
//!
//! git pops commits from a priority queue on committer time whose ties go
//! to the commit queued first. libgit2's time-sorted walk breaks ties
//! differently, which reorders commits made within the same second (common
//! for merges done by scripts), so the walk is reproduced here. With a
//! `since` bottom, git's `limit_list` runs the same queue with uninteresting
//! commits in it and stops once only uninteresting ones remain.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use git2::{Oid, Repository};

/// git's `SLOP`: extra uninteresting commits examined after the last
/// interesting one, to tolerate clock skew.
const SLOP: u32 = 5;

/// Object flags; git keeps them for commits it has not parsed yet too.
#[derive(Debug, Default)]
struct Flags {
    seen: bool,
    added: bool,
    uninteresting: bool,
}

/// A parsed commit (git knows parents only for parsed commits).
#[derive(Debug)]
struct Parsed {
    time: i64,
    parents: Vec<Oid>,
}

struct Walk<'r> {
    repo: &'r Repository,
    flags: HashMap<Oid, Flags>,
    parsed: HashMap<Oid, Parsed>,
    queue: BinaryHeap<(i64, Reverse<u64>, Oid)>,
    counter: u64,
}

impl Walk<'_> {
    fn parse(&mut self, id: Oid) -> Result<&Parsed, git2::Error> {
        if !self.parsed.contains_key(&id) {
            let commit = self.repo.find_commit(id)?;
            let parsed = Parsed {
                time: commit.time().seconds(),
                parents: commit.parent_ids().collect(),
            };
            self.parsed.insert(id, parsed);
        }
        Ok(&self.parsed[&id])
    }

    fn flags(&mut self, id: Oid) -> &mut Flags {
        self.flags.entry(id).or_default()
    }

    fn is_uninteresting(&self, id: &Oid) -> bool {
        self.flags.get(id).is_some_and(|flags| flags.uninteresting)
    }

    fn put(&mut self, id: Oid) -> Result<(), git2::Error> {
        let time = self.parse(id)?.time;
        self.queue.push((time, Reverse(self.counter), id));
        self.counter += 1;
        Ok(())
    }

    /// git's `mark_parents_uninteresting`: flag the parents, and the known
    /// ancestors of those already parsed.
    fn mark_parents_uninteresting(&mut self, id: Oid) {
        let mut pending: Vec<Oid> = self.parsed[&id].parents.clone();
        while let Some(parent) = pending.pop() {
            let flags = self.flags(parent);
            if flags.uninteresting {
                continue;
            }
            flags.uninteresting = true;
            if let Some(parsed) = self.parsed.get(&parent) {
                pending.extend(parsed.parents.iter().copied());
            }
        }
    }

    /// git's `process_parents`.
    fn process_parents(&mut self, id: Oid) -> Result<(), git2::Error> {
        let flags = self.flags(id);
        if flags.added {
            return Ok(());
        }
        flags.added = true;
        let uninteresting = flags.uninteresting;
        for parent in self.parsed[&id].parents.clone() {
            self.parse(parent)?;
            if uninteresting {
                self.flags(parent).uninteresting = true;
                self.mark_parents_uninteresting(parent);
            }
            let flags = self.flags(parent);
            if !flags.seen {
                flags.seen = true;
                self.put(parent)?;
            }
        }
        Ok(())
    }
}

/// The commits of `git log [since..]HEAD`, in `git log` order.
pub(crate) fn rev_list(repo: &Repository, since: Option<Oid>) -> Result<Vec<Oid>, git2::Error> {
    let head = repo.head()?.peel_to_commit()?.id();
    let mut walk = Walk {
        repo,
        flags: HashMap::new(),
        parsed: HashMap::new(),
        queue: BinaryHeap::new(),
        counter: 0,
    };
    // `prepare_revision_walk`: the given commits, newest first (a stable sort,
    // so on a tie the bottom, listed first on the command line, stays first).
    let mut starts = Vec::new();
    if let Some(since) = since {
        walk.flags(since).uninteresting = true;
        starts.push(since);
    }
    if !starts.contains(&head) {
        starts.push(head);
    }
    for &start in &starts {
        walk.parse(start)?;
        walk.flags(start).seen = true;
    }
    starts.sort_by_key(|id| Reverse(walk.parsed[id].time));
    for start in starts {
        walk.put(start)?;
    }

    let mut out = Vec::new();
    if since.is_none() {
        // The streaming walk.
        while let Some((_, _, id)) = walk.queue.pop() {
            walk.process_parents(id)?;
            out.push(id);
        }
        return Ok(out);
    }

    // `limit_list`.
    let mut slop = SLOP;
    let mut date = i64::MAX;
    while let Some((_, _, id)) = walk.queue.pop() {
        walk.process_parents(id)?;
        if walk.is_uninteresting(&id) {
            walk.mark_parents_uninteresting(id);
            // `still_interesting`
            slop = match walk.queue.peek() {
                None => 0,
                Some(&(time, _, _)) if date <= time => SLOP,
                Some(_)
                    if walk
                        .queue
                        .iter()
                        .any(|(_, _, id)| !walk.is_uninteresting(id)) =>
                {
                    SLOP
                }
                Some(_) => slop - 1,
            };
            if slop == 0 {
                break;
            }
            continue;
        }
        date = walk.parsed[&id].time;
        out.push(id);
    }
    // Commits found uninteresting after being listed are not shown.
    out.retain(|id| !walk.is_uninteresting(id));
    Ok(out)
}
