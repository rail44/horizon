//! The shell's runtime clients (`horizon-shell-runtime`), with the half
//! that is GPUI's own: [`link`] bridges an attachment's channels into GPUI
//! tasks and entities.

mod link;

pub(crate) use horizon_shell_runtime::*;
pub(crate) use link::{event_stream, RuntimeLink};
