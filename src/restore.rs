//! What the shell opens with: the workspace kept from the last run, as
//! far as the two runtimes still have its sessions.

use horizon_workspace::store::{InvalidState, LoadResult, WorkspaceStateStore};
use horizon_workspace::{
    SessionId, SessionInventory, SessionKind, WORKSPACE_STATE_VERSION, Workspace,
};

use crate::agent::plugin::AgentPlugin;
use crate::agentd::Agentd;
use crate::plugin::SessionPlugin;
use crate::terminal::Session;
use crate::terminal::plugin::TerminalPlugin;
use crate::terminald::Terminald;
use crate::workspace::held::{inventory_of, take_in};
use crate::workspace::{Opening, Save};

/// The kept workspace, if there is one to go on from.
enum Kept {
    /// A workspace to restore.
    Workspace(Workspace),
    /// None, or one that cannot be read: a new one takes its place.
    Nothing,
    /// One this build must not write over: of another version, or
    /// unreadable for a reason that may pass.
    Untouchable,
}

fn kept(store: &mut WorkspaceStateStore) -> Kept {
    let unsupported = |found: u64, supported: u64| {
        eprintln!(
            "workspace state version {found} is unsupported (expected {supported}); preserving the file"
        );
        Kept::Untouchable
    };
    match store.load(u64::from(WORKSPACE_STATE_VERSION)) {
        Ok(LoadResult::Valid(json)) => match Workspace::from_persisted_json(&json) {
            Ok(workspace) => Kept::Workspace(workspace),
            Err(horizon_workspace::WorkspaceStateError::UnsupportedVersion {
                found,
                supported,
            }) => unsupported(u64::from(found), u64::from(supported)),
            Err(error) => {
                eprintln!("ignoring invalid workspace state: {error}");
                Kept::Nothing
            }
        },
        Ok(LoadResult::Missing) => Kept::Nothing,
        Ok(LoadResult::Invalid(InvalidState::UnsupportedVersion { found, supported })) => {
            unsupported(found, supported)
        }
        Ok(LoadResult::Invalid(InvalidState::Corrupt(error))) => {
            eprintln!("ignoring corrupt workspace state: {error}");
            Kept::Nothing
        }
        Err(error) => {
            eprintln!("failed to load workspace state: {error}");
            Kept::Untouchable
        }
    }
}

/// The workspace in `store` with the sessions the runtimes still run
/// attached to again, kept in `store` as it changes. A session its
/// runtime no longer has leaves the workspace with its pane, and one the
/// workspace did not know arrives detached. With nothing kept, one new
/// terminal. Without an agent runtime, the agent sessions leave.
pub fn opening(
    mut store: WorkspaceStateStore,
    terminald: &Terminald,
    agentd: Option<&Agentd>,
) -> Opening {
    let save = |mut store: WorkspaceStateStore| -> Option<Save> {
        Some(Box::new(move |model| {
            let saved = (model.to_persisted_json().map_err(|error| error.to_string()))
                .and_then(|json| store.save(&json).map_err(|error| error.to_string()));
            if let Err(error) = saved {
                eprintln!(
                    "failed to save workspace state to {}: {error}",
                    store.path().display()
                );
            }
        }))
    };
    let mut model = match kept(&mut store) {
        Kept::Workspace(model) => model,
        Kept::Nothing => {
            return Opening {
                save: save(store),
                ..Opening::default()
            };
        }
        Kept::Untouchable => return Opening::default(),
    };
    // What cannot be restored now may be restorable on a later run: the
    // file stays as it is.
    let failed = |error: String| {
        eprintln!("workspace restore failed: {error}");
        Opening::default()
    };
    let terminals = match TerminalPlugin::running(terminald) {
        Ok(running) => running,
        Err(error) => return failed(error),
    };
    let agents = match agentd.map(AgentPlugin::running) {
        Some(Ok(agents)) => agents,
        Some(Err(error)) => return failed(error),
        None => Vec::new(),
    };
    let inventory = SessionInventory::new(
        inventory_of::<TerminalPlugin>(&terminals).chain(inventory_of::<AgentPlugin>(&agents)),
    );
    let running: Vec<_> = terminals.into_iter().chain(agents).collect();
    if let Err(error) = take_in(&mut model, inventory, &running) {
        return failed(error);
    }
    let terminald = terminald.handle();
    let of_kind = |model: &Workspace, kind: SessionKind| -> Vec<SessionId> {
        (model.session_summaries().iter())
            .filter(|session| session.kind == kind)
            .map(|session| session.id)
            .collect()
    };
    let agent_sessions = match agentd {
        Some(agentd) => (of_kind(&model, SessionKind::Agent).into_iter())
            .map(|session| (session, agentd.attach(session)))
            .collect(),
        None => Vec::new(),
    };
    let known = of_kind(&model, SessionKind::Terminal);
    let attached = terminald.attach_terminals(known.iter().map(|id| id.as_uuid()).collect());
    let sessions: Vec<(SessionId, Session)> = (attached.into_iter())
        .map(|(id, handle)| (SessionId::from_uuid(id), Session::attached(handle)))
        .collect();
    // One that went between the list and the attaching.
    for id in known {
        if !sessions.iter().any(|(attached, _)| *attached == id) {
            model.terminate_session(id);
        }
    }
    Opening {
        model,
        sessions,
        agent_sessions,
        save: save(store),
        ..Opening::default()
    }
}
