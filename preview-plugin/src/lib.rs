//! The preview plugin: a `wasm32-wasip2` component carrying every preview
//! in `horizon::preview::registry`.
//!
//! There is nothing to add here when a preview is added — register it next
//! to the view it shows and rebuild this crate. The host selects which one
//! to mount by name.

embedded_gpui::register_plugin!(horizon::preview::guest::PreviewGuest);
