mod frames;
mod registry;

pub(crate) use frames::Frames;
pub(crate) use registry::Registry;

// The id newtype moved to the shared workspace-model crate
// (`docs/gpui-migration-design.md`); this re-export keeps every
// `crate::session::SessionId` path in the shell working unchanged.
pub use horizon_workspace::SessionId;
