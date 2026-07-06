//! Shared constants for "outer boundary" spacing: the padding/margin between
//! a view's content and its own bounding edge (a header bar's left/right
//! inset, a composer/banner box's margin off the pane edge, a list row's or
//! modal panel's horizontal inset, ...). Deliberately not for inter-sibling
//! `.gap(...)` between elements in a stack -- that reads as a different kind
//! of spacing and stays ad-hoc per call site.
//!
//! A short, roughly-linear scale rather than named t-shirt sizes tied to a
//! ratio: each constant exists because multiple call sites converged on the
//! same value once the previous, wider ad-hoc numbers were tightened (see
//! the "reduce the owner-reported oversized outer margins" pass that added
//! this module), not because it fits a formula.

pub(crate) const SPACING_XXS: f64 = 3.0;
pub(crate) const SPACING_XS: f64 = 4.0;
pub(crate) const SPACING_SM: f64 = 5.0;
pub(crate) const SPACING_MD: f64 = 6.0;
pub(crate) const SPACING_LG: f64 = 7.0;
