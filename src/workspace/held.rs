//! The sessions the shell holds, kind by kind, whether a pane shows them
//! or not: closing a pane lets go of its view and keeps its session.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use horizon_workspace::{SessionId, SessionInventory, Workspace};

use crate::plugin::{Running, SessionPlugin};

/// The sessions of `P`'s kind the shell holds.
pub(super) struct Held<P: SessionPlugin> {
    sessions: RefCell<HashMap<SessionId, Rc<P::Session>>>,
}

impl<P: SessionPlugin> Default for Held<P> {
    fn default() -> Self {
        Self {
            sessions: RefCell::default(),
        }
    }
}

impl<P: SessionPlugin> Deref for Held<P> {
    type Target = RefCell<HashMap<SessionId, Rc<P::Session>>>;

    fn deref(&self) -> &Self::Target {
        &self.sessions
    }
}

impl<P: SessionPlugin> Held<P> {
    /// Ends and lets go of the sessions not among `known`.
    pub fn keep_only(&self, known: &HashSet<SessionId>) {
        self.sessions.borrow_mut().retain(|id, session| {
            let kept = known.contains(id);
            if !kept {
                P::end(session);
            }
            kept
        });
    }
}

/// `running`, as sessions of `P`'s kind in an inventory.
pub(crate) fn inventory_of<P: SessionPlugin>(
    running: &[Running],
) -> impl Iterator<Item = (horizon_workspace::SessionKind, SessionId)> + '_ {
    running.iter().map(|running| (P::KIND, running.session))
}

/// Brings `model` to what the runtimes run: `inventory` for every kind,
/// and what each says of where its sessions work and what they came
/// from.
pub(crate) fn take_in(
    model: &mut Workspace,
    inventory: SessionInventory,
    running: &[Running],
) -> Result<(), String> {
    model
        .reconcile_session_inventory(&inventory)
        .map_err(|error| error.to_string())?;
    for running in running {
        if let Some(directory) = &running.directory {
            model.set_session_directory(running.session, directory.clone());
        }
        if let Some(parent) = running.parent {
            model.set_session_parent(running.session, parent);
        }
    }
    Ok(())
}
