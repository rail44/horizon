---
name: horizon-config
description: Edit Horizon's config.toml to change its color theme or keybindings -- file location, format, precedence, and which sections apply live vs. need a restart.
---

# Horizon configuration file

Horizon reads exactly one optional TOML file. There is no layered
system/user/project merge.

## Location

Resolution order (first one found wins):

1. `$HORIZON_CONFIG`, if set -- an absolute path to any file.
2. `$XDG_CONFIG_HOME/horizon/config.toml`, if `$XDG_CONFIG_HOME` is set.
3. `~/.config/horizon/config.toml` otherwise.

Always call `config.read` first to see the resolved path and current
contents -- never assume it exists or guess its contents. `config.read`
reports `"exists": false` (with the resolved path) when nothing has been
written there yet; that is normal, not an error.

## Precedence and secrets

For any given setting: an environment variable (if one exists for it)
always wins over this file, which always wins over Horizon's built-in
default. Secrets (an API key) are environment-only and never belong in
this file -- never write one there even if the user asks.

## Editing rules

- Always `config.read` before `config.write` in this session, even if you
  already showed the user the contents earlier in the conversation --
  `config.write` refuses a write if the file was not read in this session,
  or if it changed on disk since that read.
- `config.write` takes the **complete** file content and replaces the file
  with it. Always preserve every existing entry the user did not ask you
  to change -- read the current file, edit only the requested section, and
  write the whole thing back. Never write just the section you touched.
- `config.write` validates the content is well-formed TOML before writing
  and reports a parse error (with detail) if not, so you can fix it and
  retry.
- An unrecognized key or an unparsable value inside `[theme]`/
  `[theme.ansi]`/`[keybindings]` does not fail the write or crash
  Horizon -- it is warned about on stderr and that one entry is skipped,
  falling back to its built-in default. Still, prefer getting names and
  syntax right the first time using the reference below.

## Apply timing

`[theme]` and `[keybindings]` changes take effect automatically as soon as
the user approves a `config.write` -- no restart needed. Every other
section (`[agent]`, `[provider]`, `[terminal]`, `[ui]`) only takes effect
the next time Horizon starts, so tell the user a restart is needed if you
change one of those.

## `[theme]` -- named color roles

Values are `#rrggbb` or `#rgb` hex strings (case-insensitive, leading `#`
optional). The terminal is not a separate palette: its foreground,
background, and cursor colors project from `text_primary`, `surface_base`,
and `accent` respectively unless overridden with their own names below, so
changing those three once recolors chrome and the terminal together.

Valid names:

- `text_primary`, `text_muted`, `text_subtle` -- text colors, most to
  least prominent.
- `accent` -- the app's one focus/selection accent.
- `danger` -- destructive-action color (e.g. a "Deny" button).
- `surface_base`, `surface_panel`, `surface_raised`, `surface_chrome`,
  `surface_selected` -- background layers, back to front.
- `border_default`, `border_subtle` -- border colors.
- `cursor_accent` -- workspace mode's cursor-frame border (distinct from
  `accent`, which is the focus border, so both stay visible at once).
- `terminal_foreground` (defaults to `text_primary`), `terminal_background`
  (defaults to `surface_base`), `terminal_cursor` (defaults to `accent`) --
  set one of these only when the terminal should diverge from chrome.

Example:

```toml
[theme]
accent = "#84dcc6"
terminal_cursor = "#84dcc6"
cursor_accent = "#e5c07b"
```

## `[theme.ansi]` -- the 16-slot terminal ANSI palette

A nested table, same hex format, one entry per slot. All 16 must use
these exact names if you set any of them (unset slots keep their built-in
default):

```
black, red, green, yellow, blue, magenta, cyan, white,
bright_black, bright_red, bright_green, bright_yellow,
bright_blue, bright_magenta, bright_cyan, bright_white
```

## `[keybindings]` -- chord string to command id

Each entry is `"<chord>" = "<command-id>"`, layered on top of Horizon's
built-in bindings: an entry bound to a chord already used by a default
overrides it, and a new chord adds a binding. Deleting an entry (or the
whole `[keybindings]` table) reverts that chord to the built-in default.

**Chord syntax**: modifiers joined with `+`, ending in the key itself.
Modifiers: `ctrl`/`control`, `shift`, `alt`/`option`, `meta`/`cmd`/
`command`/`super`/`win`. The final key is either a single character
(`t`, `1`, `'`, ...) or a named key: `enter`, `escape`, `tab`, `space`,
`backspace`, `delete`, `up`/`arrowup`, `down`, `left`, `right`, `home`,
`end`, `pageup`, `pagedown`. Case-insensitive throughout.

**Command ids**: `new-terminal`, `new-agent`, `new-config-agent`,
`split-active-pane`, `focus-next-pane`, `close-active-pane`,
`close-active-tab`, `terminate-active-session`, `approve-tool-call`,
`deny-tool-call`, `cancel-agent-turn`, `reload-agent-runtime`,
`reload-config`. Two reserved pseudo-command ids are also accepted
here even though they are not real commands: `open-palette` (overrides the
chord that opens the command palette) and `workspace-mode` (overrides the
chord that enters workspace mode, `ctrl+'` by default).

Example:

```toml
[keybindings]
"ctrl+shift+t" = "new-terminal"
"ctrl+shift+p" = "open-palette"
"ctrl+'" = "workspace-mode"
```

An unparsable chord or an unrecognized command id is warned about on
stderr and skipped -- it never breaks startup, but double-check spelling
against the list above since a typo just silently does nothing.
