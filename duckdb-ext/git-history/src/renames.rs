//! git's `-M` rename detection (`diffcore-rename.c`, `diffcore-delta.c` of
//! git v2.55.0), applied to the deletions and additions of one tree diff.
//!
//! libgit2's `find_similar` scores similarity with a different metric and
//! has no basename pass, so it pairs files differently from `git log -M`
//! on real histories. This module follows git's steps in git's order:
//! exact renames, then unique-basename matches at a raised threshold, then
//! the candidate matrix, each step consuming the sources it used.

use std::cmp::Ordering;
use std::collections::HashMap;

use git2::{Delta, FileMode, Oid, Repository};

use crate::history::{Change, is_binary};

/// git's `MAX_SCORE` and `DEFAULT_RENAME_SCORE` (50 %).
const MAX_SCORE: f64 = 60000.0;
const MINIMUM_SCORE: i64 = 30000;
/// git's `NUM_CANDIDATE_PER_DST`.
const CANDIDATES_PER_DST: usize = 4;
/// git's `diff.renameLimit` default.
const RENAME_LIMIT: usize = 1000;
/// git's `HASHBASE`.
const HASHBASE: u32 = 107927;

fn is_regular(mode: FileMode) -> bool {
    matches!(mode, FileMode::Blob | FileMode::BlobExecutable)
}

/// git's `basename_same`: the paths agree from the last `/` to the end.
fn basename_same(src: &str, dst: &str) -> bool {
    let (src, dst) = (src.as_bytes(), dst.as_bytes());
    let (mut i, mut j) = (src.len(), dst.len());
    while i > 0 && j > 0 {
        i -= 1;
        j -= 1;
        if src[i] != dst[j] {
            return false;
        }
        if src[i] == b'/' {
            return true;
        }
    }
    (i == 0 || src[i - 1] == b'/') && (j == 0 || dst[j - 1] == b'/')
}

fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, base)| base)
}

/// git's `hash_chars`: byte counts of line-or-64-byte spans, keyed by span hash.
fn span_hashes(data: &[u8], is_text: bool) -> HashMap<u32, u64> {
    let mut spans = HashMap::new();
    let (mut accum1, mut accum2, mut n) = (0u32, 0u32, 0u64);
    let mut rest = data;
    while let Some((&c, tail)) = rest.split_first() {
        rest = tail;
        // Ignore CR in CRLF sequence if text.
        if is_text && c == b'\r' && rest.first() == Some(&b'\n') {
            continue;
        }
        let old1 = accum1;
        accum1 = (accum1 << 7) ^ (accum2 >> 25);
        accum2 = (accum2 << 7) ^ (old1 >> 25);
        accum1 = accum1.wrapping_add(u32::from(c));
        n += 1;
        if n < 64 && c != b'\n' {
            continue;
        }
        *spans
            .entry(accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE)
            .or_insert(0) += n;
        n = 0;
        accum1 = 0;
        accum2 = 0;
    }
    if n > 0 {
        *spans
            .entry(accum1.wrapping_add(accum2.wrapping_mul(0x61)) % HASHBASE)
            .or_insert(0) += n;
    }
    spans
}

/// Blob sizes and span hashes, computed once per blob.
struct Blobs<'r> {
    repo: &'r Repository,
    sizes: HashMap<Oid, u64>,
    spans: HashMap<Oid, HashMap<u32, u64>>,
}

impl Blobs<'_> {
    fn size(&mut self, id: Oid) -> Result<u64, git2::Error> {
        if let Some(&size) = self.sizes.get(&id) {
            return Ok(size);
        }
        let (size, _) = self.repo.odb()?.read_header(id)?;
        self.sizes.insert(id, size as u64);
        Ok(size as u64)
    }

    fn load_spans(&mut self, id: Oid, path: &str) -> Result<(), git2::Error> {
        if !self.spans.contains_key(&id) {
            let blob = self.repo.find_blob(id)?;
            let is_text = !is_binary(self.repo, path, blob.content());
            self.spans.insert(id, span_hashes(blob.content(), is_text));
        }
        Ok(())
    }

    /// git's `estimate_similarity`.
    fn similarity(
        &mut self,
        src: &Change,
        dst: &Change,
        minimum_score: i64,
    ) -> Result<i64, git2::Error> {
        if !is_regular(src.old.mode) || !is_regular(dst.new.mode) {
            return Ok(0);
        }
        let (src_size, dst_size) = (self.size(src.old.id)?, self.size(dst.new.id)?);
        let max_size = src_size.max(dst_size);
        let delta_size = max_size - src_size.min(dst_size);
        if (max_size as f64) * (MAX_SCORE - minimum_score as f64) < (delta_size as f64) * MAX_SCORE
        {
            return Ok(0);
        }
        if dst_size == 0 {
            return Ok(0);
        }
        self.load_spans(src.old.id, &src.old_path)?;
        self.load_spans(dst.new.id, &dst.path)?;
        let (src_spans, dst_spans) = (&self.spans[&src.old.id], &self.spans[&dst.new.id]);
        let copied: u64 = src_spans
            .iter()
            .map(|(hash, &count)| count.min(dst_spans.get(hash).copied().unwrap_or(0)))
            .sum();
        Ok((copied as f64 * MAX_SCORE / max_size as f64) as i64)
    }
}

#[derive(Debug, Clone, Copy)]
struct Score {
    /// Index into the destination list, or `None` for an empty slot.
    dst: Option<usize>,
    src: usize,
    score: i64,
    name_score: bool,
}

/// git's `score_compare`: empty slots last, then higher score, then same basename.
fn score_compare(a: &Score, b: &Score) -> Ordering {
    match (a.dst, b.dst) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        _ => b.score.cmp(&a.score).then(b.name_score.cmp(&a.name_score)),
    }
}

/// Pair deletions with additions as git's `-M` does, turning each paired
/// addition into a rename and dropping the deletion it consumed.
pub(crate) fn detect(repo: &Repository, changes: &mut Vec<Change>) -> Result<(), git2::Error> {
    let dsts: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].status == Delta::Added)
        .collect();
    let mut srcs: Vec<usize> = (0..changes.len())
        .filter(|&i| changes[i].status == Delta::Deleted)
        .collect();
    if dsts.is_empty() || srcs.is_empty() {
        return Ok(());
    }
    // dst position -> src change index
    let mut paired: Vec<Option<usize>> = vec![None; dsts.len()];
    let mut used = vec![false; changes.len()];

    // Exact renames: the first unused source with the same blob, preferring the same basename.
    for (d, &dst) in dsts.iter().enumerate() {
        let target = &changes[dst];
        let mut best: Option<(usize, u8)> = None;
        let mut budget = 100;
        for &src in &srcs {
            let source = &changes[src];
            if source.old.id != target.new.id {
                continue;
            }
            if (!is_regular(source.old.mode) || !is_regular(target.new.mode))
                && source.old.mode != target.new.mode
            {
                continue;
            }
            if used[src] {
                continue;
            }
            let score = 1 + u8::from(basename_same(&source.old_path, &target.path));
            if best.is_none_or(|(_, best_score)| score > best_score) {
                best = Some((src, score));
                if score == 2 {
                    break;
                }
            }
            budget -= 1;
            if budget == 0 {
                break;
            }
        }
        if let Some((src, _)) = best {
            paired[d] = Some(src);
            used[src] = true;
        }
    }
    srcs.retain(|&src| !used[src]);

    let mut blobs = Blobs {
        repo,
        sizes: HashMap::new(),
        spans: HashMap::new(),
    };

    // Basename matches: a basename unique among the remaining sources and
    // among the remaining destinations, at a raised threshold.
    let min_basename_score = MINIMUM_SCORE + (0.5 * (MAX_SCORE - MINIMUM_SCORE as f64)) as i64;
    let mut src_names: HashMap<&str, Option<usize>> = HashMap::new();
    for &src in &srcs {
        let base = basename(&changes[src].old_path);
        src_names
            .entry(base)
            .and_modify(|unique| *unique = None)
            .or_insert(Some(src));
    }
    let mut dst_names: HashMap<&str, Option<usize>> = HashMap::new();
    for (d, &dst) in dsts.iter().enumerate() {
        if paired[d].is_none() {
            let base = basename(&changes[dst].path);
            dst_names
                .entry(base)
                .and_modify(|unique| *unique = None)
                .or_insert(Some(d));
        }
    }
    for &src in &srcs {
        let base = basename(&changes[src].old_path);
        let Some(&dst_unique) = dst_names.get(base) else {
            continue;
        };
        let (Some(Some(_)), Some(d)) = (src_names.get(base), dst_unique) else {
            continue;
        };
        if paired[d].is_some() {
            continue;
        }
        if blobs.similarity(&changes[src], &changes[dsts[d]], min_basename_score)?
            < min_basename_score
        {
            continue;
        }
        paired[d] = Some(src);
        used[src] = true;
    }
    srcs.retain(|&src| !used[src]);

    // The candidate matrix over what is left, unless it exceeds the rename limit.
    let remaining: Vec<usize> = (0..dsts.len()).filter(|&d| paired[d].is_none()).collect();
    if !remaining.is_empty()
        && !srcs.is_empty()
        && remaining.len() * srcs.len() <= RENAME_LIMIT * RENAME_LIMIT
    {
        let mut matrix = Vec::with_capacity(remaining.len() * CANDIDATES_PER_DST);
        for &d in &remaining {
            let mut slots = [Score {
                dst: None,
                src: 0,
                score: 0,
                name_score: false,
            }; CANDIDATES_PER_DST];
            for (s, &src) in srcs.iter().enumerate() {
                let candidate = Score {
                    dst: Some(d),
                    src: s,
                    score: blobs.similarity(&changes[src], &changes[dsts[d]], MINIMUM_SCORE)?,
                    name_score: basename_same(&changes[src].old_path, &changes[dsts[d]].path),
                };
                // git's `record_if_better`: replace the worst slot if the candidate beats it.
                let mut worst = 0;
                for i in 1..CANDIDATES_PER_DST {
                    if score_compare(&slots[i], &slots[worst]) == Ordering::Greater {
                        worst = i;
                    }
                }
                if score_compare(&slots[worst], &candidate) == Ordering::Greater {
                    slots[worst] = candidate;
                }
            }
            matrix.extend(slots);
        }
        matrix.sort_by(score_compare);
        for candidate in matrix {
            let Some(d) = candidate.dst else { break };
            if candidate.score < MINIMUM_SCORE {
                break;
            }
            let src = srcs[candidate.src];
            if paired[d].is_some() || used[src] {
                continue;
            }
            paired[d] = Some(src);
            used[src] = true;
        }
    }

    for (d, &dst) in dsts.iter().enumerate() {
        if let Some(src) = paired[d] {
            let (old_path, old) = (changes[src].old_path.clone(), changes[src].old);
            let change = &mut changes[dst];
            change.status = Delta::Renamed;
            change.old_path = old_path;
            change.old = old;
        }
    }
    let mut index = 0;
    changes.retain(|_| {
        let keep = !used[index];
        index += 1;
        keep
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{basename_same, span_hashes};

    #[test]
    fn basename_same_compares_the_last_component() {
        assert!(basename_same("a/b/x.rs", "c/x.rs"));
        assert!(basename_same("x.rs", "c/x.rs"));
        assert!(!basename_same("a/yx.rs", "c/x.rs"));
        assert!(!basename_same("a/x.rs", "a/y.rs"));
    }

    #[test]
    fn spans_end_at_newlines_and_every_64_bytes() {
        let spans = span_hashes(b"ab\ncd\n", true);
        assert_eq!(spans.values().sum::<u64>(), 6);
        assert_eq!(spans.len(), 2);
        let long = vec![b'x'; 130];
        let spans = span_hashes(&long, true);
        assert_eq!(spans.values().sum::<u64>(), 130);
        // Two identical 64-byte spans and a 2-byte tail.
        assert_eq!(spans.len(), 2);
        // CR before LF is not counted in text.
        assert_eq!(span_hashes(b"a\r\n", true).values().sum::<u64>(), 2);
        assert_eq!(span_hashes(b"a\r\n", false).values().sum::<u64>(), 3);
    }
}
