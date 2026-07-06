//! `horizon-ctl`: hand-rolled CLI client for Horizon's Unix-socket control
//! plane -- see `docs/cli-control-plane-design.md`. All real logic lives in
//! the `horizon_ctl` library crate ([`horizon_ctl::run`]); this binary only
//! wires up real argv/env/stdio and never talks to the real Horizon in a
//! test (that's `tests/integration.rs`'s stub server, driven through the
//! same [`horizon_ctl::run`] entry point).

use std::io::{self, IsTerminal};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env_socket = std::env::var("HORIZON_SOCKET").ok();
    let stdin_is_tty = io::stdin().is_terminal();

    let code = horizon_ctl::run(
        &args,
        env_socket,
        &mut io::stdout(),
        &mut io::stderr(),
        stdin_is_tty,
        &mut horizon_ctl::confirm::interactive_prompt,
    );
    ExitCode::from(code)
}
