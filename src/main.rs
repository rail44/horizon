use floem::{window::WindowConfig, AppEvent, Application};
use horizon::app_view;

fn main() {
    Application::new()
        // Flush buffered runtime state (the agent event log's writer
        // thread — see `horizon::shutdown`) on a normal exit. `main`
        // returning doesn't drop the process-global writer static, so
        // without this hook whatever's still sitting in its buffer at
        // shutdown is silently lost instead of merely torn.
        .on_event(|event| {
            if matches!(event, AppEvent::WillTerminate) {
                horizon::shutdown();
            }
        })
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
