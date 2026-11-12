//! A color picker: a swatch of the color, which opens a panel to choose
//! one on: an area of saturation and brightness for a hue, a bar of the
//! hues, and a field for the color written out.

use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{El, dispatched, each_layout, el, prevent_default, relayout, stop_propagation};
use keyboard_types::{Key, NamedKey};

use super::Drags;

pub(super) const CSS: &str = "
    .color { position: relative; outline: none;
        .swatch { width: 28px; height: 28px; box-sizing: border-box; border-radius: 4px;
                  border: 1px solid var(--border); cursor: pointer; }
        .panel { position: absolute; z-index: 60; left: 0; top: 100%; margin-top: 4px;
                 display: flex; flex-direction: column; gap: 8px; padding: 8px; width: 200px;
                 border-radius: 8px; border: 1px solid var(--border);
                 background: var(--background); box-shadow: 0 4px 16px rgba(0, 0, 0, 0.15); }
        .area { position: relative; height: 140px; border-radius: 6px; cursor: pointer; }
        .area .white, .area .black { position: absolute; left: 0; top: 0; right: 0; bottom: 0;
                                     border-radius: 6px; }
        .area .white { background: linear-gradient(to right, white, rgba(255, 255, 255, 0)); }
        .area .black { background: linear-gradient(to top, black, rgba(0, 0, 0, 0)); }
        .hue { position: relative; height: 16px; border-radius: 4px; cursor: pointer;
               background: linear-gradient(to right, rgb(255, 0, 0), rgb(255, 255, 0),
                   rgb(0, 255, 0), rgb(0, 255, 255), rgb(0, 0, 255), rgb(255, 0, 255),
                   rgb(255, 0, 0)); }
        .knob { position: absolute; width: 12px; height: 12px; margin: -7px 0 0 -7px;
                border-radius: 50%; border: 2px solid white; box-shadow: 0 0 0 1px black;
                pointer-events: none; }
        .hue .knob { top: 50%; }
        .written { display: flex; align-items: center; gap: 8px; }
        .written input { flex: 1; min-width: 0; padding: 2px 6px; border-radius: 4px;
                         border: 1px solid var(--border); outline: none;
                         background: var(--background); color: var(--text);
                         font-family: var(--font-family); font-size: var(--meta-size);
                         line-height: var(--meta-line); }
        .written .swatch { flex: none; cursor: default; }
    }
    .color:focus > .swatch { border-color: var(--accent); }
";

/// A color written `#rrggbb`.
pub fn hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

/// The color `#rrggbb` (or `rrggbb`) writes.
pub fn parse_hex(written: &str) -> Option<[u8; 3]> {
    let digits = written.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

/// A hue in degrees, and saturation and brightness from 0 to 1.
type Hsv = (f64, f64, f64);

fn to_hsv(color: [u8; 3]) -> Hsv {
    let [r, g, b] = color.map(|channel| channel as f64 / 255.);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let spread = max - min;
    let hue = if spread == 0. {
        0.
    } else if max == r {
        60. * ((g - b) / spread).rem_euclid(6.)
    } else if max == g {
        60. * ((b - r) / spread + 2.)
    } else {
        60. * ((r - g) / spread + 4.)
    };
    let saturation = if max == 0. { 0. } else { spread / max };
    (hue, saturation, max)
}

fn from_hsv((hue, saturation, value): Hsv) -> [u8; 3] {
    let chroma = value * saturation;
    let sector = hue.rem_euclid(360.) / 60.;
    let second = chroma * (1. - (sector.rem_euclid(2.) - 1.).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, second, 0.),
        1 => (second, chroma, 0.),
        2 => (0., chroma, second),
        3 => (0., second, chroma),
        4 => (second, 0., chroma),
        _ => (chroma, 0., second),
    };
    let lift = value - chroma;
    [r, g, b].map(|channel| ((channel + lift) * 255.).round() as u8)
}

/// Builds a color picker over `value`. `changed` hears each color a
/// person chose.
pub fn color_picker(
    value: RwSignal<[u8; 3]>,
    drags: &Drags,
    changed: impl Fn([u8; 3]) + 'static,
) -> El {
    let open = RwSignal::new(false);
    // The color as the panel holds it: a gray has no hue of its own, and
    // the bar must not jump back to red while one is passed through.
    let held = RwSignal::new(to_hsv(value.get_untracked()));
    let changed: Rc<dyn Fn([u8; 3])> = Rc::new(changed);
    let root = el("div").class("color").attr("tabindex", "0");
    let set = move |hsv: Hsv| {
        held.set(hsv);
        let color = from_hsv(hsv);
        if color != value.get_untracked() {
            value.set(color);
            changed(color);
        }
    };
    // The panel shuts when the keys go elsewhere: a press outside it.
    each_layout(move |page| {
        if open.get_untracked() && !page.has_focus_within(root) {
            open.set(false);
            relayout();
        }
    });
    let drags = drags.clone();
    root.on("keydown", move |event| {
        let DomEventData::KeyDown(key) = &event.data else {
            return;
        };
        match (&key.key, open.get_untracked()) {
            (Key::Named(NamedKey::Enter), false) => {
                held.set(to_hsv(value.get_untracked()));
                open.set(true);
            }
            (Key::Named(NamedKey::Escape), true) => {
                open.set(false);
                root.focus();
            }
            _ => return,
        }
        prevent_default();
        stop_propagation();
    })
    .child(
        el("div")
            .class("swatch")
            .style_dyn("background", move || hex(value.get()))
            .on("click", move |_| {
                held.set(to_hsv(value.get_untracked()));
                open.set(!open.get_untracked());
            }),
    )
    .when(
        move || open.get(),
        move || {
            let (area, bar) = (el("div").class("area"), el("div").class("hue"));
            // The saturation and brightness under a point of the page.
            let on_area = {
                let set = set.clone();
                move |x: f32, y: f32| {
                    let set = set.clone();
                    dispatched(move |page| {
                        let Some(bounds) = page.bounds(area) else {
                            return;
                        };
                        let across = ((x as f64 - bounds.x) / bounds.width).clamp(0., 1.);
                        let down = ((y as f64 - bounds.y) / bounds.height).clamp(0., 1.);
                        let (hue, ..) = held.get_untracked();
                        set((hue, across, 1. - down));
                    });
                }
            };
            let on_bar = {
                let set = set.clone();
                move |x: f32| {
                    let set = set.clone();
                    dispatched(move |page| {
                        let Some(bounds) = page.bounds(bar) else {
                            return;
                        };
                        let across = ((x as f64 - bounds.x) / bounds.width).clamp(0., 1.);
                        let (_, saturation, brightness) = held.get_untracked();
                        set((across * 360., saturation, brightness));
                    });
                }
            };
            let (area_drags, bar_drags) = (drags.clone(), drags.clone());
            let written = set.clone();
            let field = el("input").attr("type", "text");
            // The field follows the color, except while it is what is
            // being typed into.
            field.attr_dyn("value", move || hex(value.get()));
            el("div")
                .class("panel")
                .child(
                    area.style_dyn("background", move || hex(from_hsv((held.get().0, 1., 1.))))
                        .child(el("div").class("white"))
                        .child(el("div").class("black"))
                        .child(
                            el("div")
                                .class("knob")
                                .style_dyn("left", move || format!("{}%", held.get().1 * 100.))
                                .style_dyn("top", move || {
                                    format!("{}%", (1. - held.get().2) * 100.)
                                }),
                        )
                        .on("pointerdown", move |event| {
                            let DomEventData::PointerDown(pointer) = &event.data else {
                                return;
                            };
                            on_area(pointer.page_x(), pointer.page_y());
                            area_drags.begin(on_area.clone());
                            prevent_default();
                        }),
                )
                .child(
                    bar.child(
                        el("div")
                            .class("knob")
                            .style_dyn("left", move || format!("{}%", held.get().0 / 3.6)),
                    )
                    .on("pointerdown", move |event| {
                        let DomEventData::PointerDown(pointer) = &event.data else {
                            return;
                        };
                        on_bar(pointer.page_x());
                        let on_bar = on_bar.clone();
                        bar_drags.begin(move |x, _| on_bar(x));
                        prevent_default();
                    }),
                )
                .child(
                    el("div")
                        .class("written")
                        .child(field.on("input", move |event| {
                            let DomEventData::Input(input) = &event.data else {
                                return;
                            };
                            if let Some(color) = parse_hex(&input.value) {
                                written(to_hsv(color));
                            }
                        }))
                        .child(
                            el("div")
                                .class("swatch")
                                .style_dyn("background", move || hex(value.get())),
                        ),
                )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_color_is_written_and_read_back() {
        assert_eq!(hex([0x12, 0xab, 0xff]), "#12abff");
        assert_eq!(parse_hex("#12ABff"), Some([0x12, 0xab, 0xff]));
        assert_eq!(parse_hex(" 000000 "), Some([0, 0, 0]));
        for written in ["", "#12ab", "#12abfg", "#12abff00"] {
            assert_eq!(parse_hex(written), None, "{written:?}");
        }
    }

    #[test]
    fn a_color_goes_to_hue_saturation_and_brightness_and_back() {
        for color in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [18, 171, 255],
            [200, 180, 40],
            [128, 128, 128],
            [0, 0, 0],
            [255, 255, 255],
        ] {
            assert_eq!(from_hsv(to_hsv(color)), color);
        }
        assert_eq!(to_hsv([255, 0, 0]), (0., 1., 1.));
        let (hue, saturation, brightness) = to_hsv([0, 255, 255]);
        assert_eq!((hue, saturation, brightness), (180., 1., 1.));
    }
}
