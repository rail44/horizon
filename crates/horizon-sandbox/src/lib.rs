//! What the agent's `bash` tool is contained by
//! (`docs/sandbox-runtime-design.md`): the seam to an off-the-shelf sandbox
//! runtime ([`runtime`]) and the filesystem grants a session's contained
//! calls may use beyond their workspace.

mod grant;
pub mod runtime;

pub use grant::{
    FilesystemGrant, FilesystemGrantAccess, FilesystemGrantScope, GrantError, home_dir,
    is_overbroad_tree, revalidate_grant,
};
