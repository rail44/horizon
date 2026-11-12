//! Horizon's layer between signals and a `blitz-dom` document.
//!
//! A view function runs once and builds its elements, as nodes of the
//! document. What changes afterwards is a closure over signals, run as an
//! effect that writes one thing — a text node's contents, an attribute, a
//! list's children — when a signal it read changes. Nothing is rendered
//! again and nothing is compared, apart from a keyed list's keys.
//!
//! `blitz-dom` owns CSS (Stylo), layout (Taffy), text (Parley), hit
//! testing, focus and event dispatch. This crate is the only code that
//! names its document: a view builds and changes elements through [`El`]
//! and asks what it needs of the layout through a [`Page`]
//! (`docs/ui-foundation-migration-design.md`).

mod app;
mod borders;
mod dispatch;
mod el;
mod frame;
mod input_method;
mod page;
mod shared;
pub mod testing;
pub mod windowed;

pub use app::{App, Live};
pub use dispatch::{prevent_default, stop_propagation};
pub use el::{El, copy, el, request_paste};
pub use frame::{frame, relayout};
pub use page::{Bounds, Page, dispatched, each_layout};

pub use blitz_dom;
pub use blitz_traits;
pub use reactive_graph;
