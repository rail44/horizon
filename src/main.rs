use std::io::{self, IsTerminal};
use std::process::ExitCode;

use floem::{window::WindowConfig, AppEvent, Application};
use horizon::{app_view, window_size};

/// Single-binary dispatch (`docs/cli-control-plane-design.md`'s Second
/// revision, "Single binary, subcommand client"): `horizon` with no
/// arguments launches the GUI exactly as before; `horizon <subcommand> ...`
/// runs the control-plane client (the former standalone `horizon-ctl`
/// binary, now `horizon_ctl::run`) and exits without ever touching floem.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if should_run_as_client(&args) {
        return run_client(&args);
    }
    run_gui();
    ExitCode::SUCCESS
}

/// Whether `args` (already stripped of `argv[0]`) selects the control-plane
/// client instead of the GUI: any subcommand at all routes to
/// [`run_client`], exactly like the standalone `horizon-ctl` binary used to;
/// empty `args` launches the GUI exactly as before. Split out so a test can
/// prove the empty-argv case picks the GUI branch without ever constructing
/// a `floem::Application` (no display needed).
fn should_run_as_client(args: &[String]) -> bool {
    !args.is_empty()
}

/// Runs the control-plane client and returns its exit code -- see
/// `horizon_ctl::run`'s doc comment for what the codes mean.
/// `HORIZON_SOCKET`/`HORIZON_SESSION_ID` are the two environment overrides
/// the design doc's Discovery/Placement-vocabulary decisions rely on (both
/// injected into every pane's environment, see `docs/cli-control-plane-
/// design.md`); reading them here (rather than inside `horizon_ctl::run`)
/// keeps that function a pure mapping from its arguments to an exit code.
fn run_client(args: &[String]) -> ExitCode {
    let env_socket = std::env::var("HORIZON_SOCKET").ok();
    let env_session_id = std::env::var("HORIZON_SESSION_ID").ok();
    let stdin_is_tty = io::stdin().is_terminal();
    let code = horizon_ctl::run(
        args,
        env_socket,
        env_session_id,
        &mut io::stdout(),
        &mut io::stderr(),
        stdin_is_tty,
        &mut horizon_ctl::confirm::interactive_prompt,
    );
    ExitCode::from(code)
}

/// The GUI entry point -- unchanged from before this file gained a
/// subcommand branch (see [`main`]), so the no-argument path is exactly as
/// it always was.
fn run_gui() {
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
                    .size(window_size())
                    .show_titlebar(true)
                    .undecorated(false),
            ),
        )
        .run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_selects_the_gui_path() {
        assert!(!should_run_as_client(&[]));
    }

    #[test]
    fn any_subcommand_selects_the_client_path() {
        assert!(should_run_as_client(&["sessions".to_string()]));
        assert!(should_run_as_client(&[
            "--json".to_string(),
            "state".to_string()
        ]));
    }
}
