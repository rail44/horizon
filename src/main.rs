//! The GPUI shell binary: a wrapper around the `horizon` library, which
//! holds every module and the CLI-vs-GUI entry logic (`horizon::run`).

use std::process::ExitCode;

fn main() -> ExitCode {
    horizon::run()
}
