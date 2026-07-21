//! Shared pieces of the remoc adoption spike: realistic frame synthesis
//! from the real `horizon-terminal-core` types, a byte-counting IO
//! wrapper for wire measurements, and the V1/V2 type pairs used by the
//! skew tests.

pub mod frames;
pub mod io_count;
pub mod skew;
pub mod svc;
