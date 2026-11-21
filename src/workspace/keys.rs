//! The shell's keys: the chord that turns workspace mode on and off, and
//! in the mode the keys that move its cursor and act on what it is on
//! (`docs/workspace-mode-design.md`). A direction moves the cursor, and
//! with Shift swaps what the cursor is on with what it would move to.

use horizon_ui::blitz_traits::events::{DomEvent, DomEventData};
use horizon_ui::reactive_graph::traits::{GetUntracked, WithUntracked};
use horizon_ui::{prevent_default, stop_propagation};
use horizon_workspace::commands::CommandId;
use horizon_workspace::{Direction, RESIZE_STEP, SessionKind, SplitAxis, Workspace};
use keyboard_types::{Key, NamedKey};

use super::keymap::Bound;
use super::{Modal, Placement, Shell};

/// A key on its way in to the view with the focus.
pub(super) fn pressed(shell: Shell, event: &DomEvent) {
    let DomEventData::KeyDown(key) = &event.data else {
        return;
    };
    // An open list has the keys.
    if shell.modal.get_untracked().is_some() {
        return;
    }
    let character = match &key.key {
        Key::Character(character) => character.as_str(),
        _ => "",
    };
    let kept = shell.kept();
    let (toggles, bound) = {
        let keymap = kept.keymap.borrow();
        (keymap.workspace_mode.matches(key), keymap.bound(key))
    };
    // A chord bound to a command that cannot be carried out now is the
    // view's key.
    let command = match &bound {
        Some(Bound::Command(name)) => shell.command(name).filter(|command| command.enabled),
        _ => None,
    };
    let bound = bound.filter(|bound| *bound == Bound::OpenPalette || command.is_some());
    if toggles {
        shell.change(|model| match model.is_workspace_mode_active() {
            true => model.cancel_workspace_mode(),
            false => model.enter_workspace_mode(),
        });
    }
    // A chord the configuration binds works in the mode and out of it.
    match (bound.as_ref().filter(|_| !toggles), &command) {
        (Some(Bound::OpenPalette), _) => shell.open(Modal::Palette),
        (Some(Bound::Command(_)), Some(command)) => _ = shell.run(command),
        _ => {}
    }
    if toggles || bound.is_some() {
        prevent_default();
        stop_propagation();
        return;
    }
    if !shell.mode.get_untracked() {
        return;
    }
    // With no tab, what is typed searches the views a tab can show, which
    // the workspace shows then; `:` is still the commands'.
    if shell.model.with_untracked(Workspace::tab_count) == 0 {
        if character == ":" {
            shell.open(Modal::Palette);
            prevent_default();
            stop_propagation();
        }
        return;
    }
    // In the mode, no key is the view's.
    prevent_default();
    stop_propagation();
    if key.modifiers.ctrl() || key.modifiers.alt() || key.modifiers.meta() {
        return;
    }
    let shifted = key.modifiers.shift();
    let found = (MODE.iter())
        .filter(|group| group.here(shell))
        .flat_map(|group| group.bound)
        .find(|(pressed, _)| pressed.is(&key.key, character, shifted));
    if let Some((_, act)) = found {
        act.carry_out(shell);
    }
}

/// What a key does in workspace mode.
#[derive(Clone, Copy)]
enum Act {
    Step(Direction),
    Swap(Direction),
    Resize(SplitAxis, f32),
    Equalize,
    NewTab,
    MoveToTab,
    Split,
    Agent,
    Close,
    Commands,
    Commit,
    Cancel,
    Tab(isize),
}

impl Act {
    fn carry_out(self, shell: Shell) {
        match self {
            Act::Step(direction) => shell.change(|model| model.move_cursor(direction)),
            Act::Swap(direction) => shell.change(|model| _ = model.swap_cursor(direction)),
            Act::Resize(axis, step) => shell.change(|model| _ = model.resize_cursor(axis, step)),
            // (The split around the cursor's part made even, as vim has
            // its windows' with `=`.)
            Act::Equalize => shell.change(|model| _ = model.equalize_cursor()),
            Act::NewTab => shell.execute(CommandId::NewTab),
            Act::MoveToTab => shell.execute(CommandId::MoveToTab),
            Act::Split => shell.execute(CommandId::SplitRight),
            Act::Agent => shell.create(SessionKind::Agent, None, Placement::NewTab),
            Act::Close => shell.close_pane(),
            Act::Commands => shell.open(Modal::Palette),
            Act::Commit => shell.change(Workspace::commit_workspace_mode),
            Act::Cancel => shell.change(Workspace::cancel_workspace_mode),
            Act::Tab(delta) => cycle_tab(shell, delta),
        }
    }
}

/// A key as the mode reads it: a character as it is typed, or a named
/// key with Shift or without.
#[derive(Clone, Copy)]
enum Pressed {
    Character(&'static str),
    Named(NamedKey),
    Shifted(NamedKey),
}

impl Pressed {
    fn is(self, key: &Key, character: &str, shifted: bool) -> bool {
        match (self, key) {
            (Pressed::Character(wanted), Key::Character(_)) => wanted == character,
            (Pressed::Named(wanted), Key::Named(named)) => wanted == *named && !shifted,
            (Pressed::Shifted(wanted), Key::Named(named)) => wanted == *named && shifted,
            _ => false,
        }
    }
}

/// The mode's keys that do one thing, as the bar says them: their
/// spelling, what they do, and each key with what it does. The bar shows
/// them in this order and leaves out the last where they do not fit.
pub(super) struct Group {
    pub(super) keys: &'static str,
    pub(super) label: &'static str,
    bound: &'static [(Pressed, Act)],
}

impl Group {
    /// Whether its keys work in `shell`: a new agent needs the agent
    /// runtime.
    pub(super) fn here(&self, shell: Shell) -> bool {
        let agent = matches!(self.bound, [(_, Act::Agent)]);
        !agent || shell.kept().spawn_agent.is_some()
    }
}

use Act::{Resize, Step, Swap};
use Direction::{Down, Left, Right, Up};
use NamedKey::{ArrowDown, ArrowLeft, ArrowRight, ArrowUp};
use Pressed::{Character as C, Named as N, Shifted as S};

/// Workspace mode's keys. A direction moves the cursor, and with Shift
/// swaps what the cursor is on with what it would move to; the split
/// around the cursor's part grows and shrinks a step at a time, as vim
/// has its windows': `<` `>` across, `-` `+` down, `=` even.
pub(super) const MODE: &[Group] = &[
    Group {
        keys: "hjkl",
        label: "move",
        bound: &[
            (C("h"), Step(Left)),
            (C("j"), Step(Down)),
            (C("k"), Step(Up)),
            (C("l"), Step(Right)),
            (N(ArrowLeft), Step(Left)),
            (N(ArrowDown), Step(Down)),
            (N(ArrowUp), Step(Up)),
            (N(ArrowRight), Step(Right)),
        ],
    },
    Group {
        keys: "enter",
        label: "go",
        bound: &[(N(NamedKey::Enter), Act::Commit)],
    },
    Group {
        keys: "esc",
        label: "back",
        bound: &[(N(NamedKey::Escape), Act::Cancel)],
    },
    Group {
        keys: "t",
        label: "new tab",
        bound: &[(C("t"), Act::NewTab)],
    },
    Group {
        keys: "s",
        label: "split",
        bound: &[(C("s"), Act::Split)],
    },
    Group {
        keys: "x",
        label: "close",
        bound: &[(C("x"), Act::Close)],
    },
    Group {
        keys: "a",
        label: "agent",
        bound: &[(C("a"), Act::Agent)],
    },
    Group {
        keys: "m",
        label: "move to tab",
        bound: &[(C("m"), Act::MoveToTab)],
    },
    Group {
        keys: "tab",
        label: "next tab",
        bound: &[
            (N(NamedKey::Tab), Act::Tab(1)),
            (S(NamedKey::Tab), Act::Tab(-1)),
        ],
    },
    Group {
        keys: ":",
        label: "commands",
        bound: &[(C(":"), Act::Commands)],
    },
    Group {
        keys: "HJKL",
        label: "swap",
        bound: &[
            (C("H"), Swap(Left)),
            (C("J"), Swap(Down)),
            (C("K"), Swap(Up)),
            (C("L"), Swap(Right)),
            (S(ArrowLeft), Swap(Left)),
            (S(ArrowDown), Swap(Down)),
            (S(ArrowUp), Swap(Up)),
            (S(ArrowRight), Swap(Right)),
        ],
    },
    Group {
        keys: "< > - + =",
        label: "resize",
        bound: &[
            (C("<"), Resize(SplitAxis::Horizontal, -RESIZE_STEP)),
            (C(">"), Resize(SplitAxis::Horizontal, RESIZE_STEP)),
            (C("-"), Resize(SplitAxis::Vertical, -RESIZE_STEP)),
            (C("+"), Resize(SplitAxis::Vertical, RESIZE_STEP)),
            (C("="), Act::Equalize),
        ],
    },
];

/// Shows the tab `delta` after the active one, around the strip's ends.
/// The mode stays on.
fn cycle_tab(shell: Shell, delta: isize) {
    let (count, active) = (shell.model).with_untracked(|model| {
        (
            model.tab_count() as isize,
            model.active_tab_index() as isize,
        )
    });
    if count > 1 {
        let next = (active + delta).rem_euclid(count) as usize;
        shell.change(|model| {
            model.activate_tab_index(next);
        });
    }
}
