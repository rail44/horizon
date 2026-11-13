//! The agent view over a live session: the session's model adapted into
//! what the view draws whenever it has changed, and what the view asks
//! sent to the session.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use horizon_shell_runtime::AgentCommand;
use horizon_shell_runtime::agent_model::ToolCallIdentity;
use horizon_shell_runtime::agent_view::adapter::SettledUnits;
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::Set;
use horizon_ui::{El, each_layout, relayout};

use super::session::AgentSession;
use super::{Agent, Asked, view};

/// Builds the view over `session`. `switch_model` is called when the
/// model's name is pressed.
pub fn live(session: Rc<AgentSession>, switch_model: impl Fn() + 'static) -> (El, Agent) {
    // The approvals on screen, by the ids the view knows them by.
    let approvals = Rc::new(RefCell::new(HashMap::<String, ToolCallIdentity>::new()));
    let (asking, known) = (session.clone(), approvals.clone());
    // The session as the view shows it: adapted below whenever it changed.
    let shown = RwSignal::new_local(None);
    let (root, agent) = view(shown.into(), move |asked| {
        let identity = |id: &str| known.borrow().get(id).cloned();
        let command = match asked {
            Asked::Send(text) => Some(AgentCommand::Prompt { text }),
            Asked::Approve(id) => identity(&id).map(|identity| AgentCommand::Approve { identity }),
            Asked::Deny(id) => identity(&id).map(|identity| AgentCommand::Deny {
                identity,
                reason: None,
            }),
            Asked::Stop => Some(AgentCommand::Cancel),
            Asked::SwitchModel => {
                switch_model();
                None
            }
        };
        if let Some(command) = command {
            asking.send(command);
        }
    });
    let (mut seen, mut settled) = (0, SettledUnits::default());
    each_layout(move |_| {
        let Some((revision, adapted)) = session.adapted_after(seen, &mut settled) else {
            return;
        };
        seen = revision;
        *approvals.borrow_mut() = adapted.approvals;
        shown.set(Some(adapted.transcript));
        // What it shows now is laid out before the page is painted.
        relayout();
    });
    (root, agent)
}
