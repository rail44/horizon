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
the user approves a `config.write` -- no restart needed. `[provider]`
changes take effect on "Reload Session Runtime" (no full restart needed
either, just a fresh `horizon-sessiond`). `[terminal]`/`[ui]` only take
effect the next time Horizon starts, so tell the user a restart is needed
if you change one of those. There is no `[agent]` section any more -- it
was retired 2026-07-18; tool caps and turn-loop guards are fixed built-in
constants now, not configurable via this file at all.

## `[theme]` -- the seed

Since 2026-07-16 (`docs/theme-design.md`'s "config surface narrowed to the
seed" decision) this section is EXACTLY the seed: three keys here, plus
the six normal `[theme.ansi]` hue slots below. Every other role -- text
colors, semantic colors, surfaces, borders, diff colors, the terminal's
own foreground/background/cursor, the ANSI `black`/`white`/`bright_*`
slots -- derives from the seed; none of them are independently settable
any more. If the user wants a derived value to look different, that means
tuning the seed (`surface_base`/`accent`/`text_contrast`/the six hues), not
looking for a per-role override -- that escape hatch is gone by design.
Setting one of the retired role keys (`text_primary`, `text_muted`,
`text_subtle`, `danger`, `warning`, `success`, `info`, `surface_panel`,
`surface_raised`, `surface_chrome`, `surface_selected`, `border_default`,
`border_subtle`, `diff_added_surface`, `diff_added_text`,
`diff_removed_surface`, `diff_removed_text`, `terminal_foreground`,
`terminal_background`, `terminal_cursor`, `cursor_accent`) is warned about
on stderr as no longer configurable and ignored -- it does not fail the
write or crash Horizon, but double-check before writing one of these; the
user's intent almost certainly needs the seed adjusted instead.

Valid names:

- `surface_base` -- a hex color, the seed's anchor. Its own lightness
  decides the scheme's polarity (dark vs. light) automatically; no
  separate switch.
- `accent` -- the app's one focus/selection accent. Either a hex value or
  one of the six `[theme.ansi]` hue names below (e.g. `accent = "blue"`)
  as a slot reference to that resolved color -- every downstream accent
  derivation is identical either way.
- `text_contrast` -- a number (not a hex string): the WCAG contrast-ratio
  target the derived foreground text meets against `surface_base`,
  clamped to `[4.5, 21.0]`. Defaults to `15` (the built-in dark scheme's
  own measured ratio, so leaving it unset keeps today's appearance). An
  unparsable value (including the wrong TOML type) falls back to the
  default silently, without failing the rest of the file.

Example -- seed-only:

```toml
[theme]
surface_base = "#f6f6f6"
accent = "blue"
text_contrast = 12
```

## `[theme.ansi]` -- the seed's hue set (six slots)

A nested table, same hex format. Only the six normal hue slots are still
independently configurable -- they double as the seed's own hue set:

```
red, green, yellow, blue, magenta, cyan
```

`black`, `white`, and the eight `bright_*` slots are derived-only since
2026-07-16 (`black`/`white` track the resolved background/foreground;
`bright_black` tracks `text_subtle`; the six colored `bright_*` hues and
`bright_white` are a fixed lightness push off their normal counterpart).
Setting any of them is warned about on stderr as no longer configurable
and ignored, same as a retired `[theme]` role key above.

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

**Command ids**: `split-right` (opens the palette's view chooser to split
the active pane horizontally), `split-down` (same chooser, but vertically),
`new-tab` (opens the chooser to open a new tab),
`focus-next-pane`, `close-active-pane`, `close-active-tab`,
`terminate-active-session`, `approve-tool-call`, `deny-tool-call`,
`cancel-agent-turn`, `reload-session-runtime`, `reload-config`. Two reserved
pseudo-command ids are also accepted here even though they are not real
commands: `open-palette` (overrides the chord that opens the command
palette) and `workspace-mode` (overrides the chord that enters workspace
mode, `ctrl+'` by default).

Example:

```toml
[keybindings]
"ctrl+shift+t" = "new-tab"
"ctrl+shift+p" = "open-palette"
"ctrl+'" = "workspace-mode"
```

An unparsable chord or an unrecognized command id is warned about on
stderr and skipped -- it never breaks startup, but double-check spelling
against the list above since a typo just silently does nothing.
