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

//!
//! Built for wasm, the crate is that layer as a plugin has it
//! ([`guest`]): the same names over the host's document.

#[cfg(not(target_family = "wasm"))]
mod app;
#[cfg(not(target_family = "wasm"))]
mod borders;
#[cfg(not(target_family = "wasm"))]
mod dispatch;
#[cfg(not(target_family = "wasm"))]
mod el;
#[cfg(not(target_family = "wasm"))]
mod frame;
#[cfg(target_family = "wasm")]
pub mod guest;
#[cfg(not(target_family = "wasm"))]
pub mod host;
#[cfg(not(target_family = "wasm"))]
mod input_method;
mod one_of;
#[cfg(not(target_family = "wasm"))]
mod page;
#[cfg(not(target_family = "wasm"))]
mod shared;
#[cfg(not(target_family = "wasm"))]
pub mod testing;
#[cfg(not(target_family = "wasm"))]
pub mod windowed;

#[cfg(not(target_family = "wasm"))]
pub use app::{App, Live};
#[cfg(not(target_family = "wasm"))]
pub use dispatch::{prevent_default, stop_propagation};
#[cfg(not(target_family = "wasm"))]
pub use el::{El, copy, el, request_paste};
#[cfg(not(target_family = "wasm"))]
pub use frame::{each_effects, frame, relayout};
#[cfg(target_family = "wasm")]
pub use guest::{
    Bounds, El, Page, dispatched, each_layout, el, prevent_default, relayout, stop_propagation,
};
#[cfg(not(target_family = "wasm"))]
pub use page::{Bounds, Page, dispatched, each_layout};

pub use one_of::OneOf;

#[cfg(not(target_family = "wasm"))]
pub use blitz_dom;
pub use blitz_traits;
pub use reactive_graph;
