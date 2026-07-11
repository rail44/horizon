pub(crate) mod agent;
pub(crate) mod app;
// The config loader moved to crates/horizon-config (shared with
// shell-gpui); this alias keeps crate::config paths working unchanged.
pub(crate) use horizon_config as config;
pub(crate) mod control_plane;
pub(crate) mod control_surface;
pub(crate) mod plugins;
pub(crate) mod profiling;
pub(crate) mod session;
pub(crate) mod terminal;
pub(crate) mod ui;
pub(crate) mod workspace;

pub use app::shutdown;
pub use app::view::app_view;
pub use app::window_size;
pub use session::SessionId;
