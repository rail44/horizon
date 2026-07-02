use floem::{window::WindowConfig, Application};
use horizon::app_view;

fn main() {
    Application::new()
        .window(
            |_| app_view(),
            Some(
                WindowConfig::default()
                    .title("Horizon")
                    .size((1100.0, 720.0))
                    .show_titlebar(true)
                    .undecorated(false),
            ),
        )
        .run();
}
