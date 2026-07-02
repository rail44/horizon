pub mod agent;
pub mod agent_config;
pub(crate) mod app;
pub mod commands;
pub(crate) mod control_surface;
pub mod fonts;
pub mod input;
pub(crate) mod plugins;
pub mod session;
pub(crate) mod terminal;
pub mod ui;
pub(crate) mod workspace;

pub use app::view::app_view;
