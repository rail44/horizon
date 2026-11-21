//! The keys a view takes, each with what it does: what an element hears
//! (`keydown`) and what the bar at the bottom of the window says while
//! the focus is in the element (board #84), written once. The element
//! keeps them in an attribute ([`ATTRIBUTE`]), so the shell reads a
//! plugin's view's keys out of the document as it reads its own.
//!
//! A key that is a character with no modifier but Shift is typed, not
//! pressed, where the focus is in a text field: it is not heard there,
//! and the bar leaves out a group of keys that has one. An element that
//! takes every key ([`El::keys_alone`], a dialog) is the last the bar
//! reads, and the first of the keys spelled the same is the one it says.

use std::rc::Rc;

use blitz_traits::events::{DomEvent, DomEventData};

use crate::{Chord, El, prevent_default, stop_propagation};

/// The attribute an element keeps its keys in: a line a key, its spelling,
/// what it does, and whether it is typed (`1`) or not (`0`), a tab
/// between them.
pub const ATTRIBUTE: &str = "data-keys";

/// The attribute an element that takes every key has.
pub const ALONE: &str = "data-keys-alone";

type Run = Rc<dyn Fn() -> bool>;

/// A key a view takes, or keys that do one thing between them (`hjkl`,
/// `↑/↓`), and what it does in a few words.
pub struct Key {
    spelled: String,
    label: String,
    bound: Vec<(Chord, Run)>,
}

impl Key {
    /// The key `chord` (`j`, `enter`, `ctrl+shift+c`), spelled as it is
    /// written, doing `label` by `run`.
    pub fn new(chord: &str, label: &str, run: impl Fn() + 'static) -> Self {
        Self::when(chord, label, move || {
            run();
            true
        })
    }

    /// [`Key::new`], with `run` saying whether it took the key: one it
    /// did not take goes on to what is around the element.
    pub fn when(chord: &str, label: &str, run: impl Fn() -> bool + 'static) -> Self {
        Self::group(chord, label).bind_when(chord, run)
    }

    /// Keys spelled `spelled` that do `label` between them, each bound
    /// after ([`Key::bind`]).
    pub fn group(spelled: &str, label: &str) -> Self {
        Self {
            spelled: spelled.to_string(),
            label: label.to_string(),
            bound: Vec::new(),
        }
    }

    /// One of the group's keys, `chord`, and what it does.
    pub fn bind(self, chord: &str, run: impl Fn() + 'static) -> Self {
        self.bind_when(chord, move || {
            run();
            true
        })
    }

    /// [`Key::bind`], with `run` saying whether it took the key.
    pub fn bind_when(mut self, chord: &str, run: impl Fn() -> bool + 'static) -> Self {
        let chord = Chord::parse(chord).unwrap_or_else(|| panic!("{chord:?} is not a chord"));
        self.bound.push((chord, Rc::new(run)));
        self
    }

    /// Whether one of its keys is typed in a text field.
    fn typed(&self) -> bool {
        self.bound.iter().any(|(chord, _)| chord.typed())
    }
}

/// A key an element keeps, as the bar reads it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Kept {
    pub spelled: String,
    pub label: String,
    /// Typed, not pressed, in a text field.
    pub typed: bool,
}

/// The keys an element keeps in [`ATTRIBUTE`], in their order.
pub fn kept(attribute: &str) -> Vec<Kept> {
    (attribute.lines())
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(Kept {
                spelled: fields.next()?.to_string(),
                label: fields.next()?.to_string(),
                typed: fields.next()? == "1",
            })
        })
        .collect()
}

impl El {
    /// The keys this element takes while the focus is in it, the most
    /// wanted first: the bar leaves out the last where they do not fit.
    /// Once an element.
    pub fn keys(self, keys: Vec<Key>) -> Self {
        self.keys_with(keys, false)
    }

    /// [`El::keys`] for an element that takes every key while the focus is
    /// in it, as a dialog does: what is behind it hears none, and the bar
    /// says none of what is behind it takes.
    pub fn keys_alone(self, keys: Vec<Key>) -> Self {
        self.attr(ALONE, "").keys_with(keys, true)
    }

    fn keys_with(self, keys: Vec<Key>, alone: bool) -> Self {
        let written: Vec<String> = (keys.iter())
            .map(|key| {
                let typed = if key.typed() { "1" } else { "0" };
                format!("{}\t{}\t{typed}", key.spelled, key.label)
            })
            .collect();
        let typed: Vec<(Chord, Run)> = (keys.into_iter()).flat_map(|key| key.bound).collect();
        self.attr(ATTRIBUTE, &written.join("\n"))
            .on("keydown", move |event: &DomEvent| {
                let DomEventData::KeyDown(pressed) = &event.data else {
                    return;
                };
                if pressed.is_composing {
                    return;
                }
                let found = (typed.iter())
                    .filter(|(chord, _)| chord.matches(pressed))
                    .find(|(chord, _)| !(chord.typed() && crate::is_text_field(event.target)));
                let taken = found.is_some_and(|(_, run)| run());
                if taken || alone {
                    prevent_default();
                    stop_propagation();
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Kept, kept};

    #[test]
    fn an_elements_keys_are_read_back_in_their_order() {
        let read = kept("j/k\tmove\t1\nenter\tgo to\t0\nnot a key");
        assert_eq!(
            read,
            [
                Kept {
                    spelled: "j/k".into(),
                    label: "move".into(),
                    typed: true,
                },
                Kept {
                    spelled: "enter".into(),
                    label: "go to".into(),
                    typed: false,
                },
            ]
        );
    }
}
