//! From a live session to the pane's display model:
//! `horizon-shell-runtime`'s adapter over this shell's session entity.

pub(crate) use horizon_shell_runtime::agent_view::adapter::*;

use super::super::session::AgentSession;

pub(crate) fn transcript_from(session: &AgentSession, settled: &mut SettledUnits) -> Adapted {
    adapt(
        &session.model,
        session.runtime_unreachable(),
        unix_epoch_ms(),
        settled,
    )
}
