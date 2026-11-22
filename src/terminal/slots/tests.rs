use super::*;

/// Places `rows` as one frame, each found by its own value.
fn frame(slots: &mut Slots<&'static str>, rows: &[&'static str]) -> Vec<Option<Placed>> {
    slots.next_frame();
    (rows.iter())
        .map(|row| slots.place(hash_of(row), |held| held == row, || *row))
        .collect()
}

trait Slot {
    fn slot(self) -> usize;
}

impl Slot for Placed {
    fn slot(self) -> usize {
        match self {
            Placed::Kept(slot) | Placed::Fresh(slot) => slot,
        }
    }
}

fn fresh(placed: &[Option<Placed>]) -> usize {
    (placed.iter())
        .filter(|placed| matches!(placed, Some(Placed::Fresh(_))))
        .count()
}

#[test]
fn a_row_drawn_before_is_found_in_its_slot_wherever_it_now_is() {
    let mut slots = Slots::new(6);
    let first = frame(&mut slots, &["a", "b", "c"]);
    assert_eq!(fresh(&first), 3);
    // Output that scrolls: the rows moved up one, and one is new.
    let second = frame(&mut slots, &["b", "c", "d"]);
    assert_eq!(fresh(&second), 1);
    assert_eq!(second[0], Some(Placed::Kept(first[1].unwrap().slot())));
    assert_eq!(second[1], Some(Placed::Kept(first[2].unwrap().slot())));
}

#[test]
fn rows_alike_share_one_slot() {
    let mut slots = Slots::new(4);
    let placed = frame(&mut slots, &["", "x", "", ""]);
    assert_eq!(fresh(&placed), 2);
    assert_eq!(placed[0].unwrap().slot(), placed[2].unwrap().slot());
    assert!(matches!(placed[2], Some(Placed::Kept(_))));
    assert_eq!(placed[2], placed[3]);
}

#[test]
fn a_new_row_takes_the_slot_used_least_recently_and_never_one_this_frame_uses() {
    let mut slots = Slots::new(3);
    frame(&mut slots, &["a", "b", "c"]);
    frame(&mut slots, &["b", "c"]);
    // "a" was used longest ago: "d" takes its slot, not "b"'s or "c"'s.
    let placed = frame(&mut slots, &["b", "c", "d"]);
    assert_eq!(fresh(&placed), 1);
    let again = frame(&mut slots, &["b", "c", "d"]);
    assert_eq!(fresh(&again), 0);
    // "a" is gone.
    let back = frame(&mut slots, &["a"]);
    assert_eq!(fresh(&back), 1);
}

#[test]
fn a_frame_with_more_rows_than_slots_runs_out_rather_than_overwriting_itself() {
    let mut slots = Slots::new(2);
    let placed = frame(&mut slots, &["a", "b", "c"]);
    assert!(placed[0].is_some() && placed[1].is_some());
    assert_eq!(placed[2], None);
}

#[test]
fn rows_whose_hashes_meet_are_told_apart_by_what_they_hold() {
    let mut slots = Slots::new(3);
    slots.next_frame();
    let a = slots.place(7, |held| *held == "a", || "a");
    let b = slots.place(7, |held| *held == "b", || "b");
    assert!(matches!(a, Some(Placed::Fresh(_))));
    assert!(matches!(b, Some(Placed::Fresh(_))));
    assert_ne!(a.unwrap().slot(), b.unwrap().slot());
    slots.next_frame();
    assert_eq!(
        slots.place(7, |held| *held == "b", || "b"),
        Some(Placed::Kept(b.unwrap().slot()))
    );
}

#[test]
fn finding_a_row_gives_it_no_slot_and_keeps_the_one_it_has_from_being_taken() {
    let mut slots = Slots::new(2);
    frame(&mut slots, &["a", "b"]);
    slots.next_frame();
    assert_eq!(slots.find(hash_of(&"c"), |held| *held == "c"), None);
    let a = slots.find(hash_of(&"a"), |held| *held == "a");
    assert!(a.is_some());
    // "b" is the only slot this frame has not used.
    let c = slots.place(hash_of(&"c"), |held| *held == "c", || "c");
    assert!(matches!(c, Some(Placed::Fresh(_))));
    assert_ne!(c.unwrap().slot(), a.unwrap());
}

#[test]
fn cleared_slots_hold_nothing() {
    let mut slots = Slots::new(2);
    frame(&mut slots, &["a", "b"]);
    slots.clear();
    assert_eq!(fresh(&frame(&mut slots, &["a", "b"])), 2);
}

#[test]
fn slots_lie_in_columns_under_the_greatest_side() {
    // 50 rows of 27 pixels: 1350 tall fits one column under 8192.
    let layout = Layout::for_rows(50, 1800, 27, 8192).unwrap();
    assert_eq!((layout.per_column, layout.columns), (50, 1));
    assert_eq!(layout.size(), (1800, 1350));
    assert_eq!(layout.origin(3), (0, 81));
    // Under 1000 pixels a column holds 37 rows of 27; 50 take two columns.
    let layout = Layout::for_rows(50, 300, 27, 1000).unwrap();
    assert_eq!((layout.per_column, layout.columns), (37, 2));
    assert_eq!(layout.origin(37), (300, 0));
    assert!(layout.capacity() >= 50);
    let (width, height) = layout.size();
    assert!(width <= 1000 && height <= 1000);
    // Too wide for even two columns.
    assert_eq!(Layout::for_rows(50, 600, 27, 1000), None);
}
