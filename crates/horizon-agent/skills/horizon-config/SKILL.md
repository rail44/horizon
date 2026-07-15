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

### Seed + derivation

Any role below left unset is no longer a flat built-in constant -- it
derives from a small seed: `surface_base` (the anchor -- its own lightness
decides dark-vs-light polarity automatically), the six normal
`[theme.ansi]` hues (`red`/`green`/`yellow`/`blue`/`magenta`/`cyan`, which
double as the seed's hue set), `accent`, and `text_contrast` (below). Set
just those and every other role -- `text_primary`, `text_muted`,
`surface_panel`/`surface_raised`/`surface_chrome`/`surface_selected`,
`border_default`, `danger`/`warning`/`success`/`info`, even the ANSI
`black`/`white`/`bright_*` slots -- derives a readable, coherent scheme.
Any role key still set explicitly wins outright, unchanged; the seed only
fills gaps. `[theme.ansi]` itself is never auto-adjusted for the
terminal -- an explicit ANSI slot is always emitted verbatim, even when
the UI-side color derived from that same hue (e.g. `danger` from `red`) is
contrast-snapped for readability.

Valid names:

- `text_primary`, `text_muted`, `text_subtle` -- text colors, most to
  least prominent. Unset, `text_primary` solves for `text_contrast`'s
  ratio against `surface_base`; `text_muted` solves for a ratio between
  the WCAG 4.5 floor and that target; `text_subtle` is decorative (no
  floor, just visual separation).
- `accent` -- the app's one focus/selection accent. Either a hex value or
  one of the six hue names above (e.g. `accent = "blue"`) as a slot
  reference to that resolved `[theme.ansi]` color -- every downstream
  accent derivation is identical either way.
- `text_contrast` -- a number (not a hex string): the WCAG contrast-ratio
  target for `text_primary` against `surface_base`, clamped to
  `[4.5, 21.0]`. Defaults to `15` (the built-in dark scheme's own measured
  ratio, so leaving it unset keeps today's appearance). An unparsable
  value (including the wrong TOML type) falls back to the default
  silently, without failing the rest of the file.
- `danger`, `warning`, `success`, `info` -- semantic colors (errors,
  tool-call requests/pending approval, finished tool-call results, the
  assistant message label). Unset, each derives from the matching seed
  hue (red/yellow/green/blue) snapped to a readable lightness.
- `surface_base`, `surface_panel`, `surface_raised`, `surface_chrome`,
  `surface_selected` -- background layers: the app base (the seed
  anchor), a lifted panel, an elevated surface (popover/dropdown-menu
  chrome), the tab strip's own chrome background, and the command palette
  / session manager / view chooser row highlight. Unset, the last four
  step between `surface_base` and the resolved foreground.
- `border_default`, `border_subtle` -- the chrome separator-line color;
  `border_subtle` is only ever read as a fallback when `border_default`
  is unset.
- `diff_added_surface`, `diff_added_text`, `diff_removed_surface`,
  `diff_removed_text` -- the agent transcript's fs.edit diff rendering.
- `terminal_foreground` (defaults to `text_primary`), `terminal_background`
  (defaults to `surface_base`), `terminal_cursor` (defaults to `accent`) --
  set one of these only when the terminal should diverge from chrome.

Also a valid name but not yet read by any code: `cursor_accent`, planned
as workspace mode's cursor-frame border, distinct from `accent`'s focus
border -- today the cursor frame reuses `accent` outright, so setting
`cursor_accent` alone has no visible effect.

Example -- override just two roles directly:

```toml
[theme]
accent = "#84dcc6"
terminal_cursor = "#84dcc6"
```

Example -- seed-only, derive the rest (relies on `[theme.ansi]`'s six
hues below for its hue set):

```toml
[theme]
surface_base = "#f6f6f6"
accent = "blue"
text_contrast = 12
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
