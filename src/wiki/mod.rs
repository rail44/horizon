//! The wiki view: one page of the project wiki (`horizon-wiki`), drawn as
//! the Markdown view draws a file, with the owner's comment threads
//! anchored in it (docs/project-wiki-design.md, "Comments and the wiki
//! view").

mod model;
mod view;

pub(crate) use view::WikiView;
