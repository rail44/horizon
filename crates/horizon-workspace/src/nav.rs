//! Geometric direction resolution for workspace mode (`docs/recursive-
//! layout-design.md`'s slice 4, Focus navigation decision): `hjkl` resolves
//! to the *nearest pane in that direction by rectangle geometry* (bspwm
//! style), not by walking the layout tree's structure (i3's tree-structural
//! resolution is the source of its well-known "focus direction depends on
//! split history" wart). Both halves here are pure functions of the tree --
//! no floem/view dependency -- so they're fully unit-testable without
//! mounting a window; `workspace::mode::move_cursor` is the only caller.

use super::mode::Direction;
use super::types::{LayoutNode, PaneId, SplitAxis};

/// A pane's rectangle in *relative unit space*: the root viewport is always
/// `(0, 0)`..`(1, 1)` regardless of the window's actual pixel size. Nearest-
/// neighbor resolution only cares about relative position and overlap along
/// each axis, which is scale-invariant, so there is no need to thread real
/// pixel geometry through this layer. `x`/`y` are the top-left corner,
/// `w`/`h` the extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    const UNIT: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    fn right(&self) -> f32 {
        self.x + self.w
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }

    fn center_x(&self) -> f32 {
        self.x + self.w / 2.0
    }

    fn center_y(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

/// Every pane's rectangle in `node`, recursively subdividing `Rect::UNIT`
/// by each `Split`'s axis and child weights (mirroring `workspace::view::
/// layout_tree`'s `flex_row`/`flex_col` -- `Horizontal` divides width,
/// `Vertical` divides height). Order matches `LayoutNode::pane_ids`'s
/// pre-order, which `nearest_in_direction` relies on as its final,
/// deterministic tie-break.
pub fn pane_rects(node: &LayoutNode) -> Vec<(PaneId, Rect)> {
    let mut out = Vec::new();
    collect_rects(node, Rect::UNIT, &mut out);
    out
}

fn collect_rects(node: &LayoutNode, rect: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        LayoutNode::Pane(pane_id) => out.push((*pane_id, rect)),
        LayoutNode::Split { axis, children } => {
            let total_weight: f32 = children.iter().map(|child| child.weight).sum();
            let mut offset = 0.0;
            for child in children {
                let share = if total_weight > 0.0 {
                    child.weight / total_weight
                } else {
                    1.0 / children.len() as f32
                };
                let child_rect = match axis {
                    SplitAxis::Horizontal => Rect {
                        x: rect.x + offset * rect.w,
                        y: rect.y,
                        w: share * rect.w,
                        h: rect.h,
                    },
                    SplitAxis::Vertical => Rect {
                        x: rect.x,
                        y: rect.y + offset * rect.h,
                        w: rect.w,
                        h: share * rect.h,
                    },
                };
                collect_rects(&child.node, child_rect, out);
                offset += share;
            }
        }
    }
}

/// Resolves a directional move from `current` to the nearest pane in
/// `rects`. A pane qualifies when it lies in `direction` from `current`
/// (its leading edge at or past `current`'s trailing edge on the move axis)
/// and shares any extent with `current` on the perpendicular axis. Among
/// qualifying panes, picks by, in order: the smallest gap on the move axis;
/// the largest perpendicular overlap; the smallest perpendicular
/// center-to-center distance; and finally pre-order position in `rects` --
/// a deterministic tie-break for the fully symmetric cases (see this
/// module's tests). Returns `None` when nothing qualifies (`current` is at
/// that edge) or `current` itself isn't in `rects`.
pub fn nearest_in_direction(
    rects: &[(PaneId, Rect)],
    current: PaneId,
    direction: Direction,
) -> Option<PaneId> {
    const EPS: f32 = 1e-4;

    let current_rect = rects.iter().find(|(id, _)| *id == current)?.1;

    rects
        .iter()
        .enumerate()
        .filter(|(_, (id, _))| *id != current)
        .filter_map(|(index, (id, rect))| {
            let gap = match direction {
                Direction::Right => rect.x - current_rect.right(),
                Direction::Left => current_rect.x - rect.right(),
                Direction::Down => rect.y - current_rect.bottom(),
                Direction::Up => current_rect.y - rect.bottom(),
            };
            if gap < -EPS {
                return None;
            }

            let (overlap, center_diff) = match direction {
                Direction::Left | Direction::Right => (
                    overlap_span(rect.y, rect.bottom(), current_rect.y, current_rect.bottom()),
                    (rect.center_y() - current_rect.center_y()).abs(),
                ),
                Direction::Up | Direction::Down => (
                    overlap_span(rect.x, rect.right(), current_rect.x, current_rect.right()),
                    (rect.center_x() - current_rect.center_x()).abs(),
                ),
            };
            if overlap <= EPS {
                return None;
            }

            Some((gap.max(0.0), -overlap, center_diff, index, *id))
        })
        .min_by(|a, b| {
            (a.0, a.1, a.2, a.3)
                .partial_cmp(&(b.0, b.1, b.2, b.3))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|candidate| candidate.4)
}

fn overlap_span(a_start: f32, a_end: f32, b_start: f32, b_end: f32) -> f32 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LayoutChild;

    fn rect_of(rects: &[(PaneId, Rect)], id: PaneId) -> Rect {
        rects
            .iter()
            .find(|(rect_id, _)| *rect_id == id)
            .expect("pane present in rects")
            .1
    }

    // --- `pane_rects` -----------------------------------------------------

    #[test]
    fn a_bare_pane_occupies_the_full_unit_viewport() {
        let a = PaneId::new();
        let root = LayoutNode::Pane(a);

        assert_eq!(pane_rects(&root), vec![(a, Rect::UNIT)]);
    }

    #[test]
    fn a_horizontal_split_divides_width_by_weight() {
        let a = PaneId::new();
        let b = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(a),
                    weight: 3.0,
                },
                LayoutChild {
                    node: LayoutNode::Pane(b),
                    weight: 1.0,
                },
            ],
        };

        let rects = pane_rects(&root);

        assert_eq!(
            rect_of(&rects, a),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 0.75,
                h: 1.0
            }
        );
        assert_eq!(
            rect_of(&rects, b),
            Rect {
                x: 0.75,
                y: 0.0,
                w: 0.25,
                h: 1.0
            }
        );
    }

    #[test]
    fn a_vertical_split_divides_height_by_weight() {
        let a = PaneId::new();
        let b = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Vertical,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(a),
                    weight: 1.0,
                },
                LayoutChild {
                    node: LayoutNode::Pane(b),
                    weight: 1.0,
                },
            ],
        };

        let rects = pane_rects(&root);

        assert_eq!(
            rect_of(&rects, a),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 0.5
            }
        );
        assert_eq!(
            rect_of(&rects, b),
            Rect {
                x: 0.0,
                y: 0.5,
                w: 1.0,
                h: 0.5
            }
        );
    }

    #[test]
    fn a_nested_split_subdivides_only_its_own_slot() {
        let (root, [a_top, a_bottom, b_top, b_bottom]) = grid_2x2();

        let rects = pane_rects(&root);

        assert_eq!(
            rect_of(&rects, a_top),
            Rect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 0.5
            }
        );
        assert_eq!(
            rect_of(&rects, a_bottom),
            Rect {
                x: 0.0,
                y: 0.5,
                w: 0.5,
                h: 0.5
            }
        );
        assert_eq!(
            rect_of(&rects, b_top),
            Rect {
                x: 0.5,
                y: 0.0,
                w: 0.5,
                h: 0.5
            }
        );
        assert_eq!(
            rect_of(&rects, b_bottom),
            Rect {
                x: 0.5,
                y: 0.5,
                w: 0.5,
                h: 0.5
            }
        );
    }

    // --- `nearest_in_direction` --------------------------------------------

    /// A 2x2 grid: two Horizontal-split columns, each itself Vertical-split
    /// into a top/bottom pane. Pre-order (and hence `pane_rects` order) is
    /// `[a_top, a_bottom, b_top, b_bottom]`.
    fn grid_2x2() -> (LayoutNode, [PaneId; 4]) {
        let a_top = PaneId::new();
        let a_bottom = PaneId::new();
        let b_top = PaneId::new();
        let b_bottom = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![
                            LayoutChild {
                                node: LayoutNode::Pane(a_top),
                                weight: 1.0,
                            },
                            LayoutChild {
                                node: LayoutNode::Pane(a_bottom),
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 1.0,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![
                            LayoutChild {
                                node: LayoutNode::Pane(b_top),
                                weight: 1.0,
                            },
                            LayoutChild {
                                node: LayoutNode::Pane(b_bottom),
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 1.0,
                },
            ],
        };
        (root, [a_top, a_bottom, b_top, b_bottom])
    }

    #[test]
    fn a_2x2_grid_resolves_all_four_directions_to_the_adjacent_pane() {
        let (root, [a_top, a_bottom, b_top, b_bottom]) = grid_2x2();
        let rects = pane_rects(&root);

        assert_eq!(
            nearest_in_direction(&rects, a_top, Direction::Right),
            Some(b_top)
        );
        assert_eq!(
            nearest_in_direction(&rects, a_top, Direction::Down),
            Some(a_bottom)
        );
        assert_eq!(nearest_in_direction(&rects, a_top, Direction::Left), None);
        assert_eq!(nearest_in_direction(&rects, a_top, Direction::Up), None);

        assert_eq!(
            nearest_in_direction(&rects, b_bottom, Direction::Left),
            Some(a_bottom)
        );
        assert_eq!(
            nearest_in_direction(&rects, b_bottom, Direction::Up),
            Some(b_top)
        );
        assert_eq!(
            nearest_in_direction(&rects, b_bottom, Direction::Right),
            None
        );
        assert_eq!(
            nearest_in_direction(&rects, b_bottom, Direction::Down),
            None
        );
    }

    /// A wide left pane spanning the full height, beside a right column
    /// split top/bottom -- an asymmetric layout where moving right from the
    /// left pane has two equally-overlapping, equally-centered candidates
    /// (`docs/recursive-layout-design.md`'s tie-break case). Pre-order is
    /// `[left, r_top, r_bottom]`.
    fn l_shape() -> (LayoutNode, PaneId, PaneId, PaneId) {
        let left = PaneId::new();
        let r_top = PaneId::new();
        let r_bottom = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(left),
                    weight: 2.0,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![
                            LayoutChild {
                                node: LayoutNode::Pane(r_top),
                                weight: 1.0,
                            },
                            LayoutChild {
                                node: LayoutNode::Pane(r_bottom),
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 1.0,
                },
            ],
        };
        (root, left, r_top, r_bottom)
    }

    #[test]
    fn moving_right_from_a_tall_pane_reaches_an_overlapping_candidate() {
        let (root, left, r_top, _r_bottom) = l_shape();
        let rects = pane_rects(&root);

        // `left` overlaps both `r_top` and `r_bottom` equally (same gap,
        // same overlap fraction, same perpendicular-center distance) --
        // the tie must resolve deterministically to the pre-order-first
        // candidate, `r_top`.
        assert_eq!(
            nearest_in_direction(&rects, left, Direction::Right),
            Some(r_top)
        );
    }

    #[test]
    fn moving_left_from_either_right_pane_reaches_the_tall_left_pane() {
        let (root, left, r_top, r_bottom) = l_shape();
        let rects = pane_rects(&root);

        assert_eq!(
            nearest_in_direction(&rects, r_top, Direction::Left),
            Some(left)
        );
        assert_eq!(
            nearest_in_direction(&rects, r_bottom, Direction::Left),
            Some(left)
        );
    }

    #[test]
    fn edges_of_the_l_shape_stay_put() {
        let (root, left, r_top, r_bottom) = l_shape();
        let rects = pane_rects(&root);

        // `left` is already the leftmost pane.
        assert_eq!(nearest_in_direction(&rects, left, Direction::Left), None);
        // `r_top`/`r_bottom` are already the rightmost column: nothing
        // further right, and `Up`/`Down` don't overlap `left` on the x
        // axis at all, only each other.
        assert_eq!(nearest_in_direction(&rects, r_top, Direction::Right), None);
        assert_eq!(nearest_in_direction(&rects, r_top, Direction::Up), None);
        assert_eq!(
            nearest_in_direction(&rects, r_bottom, Direction::Down),
            None
        );
    }

    #[test]
    fn a_larger_perpendicular_overlap_wins_over_a_smaller_one_at_the_same_gap() {
        // Left column is one full-height pane; right column is split into a
        // large top pane and a small bottom sliver. Moving right from the
        // left pane must prefer the large-overlap top candidate over the
        // sliver, even though both are at the same gap (0).
        let left = PaneId::new();
        let r_top = PaneId::new();
        let r_bottom = PaneId::new();
        let root = LayoutNode::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                LayoutChild {
                    node: LayoutNode::Pane(left),
                    weight: 1.0,
                },
                LayoutChild {
                    node: LayoutNode::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![
                            LayoutChild {
                                node: LayoutNode::Pane(r_top),
                                weight: 9.0,
                            },
                            LayoutChild {
                                node: LayoutNode::Pane(r_bottom),
                                weight: 1.0,
                            },
                        ],
                    },
                    weight: 1.0,
                },
            ],
        };
        let rects = pane_rects(&root);

        assert_eq!(
            nearest_in_direction(&rects, left, Direction::Right),
            Some(r_top)
        );
    }

    #[test]
    fn nearest_in_direction_is_none_for_a_pane_id_not_in_rects() {
        let (root, ..) = l_shape();
        let rects = pane_rects(&root);
        let unknown = PaneId::new();

        assert_eq!(
            nearest_in_direction(&rects, unknown, Direction::Right),
            None
        );
    }
}
