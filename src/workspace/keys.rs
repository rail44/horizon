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
    let step = |direction| shell.change(|model| model.move_cursor(direction));
    let swap = |direction| shell.change(|model| _ = model.swap_cursor(direction));
    let resize = |axis, step| shell.change(|model| _ = model.resize_cursor(axis, step));
    let shifted = key.modifiers.shift();
    match &key.key {
        Key::Character(_) => match character {
            "h" => step(Direction::Left),
            "j" => step(Direction::Down),
            "k" => step(Direction::Up),
            "l" => step(Direction::Right),
            "H" => swap(Direction::Left),
            "J" => swap(Direction::Down),
            "K" => swap(Direction::Up),
            "L" => swap(Direction::Right),
            // The split around the cursor's part, a step at a time, as vim
            // has its windows': `<` `>` across, `-` `+` down, `=` even.
            "<" => resize(SplitAxis::Horizontal, -RESIZE_STEP),
            ">" => resize(SplitAxis::Horizontal, RESIZE_STEP),
            "-" => resize(SplitAxis::Vertical, -RESIZE_STEP),
            "+" => resize(SplitAxis::Vertical, RESIZE_STEP),
            "=" => shell.change(|model| _ = model.equalize_cursor()),
            "t" => shell.execute(CommandId::NewTab),
            "s" => shell.execute(CommandId::SplitRight),
            "a" if kept.spawn_agent.is_some() => {
                shell.create(SessionKind::Agent, None, Placement::NewTab);
            }
            "x" => shell.close_pane(),
            ":" => shell.open(Modal::Palette),
            _ => {}
        },
        Key::Named(NamedKey::ArrowLeft) if shifted => swap(Direction::Left),
        Key::Named(NamedKey::ArrowDown) if shifted => swap(Direction::Down),
        Key::Named(NamedKey::ArrowUp) if shifted => swap(Direction::Up),
        Key::Named(NamedKey::ArrowRight) if shifted => swap(Direction::Right),
        Key::Named(NamedKey::ArrowLeft) => step(Direction::Left),
        Key::Named(NamedKey::ArrowDown) => step(Direction::Down),
        Key::Named(NamedKey::ArrowUp) => step(Direction::Up),
        Key::Named(NamedKey::ArrowRight) => step(Direction::Right),
        Key::Named(NamedKey::Enter) => shell.change(Workspace::commit_workspace_mode),
        Key::Named(NamedKey::Escape) => shell.change(Workspace::cancel_workspace_mode),
        Key::Named(NamedKey::Tab) => cycle_tab(shell, if key.modifiers.shift() { -1 } else { 1 }),
        _ => {}
    }
}

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
