//! Stopped rounds wait for the host to settle issued calls before another request.
use super::{SessionLoopState, ToolCallDescriptor, TurnCompletion};
use crate::contract::{Command, ProviderEvent, ToolCallId};
use std::collections::HashMap;

impl SessionLoopState {
    pub(super) async fn settle_outcome(&mut self, outcome: &mut TurnCompletion) {
        let calls = std::mem::take(&mut outcome.requested_tool_calls);
        let order = std::mem::take(&mut outcome.requested_tool_call_ids);
        self.settle_calls(order, calls).await;
    }
    pub(super) async fn settle_calls(
        &mut self,
        order: Vec<ToolCallId>,
        calls: HashMap<ToolCallId, ToolCallDescriptor>,
    ) {
        if calls.is_empty()
            || self
                .inbox
                .iter()
                .any(|command| matches!(command, Command::Shutdown))
        {
            return;
        }
        let id = uuid::Uuid::new_v4().to_string();
        if self
            .events_tx
            .send(ProviderEvent::SettleTools {
                id: id.clone(),
                calls: order
                    .iter()
                    .map(|key| calls[key].identity.clone())
                    .collect(),
            })
            .is_err()
        {
            self.inbox.push_front(Command::Shutdown);
            return;
        }
        loop {
            match self.commands.recv().await {
                Some(Command::ToolCallsSettled {
                    id: received,
                    results,
                }) if received == id => {
                    for result in results {
                        if let Some(descriptor) = calls.get(&result.call_id) {
                            self.record_tool_effects(&result, descriptor);
                            if let Err(message) =
                                self.rig_history.append_result(&result, &descriptor.tool_id)
                            {
                                let _ = self.events_tx.send(
                                    crate::contract::Event::Error(crate::contract::Error {
                                        message,
                                    })
                                    .into(),
                                );
                                self.inbox.push_front(Command::Shutdown);
                                return;
                            }
                        }
                    }
                    self.inbox.retain(|command| !matches!(command,
                        Command::ToolCallResult(result) if calls.contains_key(&result.call_id))
                        && !matches!(command, Command::ToolCallReissued(identity) if calls.contains_key(&identity.call_id)));
                    break;
                }
                Some(Command::Shutdown) | None => {
                    self.inbox.push_front(Command::Shutdown);
                    break;
                }
                Some(command) => self.inbox.push_back(command),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::{event_log, projection::duckdb::Store};
    use crate::providers::rig::{
        completion::CompletionStop,
        conversation::{ConversationHistory, Prompt},
    };
    use crate::{
        contract::{
            Event, MessageRole, OccurrenceId, ProviderRequestSent, SessionId, ToolCallRequest,
            ToolOutcome,
        },
        live::LiveState,
        tools::{process_agent_provider_event, HostTools, ToolSessionBuilder},
    };
    use serde_json::json;

    #[tokio::test]
    async fn settlement_stops_when_shutdown_is_queued_or_the_host_is_gone() {
        for queued_shutdown in [true, false] {
            let (events, receive) = crossbeam_channel::unbounded();
            // Keep the command channel open to prove neither path waits for an ack.
            let (_commands, input) = tokio::sync::mpsc::unbounded_channel();
            let mut state = SessionLoopState {
                commands: input,
                events_tx: events,
                ..Default::default()
            };
            if queued_shutdown {
                state.inbox.push_front(Command::Shutdown);
            } else {
                drop(receive);
            }
            let identity = crate::contract::ToolCallIdentity {
                call_id: ToolCallId("pending".into()),
                occurrence_id: OccurrenceId::new(),
            };
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                state.settle_calls(
                    vec![identity.call_id.clone()],
                    HashMap::from([(
                        identity.call_id.clone(),
                        ToolCallDescriptor {
                            identity,
                            tool_id: "fs.read".into(),
                            args: json!({"path":"pending.txt"}),
                        },
                    )]),
                ),
            )
            .await
            .expect("shutdown must not wait for host settlement");
            assert!(matches!(state.inbox.front(), Some(Command::Shutdown)));
            assert!(state.rig_history.is_empty());
        }
    }

    #[tokio::test]
    async fn stopped_round_keeps_a_real_write_in_live_history_jsonl_and_database() {
        struct NoHost;
        impl HostTools for NoHost {
            fn execute_auto(&self, _: &str, _: &serde_json::Value) -> Option<serde_json::Value> {
                None
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let session = SessionId::new();
        let tools = ToolSessionBuilder::new(root.clone())
            .with_isolated_worktree(true)
            .build();
        let log = root.join("history.jsonl");
        let (writer, ready) = event_log::WriterHandle::open(&log);
        assert!(matches!(
            ready.recv().unwrap(),
            event_log::WriterInit::Ready(_)
        ));
        let live =
            LiveState::with_event_log_and_history(session, None, None, writer.clone(), vec![]);
        let written = ToolCallRequest {
            call_id: ToolCallId("write".into()),
            occurrence_id: OccurrenceId::new(),
            tool_id: "fs.write".into(),
            input: json!({"path":root.join("changed.txt"),"content":"actual change\n"}).into(),
        };
        let pending = ToolCallRequest {
            call_id: ToolCallId("pending".into()),
            occurrence_id: OccurrenceId::new(),
            tool_id: "fs.read".into(),
            input: json!({"path":"not-run.txt"}).into(),
        };
        let (events, receive) = crossbeam_channel::unbounded();
        let mut history = ConversationHistory::default();
        history.open_turn(&events);
        history
            .append_prompt(
                Prompt::input(crate::contract::ConversationInputKind::User, "edit"),
                &events,
            )
            .unwrap();
        for event in receive.try_iter() {
            live.extend_provider_events([event]).unwrap();
        }
        for request in [&written, &pending] {
            let event =
                crate::providers::rig::conversation::announcement(request, "stopped-response");
            history.apply_event(&event).unwrap();
            live.extend_events([event]);
        }
        live.extend_events([
            Event::MessageCommitted(crate::contract::Message {
                role: MessageRole::User,
                text: "edit".into(),
            }),
            Event::ProviderRequestSent(ProviderRequestSent {
                model: "test".into(),
            }),
        ]);
        let written_processing = process_agent_provider_event(
            &NoHost,
            &tools,
            session,
            &live,
            Event::ToolCallRequested(written.clone()),
        )
        .unwrap();
        let [Command::ToolCallResult(real_result)] = &written_processing.provider_commands[..]
        else {
            panic!("write should complete")
        };
        assert_eq!(real_result.outcome, ToolOutcome::Succeeded);
        live.extend_events([
            Event::ToolCallRequested(pending.clone()),
            Event::ProviderRequestFinished,
        ]);
        let (commands, input) = tokio::sync::mpsc::unbounded_channel();
        let mut state = SessionLoopState {
            commands: input,
            events_tx: events,
            rig_history: history,
            ..Default::default()
        };
        state
            .inbox
            .extend(written_processing.provider_commands.clone());
        let mut outcome = TurnCompletion {
            stop: CompletionStop::Failed,
            requested_tool_call_ids: vec![written.call_id.clone(), pending.call_id.clone()],
            requested_tool_calls: [&written, &pending]
                .into_iter()
                .map(|request| {
                    (
                        request.call_id.clone(),
                        ToolCallDescriptor {
                            identity: request.identity(),
                            tool_id: request.tool_id.clone(),
                            args: request.input.0.clone(),
                        },
                    )
                })
                .collect(),
            ..Default::default()
        };
        tokio::join!(state.settle_outcome(&mut outcome), async {
            let request = loop {
                if let Ok(event) = receive.try_recv() {
                    break event;
                }
                tokio::task::yield_now().await;
            };
            assert!(matches!(request, ProviderEvent::SettleTools { .. }));
            let processing =
                process_agent_provider_event(&NoHost, &tools, session, &live, request).unwrap();
            for command in processing.provider_commands {
                commands.send(command).unwrap();
            }
        });
        state.apply_turn_outcome(outcome);
        for event in receive.try_iter() {
            live.extend_provider_events([event]).unwrap();
        }
        assert!(
            state.inbox.is_empty(),
            "the earlier queued result was consumed by settlement"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("changed.txt")).unwrap(),
            "actual change\n"
        );
        assert!(live.frame().unfinished_tool_calls().is_empty());
        let events = live.events();
        assert_eq!(events.iter().filter(|event| matches!(event, Event::ToolCallFinished(result) if result.call_id == written.call_id)).count(), 1);
        assert!(events.iter().any(
            |event| matches!(event, Event::ToolCallFinished(result) if result == real_result)
        ));
        writer.flush().unwrap();
        let records = event_log::read(&log).unwrap().records;
        let restored: Vec<_> = records.iter().map(|record| record.event.clone()).collect();
        assert_eq!(
            ConversationHistory::from_events(&restored)
                .unwrap()
                .messages(),
            state.rig_history.messages()
        );
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store
                .replace_from_event_log_records(records)
                .unwrap()
                .skipped,
            0
        );
        let database = crate::persistence::projection::duckdb::DuckdbStoreHandle::new(store);
        assert_eq!(
            crate::providers::rig::history::load_rig_session_history(Some(&database), session, &[])
                .unwrap()
                .messages
                .messages(),
            state.rig_history.messages()
        );
        let changes = crate::transcript::aggregate_changes(
            &crate::transcript::build_tool_call_views(&live.frame().items),
        );
        assert_eq!(changes.len(), 1);
    }
}
