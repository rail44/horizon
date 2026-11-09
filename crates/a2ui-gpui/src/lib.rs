//! Spike: render [A2UI](https://a2ui.org/) v1.0 surfaces with GPUI.
//!
//! The crate has two halves:
//!
//! - a toolkit-free model ([`protocol`], [`Surfaces`], [`Surface`]) that
//!   parses agent-to-renderer messages, keeps each surface's component map
//!   and data model, resolves `@path` bindings and list templates, and builds
//!   renderer-to-agent `action` messages;
//! - a GPUI view ([`SurfaceView`]) that draws one surface with
//!   gpui-component widgets, writes input back into the data model (two-way
//!   binding), and emits an [`ActionEvent`] when a component's `event`
//!   action fires.
//!
//! Scope is the minimal round trip of the basic catalog: `Text`, `Row`,
//! `Column`, `List`, `Card`, `Button`, `TextField` and `CheckBox`, plus data
//! binding, list templates, `event` actions and `updateDataModel`. Function
//! calls (`@call`, `functionCall`, `callRendererFunction`), `checks`, the
//! remaining components and catalog negotiation are out of scope; a component
//! the renderer does not know is drawn as a labelled placeholder rather than
//! failing the surface.

mod pointer;
pub mod protocol;
mod render;
mod surface;
mod time;

pub use render::{ActionEvent, SurfaceView};
pub use surface::{Surface, SurfaceError, Surfaces};
pub use time::rfc3339_utc;
