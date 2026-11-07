//! The Markdown view: a session-less pane that shows one Markdown file
//! from disk, follows its links to other Markdown files in place, and
//! redraws when the file changes.

mod document;
mod view;

pub(crate) use view::{DocumentChanged, MarkdownView};
