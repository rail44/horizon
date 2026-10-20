//! Own one request span and its response until the retry decision consumes it.
use super::super::retry::Attempt;
use super::{
    response::{ResponseCollector, ResponseEnd},
    ProviderRequestSpan, TurnCompletion,
};
use crate::{
    config::RigAgentConfig,
    contract::{Event, ProviderEvent, ProviderRequestSent},
};
use crossbeam_channel::Sender;
use rig_core::{
    completion::Message,
    streaming::{Item, StreamEvent},
};

pub(super) struct ResponseAttempt {
    span: ProviderRequestSpan,
    response: ResponseCollector,
}
impl ResponseAttempt {
    pub(super) fn new(config: &RigAgentConfig, events: Sender<ProviderEvent>) -> Self {
        let _ = events.send(
            Event::ProviderRequestSent(ProviderRequestSent {
                model: config.model.clone(),
            })
            .into(),
        );
        Self {
            span: ProviderRequestSpan::new(events.clone()),
            response: ResponseCollector::new(config, events),
        }
    }
    pub(super) fn push(&mut self, chunk: Item<StreamEvent>) {
        self.response.push(chunk);
    }
    pub(super) fn close(
        mut self,
        end: anyhow::Result<ResponseEnd>,
        message_id: Option<String>,
        response: Option<rig_core::completion::CompletionResponse>,
    ) -> Attempt<(Message, TurnCompletion)> {
        let durable_output_emitted = self.response.has_issued_tools();
        self.span.finish();
        let (end, error) = match end {
            Ok(end) => (end, None),
            Err(error) => (ResponseEnd::Failed, Some(error)),
        };
        let collected = self.response.finish(end, message_id, response);
        match error {
            Some(error) => Attempt::Failed {
                error,
                partial: Some(collected),
                durable_output_emitted,
            },
            None => Attempt::Complete(collected),
        }
    }
}
