use std::env;
use std::path::Path;

use portable_pty::CommandBuilder;

use crate::session::SessionId;

const TERMINAL_ENV_REMOVE: &[&str] = &[
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "GHOSTTY_BIN_DIR",
    "GHOSTTY_RESOURCES_DIR",
    "GHOSTTY_SHELL_INTEGRATION_NO_SUDO",
    "GHOSTTY_SHELL_INTEGRATION_XDG_DIR",
    "KITTY_INSTALLATION_DIR",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "KITTY_WINDOW_ID",
    "WEZTERM_CONFIG_FILE",
    "WEZTERM_EXECUTABLE",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "VTE_VERSION",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "KONSOLE_PROFILE_NAME",
    "KONSOLE_VERSION",
    "TERM_SESSION_ID",
    "WT_PROFILE_ID",
    "WT_SESSION",
    "TMUX",
    "TMUX_PANE",
    "STY",
    "WINDOW",
    "SSH_TTY",
    "DESKTOP_STARTUP_ID",
    "XDG_ACTIVATION_TOKEN",
];

pub(crate) fn terminal_command(
    shell: &str,
    args: &[String],
    term: &str,
    session_id: SessionId,
    cwd: &Path,
) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    cmd.args(args);
    cmd.cwd(cwd);
    configure_terminal_environment(&mut cmd, term, session_id);
    cmd
}

fn configure_terminal_environment(cmd: &mut CommandBuilder, term: &str, session_id: SessionId) {
    for key in TERMINAL_ENV_REMOVE {
        cmd.env_remove(key);
    }
    cmd.env("TERM", term);
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "horizon");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    // `docs/cli-control-plane-design.md`'s "Discovery" decision: every pane
    // is born pointed at the control socket of the Horizon instance it's
    // running inside, so a CLI invoked from a shell in this terminal
    // defaults to targeting *this* instance rather than requiring an
    // explicit `--socket`/env override.
    cmd.env(
        "HORIZON_SOCKET",
        crate::control_plane::default_socket_path(),
    );
    // The Second revision's "Placement vocabulary" decision: this pane's own
    // stable external reference, so a CLI invoked from this shell can
    // resolve `--split`'s bare "here" form without the caller ever having to
    // name a session id itself.
    cmd.env("HORIZON_SESSION_ID", session_id.as_uuid().to_string());
}
