//! The containment tiers (`docs/agent-approval-design.md`) as a module of
//! the verdict stage: from what a call reaches, as its tool says
//! ([`Reach`]), whether it runs, waits for an approval, or is refused.

use std::path::Path;

use super::{CallContext, Containment, SessionModule};
use crate::contract::{ApprovalKind, ApprovalRequest, ToolPermission};
use crate::policy::ToolPlan;
use crate::tools::output::error;

pub(crate) struct TierPolicy;

impl SessionModule for TierPolicy {
    fn verdict(&self, cx: &CallContext<'_>) -> Option<ToolPlan> {
        let (call, reach) = (cx.call, cx.reach);
        let ask = |reason: String, kind| {
            ToolPlan::Approval(Box::new(ApprovalRequest {
                call_id: call.call_id.clone(),
                occurrence_id: call.occurrence_id.clone(),
                reason,
                kind,
            }))
        };
        // `bash` in a sandbox needs no refusal here: the sandbox makes the
        // checkout read-only, and an attempt to write comes back as a
        // containment denial that asks after the fact.
        if let Some(read_only) = cx.state.read_only()
            && reach.uncontained_write
        {
            return Some(ToolPlan::Reject(error(format!(
                "`{}` was not run: {}.",
                call.tool_id,
                read_only.guidance()
            ))));
        }
        if let Some(root) = &reach.primary_checkout {
            return Some(ask(
                primary_checkout_reason(&call.tool_id, root),
                ApprovalKind::Standard,
            ));
        }
        match reach.permission {
            Some(ToolPermission::Deny) => {
                return Some(ToolPlan::Reject(error(format!(
                    "Tool `{}` is denied by Horizon policy.",
                    call.tool_id
                ))));
            }
            None if reach.containment != Containment::Package => {
                return Some(ToolPlan::Reject(error(format!(
                    "Tool `{}` has no catalog permission.",
                    call.tool_id
                ))));
            }
            _ => {}
        }
        if let Some(path) = &reach.git_metadata {
            return Some(ask(
                format!(
                    "`{}` requested to write `{path}`, which is inside the repository's Git \
                     metadata directory. Allow this?",
                    call.tool_id
                ),
                ApprovalKind::Standard,
            ));
        }
        Some(match reach.containment {
            Containment::Inside
            | Containment::Contained
            | Containment::Package
            | Containment::CrossesBoundary { allowed: true } => ToolPlan::Automatic,
            Containment::ReadsOutside
            | Containment::CrossesBoundary { allowed: false }
            | Containment::Uncontained => {
                let (reason, kind) = reach.ask.clone().unwrap_or_else(|| {
                    (
                        format!(
                            "`{}` requested Horizon approval for this tool call.",
                            call.tool_id
                        ),
                        ApprovalKind::Standard,
                    )
                });
                ask(reason, kind)
            }
        })
    }
}

fn primary_checkout_reason(tool_id: &str, root: &Path) -> String {
    format!(
        "`{tool_id}` would write inside the shared primary checkout `{}`. Allow direct edits \
         for this session, or call `worktree_enter` to work in a disposable worktree instead?",
        root.display()
    )
}
