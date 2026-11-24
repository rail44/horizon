//! The shell's own keys, kept as a view keeps its (`horizon_ui::keys`),
//! for the status bar (board #84), and the place at the bottom of the
//! window where the extension named `status-bar` shows it.
//!
//! The shell's root keeps workspace mode's keys while it is on, and
//! otherwise the chords the configuration binds: the focus is in the root
//! in the mode, and the root is around every view, so they are the last
//! of what the bar reads. A sheet over the workspace takes every key
//! (`over`), so under one the bar reads no further.

use std::rc::Rc;

use horizon_ui::blitz_dom::NodeId;
use horizon_ui::keys::{self, Kept};
use horizon_ui::reactive_graph::effect::Effect;
use horizon_ui::reactive_graph::owner::Owner;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, GetUntracked, Set, Update, WithUntracked};
use horizon_ui::{El, each_layout};
use horizon_workspace::Workspace;

use super::keymap::Bound;
use super::keys::MODE;
use super::{Shell, plugins};
use crate::extension::Running;
use crate::plugin_host::Asks;

/// The extension the status bar is.
const STATUS_BAR: &str = "status-bar";

/// The status bar shown: its view, the extension running it, and what it
/// runs under, which ends it.
pub(super) struct Shown {
    root: El,
    owner: Owner,
    _running: Rc<Running>,
}

fn kept(spelled: &str, label: &str, typed: bool) -> Kept {
    Kept {
        spelled: spelled.to_string(),
        label: label.to_string(),
        typed,
    }
}

impl Shell {
    /// Keeps the shell's own keys on `root` as they are after each
    /// layout: the mode's, or the chords, of which a command's can be
    /// carried out or not as anything changes.
    pub(super) fn say_own_keys(&self, root: El) -> El {
        let shell = *self;
        let said = RwSignal::new(String::new());
        each_layout(move |_| {
            let now = keys::written(&shell.own_keys());
            if said.with_untracked(|was| *was != now) {
                said.set(now);
            }
        });
        root.attr_dyn(keys::ATTRIBUTE, move || said.get())
    }

    /// The shell's own keys now.
    fn own_keys(&self) -> Vec<Kept> {
        if self.mode.get_untracked() {
            // With no pane, what is typed searches the views the workspace
            // shows; `:` is still the commands'.
            if self.model.with_untracked(Workspace::is_empty) {
                return vec![kept(":", "commands", true)];
            }
            return (MODE.iter())
                .filter(|group| group.here(*self))
                .map(|group| kept(group.keys, group.label, false))
                .collect();
        }
        self.chords()
    }

    /// The chords the configuration binds that work now: workspace mode's,
    /// and those of the commands that can be carried out, each command
    /// once.
    fn chords(&self) -> Vec<Kept> {
        let kept_by = self.kept();
        let keymap = kept_by.keymap.borrow();
        let mode = &keymap.workspace_mode;
        let mut chords = vec![kept(&mode.label(), "workspace mode", mode.typed())];
        let mut said: Vec<&Bound> = Vec::new();
        for (chord, bound) in keymap.bindings() {
            if said.contains(&bound) {
                continue;
            }
            let label = match bound {
                Bound::OpenPalette => "commands".to_string(),
                Bound::Command(name) => match self.command(name) {
                    Some(command) if command.enabled => command.title.to_lowercase(),
                    _ => continue,
                },
            };
            said.push(bound);
            chords.push(kept(&chord.label(), &label, chord.typed()));
        }
        chords
    }

    /// Shows the status bar again where `names` has the extension it is:
    /// one read again, or taken in place of the one of its name. (It is
    /// shown by an effect: this is told after a layout, where a plugin may
    /// not change the document.)
    pub(super) fn remake_status_bar(&self, names: &[&str]) {
        if names.contains(&STATUS_BAR) {
            self.status_asked
                .update(|asked| *asked = asked.wrapping_add(1));
        }
    }

    /// Shows the status bar each time it is asked to be shown again.
    pub(super) fn follow_status_bar(&self) {
        let shell = *self;
        Effect::new(move |_| {
            if shell.status_asked.get() > 0 {
                shell.show_status_bar();
            }
        });
    }

    /// Shows the status bar the extension of its name has, in place of the
    /// one shown.
    fn show_status_bar(&self) {
        let kept = self.kept();
        let Some(place) = kept.status_place.get() else {
            return;
        };
        // (The one shown ends before its view goes: its work would go on
        // into nodes that are gone.)
        let gone = kept.status_bar.borrow_mut().take();
        if let Some(shown) = gone {
            shown.owner.cleanup();
            horizon_ui::host::remove(shown.root.0);
        }
        let Some(extension) = plugins::extension(STATUS_BAR) else {
            return;
        };
        let owner = kept.owner.child();
        let shown = owner.with(|| {
            let failed = RwSignal::new(None::<String>);
            let shown = extension.show_status_bar(Asks::default(), kept.wake.clone(), failed);
            shown.map(|(running, node)| {
                let running = Rc::new(running);
                running.follow();
                (running, node)
            })
        });
        match shown {
            Ok((running, node)) => {
                let root = El(NodeId::from_u64(node));
                place.child(root);
                *kept.status_bar.borrow_mut() = Some(Shown {
                    root,
                    owner,
                    _running: running,
                });
            }
            Err(error) => {
                owner.cleanup();
                eprintln!("horizon: {STATUS_BAR}: {error}");
            }
        }
    }
}
