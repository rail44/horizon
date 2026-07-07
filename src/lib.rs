pub(crate) mod agent;
pub(crate) mod app;
pub(crate) mod config;
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
