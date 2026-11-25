//! The board's reply sink in the outbox (`horizon_agent_host::outbox`):
//! an answer addressed to a board task is posted to its thread, and a
//! review result is handed to the session that asked for the review.

use super::ReplyAddress;
use super::operations::parse_session;
use horizon_agent::contract::{SessionId, SessionInput};
use horizon_agent_host::outbox::log::Index;
use horizon_agent_host::outbox::{BoxFuture, Context, Reply, ReplySink};
use horizon_board::{Comment, Store};

/// The board's reply sink: an answer addressed to a board task is posted to
/// its thread; a review result is handed to the requesting task session.
pub(super) struct BoardSink;

impl ReplySink for BoardSink {
    fn kinds(&self) -> &'static [&'static str] {
        &["board", "review-result"]
    }

    fn recipient_key(&self, address: &str) -> Option<String> {
        match serde_json::from_str::<ReplyAddress>(address).ok()? {
            ReplyAddress::Session { id } | ReplyAddress::ReviewResult { id, .. } => {
                Some(format!("session:{id}"))
            }
            ReplyAddress::Board { root, task } => Some(format!("board:{}:{task}", root.display())),
        }
    }

    fn deliver<'a>(
        &'a self,
        ctx: Context<'a>,
        reply: Reply<'a>,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let address: ReplyAddress = serde_json::from_str(reply.address)
                .map_err(|e| format!("Invalid stored reply destination: {e}"))?;
            let source = reply.source;
            let id = reply.id;
            Ok(match address {
                ReplyAddress::Board { root, task } => {
                    source_project(ctx.index, source, &root)?;
                    let store = Store::from_dir(&root).map_err(|e| e.to_string())?;
                    store
                        .post_message(
                            task,
                            Comment {
                                id: format!("session:{}:{id}", source.as_uuid()),
                                author: reply.author,
                                text: reply.text,
                                at: Some(reply.at),
                                source: Some(format!("session:{}:{id}", source.as_uuid())),
                            },
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    true
                }
                ReplyAddress::ReviewResult {
                    id: target,
                    root,
                    task,
                } => {
                    let target = parse_session(&target)?;
                    source_project(ctx.index, source, &root)?;
                    source_project(ctx.index, target, &root)?;
                    ctx.deliver_input(
                        target,
                        SessionInput {
                            id: format!("session:{}:{id}", source.as_uuid()),
                            origin: reply.author,
                            text: reply.text,
                            reply_to: Some(ReplyAddress::board(&root, task)?),
                            resume_work: false,
                        },
                    )
                }
                ReplyAddress::Session { .. } => {
                    return Err("A session address belongs to the outbox core".into());
                }
            })
        })
    }
}

fn source_project(
    index: &Index,
    source: SessionId,
    expected: &std::path::Path,
) -> Result<(), String> {
    let recorded = index.projects.get(&source).cloned().or_else(|| {
        index
            .sessions
            .get(&source)
            .and_then(|record| record.session_context.as_ref())
            .and_then(|context| context.workspace_root.as_deref())
            .and_then(horizon_agent_host::worktree::project_root)
    });
    if recorded.as_deref() != Some(expected) {
        return Err("Reply destination differs from the source session's project".into());
    }
    Ok(())
}

#[cfg(test)]
mod review_tests;
