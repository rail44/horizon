//! Derived-color swatch chip data (design option b,
//! `docs/theme-settings-view-design.md`): pure assembly of (label, color)
//! pairs read from the live `theme::scheme()` accessors, grouped the way
//! the design doc's preview section describes -- ANSI 16 (the 10 derived
//! slots visually grouped with the 6 seed hues, by stacking the normal and
//! bright rows so each bright sits directly under its own seed hue), text
//! hierarchy, surfaces + borders, semantic, diff. Pure/no GPUI beyond
//! `Hsla` itself, so this is unit-tested directly; rendering
//! (`super::mod`) just lays this data out as a grid of small rects.

use alacritty_terminal::vte::ansi::Rgb;
use gpui::Hsla;

use crate::theme;

/// One swatch: a label plus the color it currently resolves to.
#[derive(Clone, Copy)]
pub(crate) struct Chip {
    pub(crate) label: &'static str,
    pub(crate) color: Hsla,
}

/// One labeled section of the preview, laid out as one or more horizontal
/// rows of chips (the ANSI 16 group uses two rows -- normal then bright --
/// so a bright hue visually sits under its own seed hue; every other group
/// is a single row).
pub(crate) struct ChipGroup {
    pub(crate) title: &'static str,
    pub(crate) rows: Vec<Vec<Chip>>,
}

/// Every chip group the view renders, freshly read from the live scheme --
/// call again after every control change / live-apply so the chips always
/// reflect the latest edit; cheap (a handful of accessor calls, no I/O).
pub(crate) fn chip_groups() -> Vec<ChipGroup> {
    vec![
        ansi_group(),
        text_group(),
        surfaces_group(),
        semantic_group(),
        diff_group(),
    ]
}

fn rgb_hsla(rgb: Rgb) -> Hsla {
    theme::to_hsla([rgb.r, rgb.g, rgb.b])
}

fn ansi_group() -> ChipGroup {
    let scheme = theme::terminal_color_scheme();
    let chip = |label, rgb| Chip {
        label,
        color: rgb_hsla(rgb),
    };
    ChipGroup {
        title: "ANSI 16",
        rows: vec![
            vec![
                chip("black", scheme.black),
                chip("red", scheme.red),
                chip("green", scheme.green),
                chip("yellow", scheme.yellow),
                chip("blue", scheme.blue),
                chip("magenta", scheme.magenta),
                chip("cyan", scheme.cyan),
                chip("white", scheme.white),
            ],
            vec![
                chip("bright black", scheme.bright_black),
                chip("bright red", scheme.bright_red),
                chip("bright green", scheme.bright_green),
                chip("bright yellow", scheme.bright_yellow),
                chip("bright blue", scheme.bright_blue),
                chip("bright magenta", scheme.bright_magenta),
                chip("bright cyan", scheme.bright_cyan),
                chip("bright white", scheme.bright_white),
            ],
        ],
    }
}

fn text_group() -> ChipGroup {
    ChipGroup {
        title: "Text",
        rows: vec![vec![
            Chip {
                label: "primary",
                color: theme::text_primary(),
            },
            Chip {
                label: "muted",
                color: theme::text_muted(),
            },
            Chip {
                label: "subtle",
                color: theme::text_subtle(),
            },
        ]],
    }
}

fn surfaces_group() -> ChipGroup {
    ChipGroup {
        title: "Surfaces + borders",
        rows: vec![vec![
            Chip {
                label: "panel",
                color: theme::surface_panel(),
            },
            Chip {
                label: "chrome",
                color: theme::surface_chrome(),
            },
            Chip {
                label: "selected",
                color: theme::surface_selected(),
            },
            Chip {
                label: "raised",
                color: theme::surface_raised(),
            },
            Chip {
                label: "border",
                color: theme::border(),
            },
        ]],
    }
}

fn semantic_group() -> ChipGroup {
    ChipGroup {
        title: "Semantic",
        rows: vec![vec![
            Chip {
                label: "danger",
                color: theme::danger(),
            },
            Chip {
                label: "warning",
                color: theme::warning(),
            },
            Chip {
                label: "success",
                color: theme::success(),
            },
            Chip {
                label: "info",
                color: theme::info(),
            },
        ]],
    }
}

fn diff_group() -> ChipGroup {
    ChipGroup {
        title: "Diff",
        rows: vec![vec![
            Chip {
                label: "added bg",
                color: theme::diff_added_surface(),
            },
            Chip {
                label: "added text",
                color: theme::diff_added_text(),
            },
            Chip {
                label: "removed bg",
                color: theme::diff_removed_surface(),
            },
            Chip {
                label: "removed text",
                color: theme::diff_removed_text(),
            },
        ]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packed(chip: &Chip) -> u32 {
        theme::packed_from_hsla(chip.color)
    }

    #[test]
    fn every_group_is_present_with_expected_shape() {
        let groups = chip_groups();
        let titles: Vec<&str> = groups.iter().map(|group| group.title).collect();
        assert_eq!(
            titles,
            vec!["ANSI 16", "Text", "Surfaces + borders", "Semantic", "Diff"]
        );

        let ansi = &groups[0];
        assert_eq!(ansi.rows.len(), 2, "normal row + bright row");
        assert_eq!(ansi.rows[0].len(), 8);
        assert_eq!(ansi.rows[1].len(), 8);

        assert_eq!(groups[1].rows[0].len(), 3); // text: primary/muted/subtle
        assert_eq!(groups[2].rows[0].len(), 5); // surfaces: panel/chrome/selected/raised/border
        assert_eq!(groups[3].rows[0].len(), 4); // semantic: danger/warning/success/info
        assert_eq!(groups[4].rows[0].len(), 4); // diff: added bg/text, removed bg/text
    }

    #[test]
    fn ansi_group_matches_the_resolved_terminal_scheme() {
        // Compared via the exact same `rgb_hsla` conversion `ansi_group`
        // itself uses, on both sides -- not against a raw-byte pack of the
        // source `Rgb`, which can legitimately differ by up to a `u8` unit
        // per channel after a round trip through `Hsla`'s float
        // representation (this is `theme::to_hsla`/`packed_from_hsla`'s own
        // precision, not a bug in this module).
        let scheme = theme::terminal_color_scheme();
        let groups = chip_groups();
        let ansi = &groups[0];
        assert_eq!(ansi.rows[0][0].label, "black");
        assert_eq!(packed(&ansi.rows[0][0]), packed_rgb(scheme.black));
        assert_eq!(ansi.rows[0][1].label, "red");
        assert_eq!(packed(&ansi.rows[0][1]), packed_rgb(scheme.red));
        assert_eq!(ansi.rows[1][0].label, "bright black");
        assert_eq!(packed(&ansi.rows[1][0]), packed_rgb(scheme.bright_black));
    }

    fn packed_rgb(rgb: Rgb) -> u32 {
        theme::packed_from_hsla(rgb_hsla(rgb))
    }

    #[test]
    fn surfaces_group_includes_chrome() {
        let groups = chip_groups();
        let surfaces = &groups[2];
        let labels: Vec<&str> = surfaces.rows[0].iter().map(|chip| chip.label).collect();
        assert_eq!(
            labels,
            vec!["panel", "chrome", "selected", "raised", "border"]
        );
    }
}
