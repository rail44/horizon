//! Re-export of [`crate::socket::default_socket_path`], kept at this path so
//! existing host-side callers (`super::listener`, the `horizon` binary) are
//! unaffected by the formula's move to the crate root -- see that module's
//! doc comment for why it moved.

pub use crate::socket::default_socket_path;
