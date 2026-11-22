//! Which slot of the row store holds which row
//! (`docs/terminal-row-texture-design.md` §3.3, §3.4).
//!
//! A slot holds the pixels of one row. A row is found by a hash of what
//! its pixels are drawn from and then checked against what the slot keeps,
//! so finding a row clones nothing. A row not held takes an empty slot, or
//! else the one used least recently among those the frame being placed has
//! not used.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};
use std::sync::LazyLock;

/// Where a frame's row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placed {
    /// In this slot already, drawn.
    Kept(usize),
    /// Given this slot, which it is to be drawn into.
    Fresh(usize),
}

struct Entry<T> {
    hash: u64,
    row: T,
    /// The frame that last used it.
    used: u64,
}

/// The slots of a store of `capacity` rows, each holding a `T`: what a row
/// was drawn from.
pub(super) struct Slots<T> {
    entries: Vec<Option<Entry<T>>>,
    /// The slots holding rows of each hash.
    by_hash: HashMap<u64, Vec<usize>>,
    /// The frame being placed: a slot it has used is not taken from it.
    frame: u64,
}

impl<T> Slots<T> {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            entries: (0..capacity).map(|_| None).collect(),
            by_hash: HashMap::new(),
            frame: 1,
        }
    }

    /// Forgets every row: what they were drawn with has changed.
    pub(super) fn clear(&mut self) {
        self.entries.iter_mut().for_each(|entry| *entry = None);
        self.by_hash.clear();
    }

    /// Starts placing another frame's rows.
    pub(super) fn next_frame(&mut self) {
        self.frame += 1;
    }

    /// The slot of the row hashing to `hash` for which `same` holds, or a
    /// slot for it that `make` fills; either way used by this frame. `None`
    /// when every slot is used by this frame already.
    pub(super) fn place(
        &mut self,
        hash: u64,
        same: impl Fn(&T) -> bool,
        make: impl FnOnce() -> T,
    ) -> Option<Placed> {
        let frame = self.frame;
        let held = (self.by_hash.get(&hash).into_iter().flatten())
            .copied()
            .find(|slot| (self.entries[*slot].as_ref()).is_some_and(|entry| same(&entry.row)));
        if let Some(slot) = held {
            if let Some(entry) = &mut self.entries[slot] {
                entry.used = frame;
            }
            return Some(Placed::Kept(slot));
        }
        let slot = self.free().or_else(|| self.least_recent())?;
        if let Some(gone) = self.entries[slot].take() {
            let slots = self.by_hash.entry(gone.hash).or_default();
            slots.retain(|other| *other != slot);
            if slots.is_empty() {
                self.by_hash.remove(&gone.hash);
            }
        }
        self.entries[slot] = Some(Entry {
            hash,
            row: make(),
            used: frame,
        });
        self.by_hash.entry(hash).or_default().push(slot);
        Some(Placed::Fresh(slot))
    }

    fn free(&self) -> Option<usize> {
        self.entries.iter().position(Option::is_none)
    }

    fn least_recent(&self) -> Option<usize> {
        (self.entries.iter().enumerate())
            .filter_map(|(slot, entry)| Some((slot, entry.as_ref()?.used)))
            .filter(|(_, used)| *used != self.frame)
            .min_by_key(|(_, used)| *used)
            .map(|(slot, _)| slot)
    }
}

/// The seed rows are hashed with: one for the process, so that hashes taken
/// at different times are of one kind, and random, so that rows a program
/// writes cannot be made to meet on purpose.
static ROWS: LazyLock<foldhash::fast::RandomState> = LazyLock::new(Default::default);

/// The hash a row is found by: of everything its pixels are drawn from. A
/// row found by it is then checked against what the slot holds.
pub(super) fn hash_of(row: &impl Hash) -> u64 {
    ROWS.hash_one(row)
}

/// How a store's slots lie in its texture: in columns as wide as the grid,
/// each as many rows tall as fit under the texture's greatest side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Layout {
    /// The grid's width and a row's height, in device pixels.
    pub width: u32,
    pub row_px: u32,
    /// Slots to a column.
    pub per_column: u32,
    pub columns: u32,
}

impl Layout {
    /// Room for `capacity` rows of a grid `width` wide, `row_px` tall, in a
    /// texture no side of which is over `greatest`. `None` when not even one
    /// column fits.
    pub(super) fn for_rows(capacity: u32, width: u32, row_px: u32, greatest: u32) -> Option<Self> {
        let per_column = (greatest / row_px.max(1)).min(capacity.max(1));
        if per_column == 0 || width == 0 || width > greatest {
            return None;
        }
        let columns = capacity.max(1).div_ceil(per_column);
        (columns * width <= greatest).then_some(Self {
            width,
            row_px,
            per_column,
            columns,
        })
    }

    /// The texture's size.
    pub(super) fn size(&self) -> (u32, u32) {
        (self.columns * self.width, self.per_column * self.row_px)
    }

    pub(super) fn capacity(&self) -> u32 {
        self.per_column * self.columns
    }

    /// Where slot `slot`'s top left is in the texture.
    pub(super) fn origin(&self, slot: usize) -> (u32, u32) {
        let slot = slot as u32;
        (
            (slot / self.per_column) * self.width,
            (slot % self.per_column) * self.row_px,
        )
    }
}

#[cfg(test)]
mod tests;
