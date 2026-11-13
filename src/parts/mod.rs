//! What views are made of: a chip, a button, a field's box, and the controls a view places (a
//! slider, a select, a color picker). A control is built over a signal of
//! the value it edits, and tells whoever placed it when a person changed
//! it.
//!
//! The controls' structure, behaviour and keys follow the Dioxus components
//! (<https://github.com/DioxusLabs/components>, MIT or Apache-2.0), which
//! are themselves after Radix; they are written over this shell's own
//! layer, and drawn in the theme's variables.

use std::cell::RefCell;
use std::rc::Rc;

use horizon_ui::El;
use horizon_ui::blitz_traits::events::DomEventData;

mod button;
mod chip;
mod color;
mod field;
mod gallery;
mod select;
mod slider;

pub use button::{Button, ButtonKind, ButtonSize, Press};
pub use chip::{Chip, Tone};
pub use color::{color_picker, hex, parse_hex};
pub use field::{field, prompt, prompt_of};
pub use gallery::gallery;
pub use select::select;
pub use slider::{Range, slider};

/// Hears where the pointer is, on the page.
type Moved = Rc<dyn Fn(f32, f32)>;

/// What follows the pointer while a button is held: a thumb being
/// dragged. A part begins a drag on its own element; the moves that
/// follow arrive wherever the pointer is, so the view wires them in at
/// its root.
#[derive(Clone, Default)]
pub struct Drags(Rc<RefCell<Option<Moved>>>);

impl Drags {
    /// From a part: `moved` hears where the pointer is, on the page,
    /// until the button comes up.
    pub fn begin(&self, moved: impl Fn(f32, f32) + 'static) {
        *self.0.borrow_mut() = Some(Rc::new(moved));
    }

    /// From the view: `root`'s pointer moves and releases drive the drag
    /// in progress.
    pub fn wire(&self, root: El) -> El {
        let (moving, ending) = (self.clone(), self.clone());
        root.on("pointermove", move |event| {
            let DomEventData::PointerMove(pointer) = &event.data else {
                return;
            };
            let moved = moving.0.borrow().clone();
            if let Some(moved) = moved {
                moved(pointer.page_x(), pointer.page_y());
            }
        })
        .on("pointerup", move |_| {
            *ending.0.borrow_mut() = None;
        })
    }
}
