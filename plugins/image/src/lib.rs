//! The image viewer, an extension: a file of a media type the document
//! decodes, opened by the shell (a link to it followed in a terminal), is
//! shown over the workspace, as large as fits there. The view is `view`.
#![cfg(target_family = "wasm")]

mod view;

#[horizon_ui::plugin(name = "image", title = "Image")]
mod image {
    use horizon_ui::guest::File;

    /// The types blitz-dom decodes, with the formats `horizon-ui` turns
    /// on.
    #[open(types = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/svg+xml"])]
    pub fn open(file: File) {
        let path = file.path();
        horizon_ui::guest::modal(move |_| crate::view::over(&path));
    }
}
