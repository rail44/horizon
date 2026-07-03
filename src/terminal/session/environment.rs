use std::env;

use portable_pty::CommandBuilder;

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

pub(crate) fn terminal_command(shell: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(shell);
    configure_terminal_environment(&mut cmd);
    cmd
}

fn configure_terminal_environment(cmd: &mut CommandBuilder) {
    for key in TERMINAL_ENV_REMOVE {
        cmd.env_remove(key);
    }
    cmd.env("TERM", "xterm-kitty");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "horizon");
    cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
}
