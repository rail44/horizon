//! A slider: one value in a range, moved by pressing or dragging along
//! its track, or from the keyboard.

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set};
use horizon_ui::{El, dispatched, el, prevent_default};
use keyboard_types::{Key, NamedKey};

use super::Drags;

pub(super) const CSS: &str = "
    .slider { position: relative; display: flex; align-items: center; min-width: 120px;
              padding: 8px 0; outline: none; cursor: pointer;
        .track { position: relative; flex: 1; height: 6px; border-radius: 3px;
                 background: var(--surface-chrome); }
        .range { position: absolute; left: 0; top: 0; bottom: 0; border-radius: 3px;
                 background: var(--accent); }
        .thumb { position: absolute; top: -5px; width: 16px; height: 16px;
                 margin-left: -8px; box-sizing: border-box; border-radius: 50%;
                 background: var(--background); border: 1px solid var(--accent); }
    }
    .slider:focus .thumb { border-width: 2px; }
";

/// The values a slider takes: from `min` to `max`, in steps of `step`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl Range {
    /// `raw` brought into the range, on a step.
    fn snapped(&self, raw: f64) -> f64 {
        let steps = ((raw - self.min) / self.step).round();
        (self.min + steps * self.step).clamp(self.min, self.max)
    }

    /// Where `value` is along the track, from 0 to 1.
    fn share(&self, value: f64) -> f64 {
        ((value - self.min) / (self.max - self.min)).clamp(0., 1.)
    }
}

/// Builds a slider over `value`. `changed` hears each value a person
/// moved it to.
pub fn slider(
    value: RwSignal<f64>,
    range: Range,
    drags: &Drags,
    changed: impl Fn(f64) + Clone + 'static,
) -> El {
    let track = el("div").class("track");
    let moved = {
        let changed = changed.clone();
        move |to: f64| {
            let to = range.snapped(to);
            if to != value.get_untracked() {
                value.set(to);
                changed(to);
            }
        }
    };
    // The value under a point of the page, along the track.
    let toward = {
        let moved = moved.clone();
        move |x: f32| {
            let moved = moved.clone();
            dispatched(move |page| {
                let Some(bounds) = page.bounds(track) else {
                    return;
                };
                let share = ((x as f64 - bounds.x) / bounds.width).clamp(0., 1.);
                moved(range.min + share * (range.max - range.min));
            });
        }
    };
    let percent = move || format!("{}%", range.share(value.get()) * 100.);
    let drags = drags.clone();
    el("div")
        .class("slider")
        .attr("tabindex", "0")
        .on("pointerdown", move |event| {
            let DomEventData::PointerDown(pointer) = &event.data else {
                return;
            };
            toward(pointer.page_x());
            let toward = toward.clone();
            drags.begin(move |x, _| toward(x));
            // Not the start of a selection in the text around it.
            prevent_default();
        })
        // An arrow is a step, and ten with Shift; Page Up and Down are
        // ten; Home and End are the ends.
        .on("keydown", move |event| {
            let DomEventData::KeyDown(key) = &event.data else {
                return;
            };
            let steps = if key.modifiers.shift() { 10. } else { 1. };
            let now = value.get_untracked();
            let to = match &key.key {
                Key::Named(NamedKey::ArrowRight | NamedKey::ArrowUp) => now + range.step * steps,
                Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowDown) => now - range.step * steps,
                Key::Named(NamedKey::PageUp) => now + range.step * 10.,
                Key::Named(NamedKey::PageDown) => now - range.step * 10.,
                Key::Named(NamedKey::Home) => range.min,
                Key::Named(NamedKey::End) => range.max,
                _ => return,
            };
            moved(to);
            prevent_default();
        })
        .child(
            track
                .child(el("div").class("range").style_dyn("width", percent))
                .child(el("div").class("thumb").style_dyn("left", percent)),
        )
}
