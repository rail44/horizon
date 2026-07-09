/// A cell's logical color: one of the 16 base ANSI slots, a default-role
/// slot (foreground/background/cursor), an xterm 256-color palette index, or
/// a literal 24-bit truecolor value. Reused directly from
/// `alacritty_terminal` rather than re-declared, since a cell's color
/// already arrives in exactly this shape from the VT parser and no
/// conversion buys anything here.
///
/// `docs/session-daemon-design.md` decision 8: this is what actually crosses
/// the `TerminalFrame`/`TerminalSpan` boundary now, instead of a resolved
/// `[u8; 3]` RGB triple — resolving a logical color against a theme (the app
/// default, or in the future a per-client one) is the UI's job
/// (`terminal::view`), not this crate's. One consequence: a color a
/// terminal app redefined at runtime via OSC 4/10/11/12 (`Term::colors()`'s
/// live per-session overrides) no longer affects cell *rendering* once it
/// crosses this boundary — only `TerminalCore`'s own OSC 4/10/11/12 *query
/// replies* (`core::color::resolve_query_color`, answered from inside this
/// crate, which still has access to that live state) still honor it. See
/// that narrowing recorded in the design doc.
pub use alacritty_terminal::vte::ansi::Color as TerminalColor;
