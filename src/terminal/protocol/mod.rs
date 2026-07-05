//! Ownership home for terminal input-encoding protocols. Today that's just
//! the Kitty keyboard protocol (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>);
//! see `kitty_keyboard`'s module doc for why Horizon owns this outright
//! rather than leaning on termwiz's (broken, at Horizon's pin) built-in
//! support, and for the resident `KITTY_COMPLIANCE` table that answers
//! "where are we against the spec" from the terminal
//! (`cargo test print_compliance_matrix -- --nocapture`).

pub(crate) mod kitty_keyboard;
