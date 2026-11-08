//! The project wiki as an extension package (docs/project-wiki-design.md):
//! a human-facing view of the project's current state, one page per concept
//! plus an overview, kept outside the repository, with comment threads an
//! agent answers. This crate holds the store, pages, the concept file, the
//! logical text comments are anchored in, the comment log, and the skill
//! that maintains the wiki; it depends on neither `horizon-agent` nor gpui.

pub mod anchor;
pub mod annotation;
pub mod concepts;
pub mod logical;
pub mod page;
pub mod skill;
pub mod store;
pub mod thread;

pub use page::{Edit, PageInfo};
pub use store::Store;
pub use thread::Thread;

#[derive(Debug, thiserror::Error)]
pub enum WikiError {
    #[error("{0} does not exist")]
    NotFound(String),
    #[error("invalid name {0}")]
    InvalidName(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, WikiError>;
