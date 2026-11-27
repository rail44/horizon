//! What views are made of: a chip, an icon, a button and a menu button, an
//! alert dialog, a field's box, and the controls a view places (a slider, a select, a color
//! picker). A control is built over a signal of
//! the value it edits, and tells whoever placed it when a person changed
//! it.
//!
//! The controls' structure, behaviour and keys follow the Dioxus components
//! (<https://github.com/DioxusLabs/components>, MIT or Apache-2.0), which
//! are themselves after Radix; they are written over this shell's own
//! layer, and drawn in the theme's variables.

use std::cell::RefCell;
use std::rc::Rc;

use horizon_ui::blitz_traits::events::DomEventData;
use horizon_ui::{El, capture_pointer};

mod alert;
mod button;
mod chip;
mod color;
mod field;
mod gallery;
mod icon;
mod link;
mod list_row;
pub mod markdown;
mod menu;
mod notice;
mod select;
mod slider;

pub use alert::AlertDialog;
pub use button::{Button, ButtonKind, ButtonSize, Press};
pub use chip::{Chip, Tone};
pub use color::{ColorPicker, hex, parse_hex};
pub use field::{Field, Prompt};
pub use gallery::gallery;
pub use icon::Icon;
pub use link::link;
pub use list_row::{ROW_SELECTED, list_row};
pub use menu::{MenuButton, MenuItem, MenuLabel};
pub use notice::Notice;
pub use select::Select;
pub use slider::{Range, Slider};

/// Hears where the pointer is, on the page.
type Moved = Rc<dyn Fn(f32, f32)>;
/// A drag in progress: what hears its moves, and what hears where its
/// button came up.
type Held = (Moved, Option<Moved>);

/// What follows the pointer while a button pressed on an element is held:
/// a thumb being dragged, text being selected. The press captures the
/// pointer for the element (`capture_pointer`), so the moves that follow
/// and the release arrive at the element wherever the pointer is.
#[derive(Clone)]
pub struct Drag {
    element: El,
    held: Rc<RefCell<Option<Held>>>,
}

impl Drag {
    /// Drags begun on `element`: its pointer moves and releases drive the
    /// one in progress.
    pub fn on(element: El) -> Self {
        let held = Rc::<RefCell<Option<Held>>>::default();
        let (moving, ending) = (held.clone(), held.clone());
        element
            .on("pointermove", move |event| {
                let DomEventData::PointerMove(pointer) = &event.data else {
                    return;
                };
                let moved = moving.borrow().clone();
                if let Some((moved, _)) = moved {
                    moved(pointer.page_x(), pointer.page_y());
                }
            })
            .on("pointerup", move |event| {
                let held = ending.borrow_mut().take();
                let (Some((_, Some(ended))), DomEventData::PointerUp(pointer)) =
                    (held, &event.data)
                else {
                    return;
                };
                ended(pointer.page_x(), pointer.page_y());
            });
        Self { element, held }
    }

    /// From the element's press: `moved` hears where the pointer is, on
    /// the page, until the button comes up.
    pub fn begin(&self, moved: impl Fn(f32, f32) + 'static) {
        *self.held.borrow_mut() = Some((Rc::new(moved), None));
        capture_pointer(self.element);
    }

    /// [`begin`](Self::begin), and `ended` hears where the button came
    /// up.
    pub fn begin_until(
        &self,
        moved: impl Fn(f32, f32) + 'static,
        ended: impl Fn(f32, f32) + 'static,
    ) {
        *self.held.borrow_mut() = Some((Rc::new(moved), Some(Rc::new(ended))));
        capture_pointer(self.element);
    }
}
