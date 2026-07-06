//! The abstraction [`connection`](super::connection) dispatches every
//! accepted `Invoke`/`Query` request through, so connection handling stays
//! testable without floem or a live `Workspace` -- the mission's "接続処理
//! は「リクエスト → 応答」を返す実行チャネルに対して書く" requirement.

use horizon_control::contract::{EnvelopeBody, ErrorMessage, Invoke, Query};

/// One accepted request, already split out of its envelope's `kind`/
/// `payload` -- everything [`ControlExecutor::execute`] needs to answer it.
#[derive(Clone, Debug)]
pub(super) enum ControlRequest {
    Invoke(Invoke),
    Query(Query),
}

/// What a connection thread calls to turn a request into a response body,
/// without knowing (or needing to know) how that answer actually gets
/// produced. [`super::bridge::ChannelExecutor`] is the real implementation
/// (bridges to the UI thread); this module's own tests, and
/// `connection`'s, use a stub that never touches floem at all.
pub(super) trait ControlExecutor: Send + Sync {
    fn execute(&self, request: ControlRequest) -> EnvelopeBody;
}

/// Builds an `EnvelopeBody::Error` -- the shared shape every executor
/// implementation's failure path (a bad command name, a timeout waiting for
/// the UI thread, ...) converges on.
pub(super) fn error_body(message: impl Into<String>) -> EnvelopeBody {
    EnvelopeBody::Error(ErrorMessage {
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_body_carries_the_message_through() {
        match error_body("something went wrong") {
            EnvelopeBody::Error(ErrorMessage { message }) => {
                assert_eq!(message, "something went wrong");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
