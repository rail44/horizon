//! The shared visual vocabulary: the live font size ([`font`]), the type
//! scale and rhythm every view draws to ([`spec`], scaled from that size),
//! and the generic parts built from them ([`parts`]: chips, running text,
//! the fade over folded text).
//!
//! Anything with a domain's structure — a post, a list row, a tool
//! receipt — does not belong here: it lives next to its view and draws from
//! this module.
//!
//! Builds for both targets: a preview plugin's views draw from the same
//! scale, with the shell's live size installed through the preview pipe.

pub(crate) mod font;
pub(crate) mod parts;
pub(crate) mod spec;
