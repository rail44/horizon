//! An ACP v2 client for `horizon-agentd`'s socket: everything the daemon
//! sends arrives, in order, on one inbox.

use std::time::Duration;

use agent_client_protocol::schema::{v2, ProtocolVersion};
use agent_client_protocol::{
    Agent, ByteStreams, Client, Error, JsonRpcRequest, Responder, V2ConnectionTo,
};
use horizon_acp as acp;
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::hub::DRAIN_TIMEOUT;

/// One message from the daemon.
#[derive(Debug)]
pub enum Inbound {
    Update(v2::UpdateSessionNotification),
    SessionEvent(acp::SessionEventNotification),
    TaskProgress(acp::TaskProgressNotification),
    ToolCallProgress(acp::ToolCallProgressNotification),
    Memory(acp::MemoryNotification),
    ProviderRequest(acp::ProviderRequestNotification),
    Permission(
        v2::RequestPermissionRequest,
        Responder<v2::RequestPermissionResponse>,
    ),
    HostTool(acp::HostToolRequest, Responder<acp::HostToolResponse>),
    /// The answer to a request sent with [`AcpClient::send_ordered`], at its
    /// place among the notifications.
    Replied(Result<serde_json::Value, Error>),
}

/// A live connection. Dropping it closes the socket, so a daemon's
/// one-at-a-time accept loop can serve the next connection.
pub struct AcpClient {
    pub cx: V2ConnectionTo<Agent>,
    pub inbox: mpsc::UnboundedReceiver<Inbound>,
    inbox_tx: mpsc::UnboundedSender<Inbound>,
    task: JoinHandle<()>,
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The `initialize` request a Horizon shell sends, with the given extension
/// version in `_meta.horizon`.
pub fn initialize_request(client_name: &str, ext_version: u32) -> v2::InitializeRequest {
    let mut meta = None;
    acp::write_horizon_meta(
        &mut meta,
        &acp::InitializeMeta {
            ext_version,
            binary_id: client_name.to_string(),
        },
    )
    .expect("initialize meta serializes");
    v2::InitializeRequest::new(
        ProtocolVersion::V2,
        v2::Implementation::new(client_name, "0.0.0"),
    )
    .meta(meta)
}

fn byte_streams(stream: UnixStream) -> impl agent_client_protocol::ConnectTo<Client> + 'static {
    let (read, write) = stream.into_split();
    ByteStreams::new(write.compat_write(), read.compat())
}

/// Opens the connection without initializing it.
pub async fn connect_acp(stream: UnixStream) -> AcpClient {
    let (inbox_tx, inbox) = mpsc::unbounded_channel();
    let (cx_tx, cx_rx) = oneshot::channel();

    macro_rules! notification {
        ($builder:expr, $ty:ty, $variant:ident) => {{
            let tx = inbox_tx.clone();
            $builder.on_receive_notification(
                async move |notification: $ty, _cx: V2ConnectionTo<Agent>| {
                    let _ = tx.send(Inbound::$variant(notification));
                    Ok(())
                },
                agent_client_protocol::on_receive_notification!(),
            )
        }};
    }
    macro_rules! request {
        ($builder:expr, $ty:ty, $variant:ident) => {{
            let tx = inbox_tx.clone();
            $builder.on_receive_request(
                async move |request: $ty,
                            responder: Responder<<$ty as JsonRpcRequest>::Response>,
                            _cx: V2ConnectionTo<Agent>| {
                    let _ = tx.send(Inbound::$variant(request, responder));
                    Ok(())
                },
                agent_client_protocol::on_receive_request!(),
            )
        }};
    }

    let builder = Client.v2().name("horizon-daemon-testkit");
    let builder = notification!(builder, v2::UpdateSessionNotification, Update);
    let builder = notification!(builder, acp::SessionEventNotification, SessionEvent);
    let builder = notification!(builder, acp::TaskProgressNotification, TaskProgress);
    let builder = notification!(builder, acp::ToolCallProgressNotification, ToolCallProgress);
    let builder = notification!(builder, acp::MemoryNotification, Memory);
    let builder = notification!(builder, acp::ProviderRequestNotification, ProviderRequest);
    let builder = request!(builder, v2::RequestPermissionRequest, Permission);
    let builder = request!(builder, acp::HostToolRequest, HostTool);
    let transport = byte_streams(stream);
    let task = tokio::spawn(async move {
        let _ = builder
            .connect_with(transport, async move |cx: V2ConnectionTo<Agent>| {
                let _ = cx_tx.send(cx.clone());
                cx.incoming_closed().await;
                Ok(())
            })
            .await;
    });
    let cx = cx_rx
        .await
        .expect("the ACP connection should start its foreground task");
    AcpClient {
        cx,
        inbox,
        inbox_tx,
        task,
    }
}

impl AcpClient {
    pub async fn request<R: JsonRpcRequest>(&self, request: R) -> Result<R::Response, Error> {
        self.cx.send_request(request).block_task().await
    }

    /// `initialize` at this build's extension version.
    pub async fn initialize(&self, client_name: &str) -> Result<v2::InitializeResponse, Error> {
        self.request(initialize_request(
            client_name,
            acp::HORIZON_ACP_EXT_VERSION,
        ))
        .await
    }

    /// Sends `request`; its answer arrives on the inbox as
    /// [`Inbound::Replied`], after every notification sent before it.
    pub fn send_ordered<R: JsonRpcRequest>(&self, request: R) {
        let inbox = self.inbox_tx.clone();
        let method = request.method().to_string();
        let registered = self
            .cx
            .send_request(request)
            .on_receiving_result(async move |result| {
                use agent_client_protocol::JsonRpcResponse;
                let result = result.and_then(|response| response.into_json(&method));
                let _ = inbox.send(Inbound::Replied(result));
                Ok(())
            });
        registered.expect("the request should be sent");
    }

    /// Asks the daemon to drain. It exits inside the call, so the answer
    /// never travels; the caller confirms the exit with
    /// [`crate::wait_for_exit`].
    pub async fn drain(&self) {
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, self.request(acp::DrainRequest {})).await;
    }

    /// Reads the next inbound message, panicking after `timeout`.
    pub async fn next(&mut self, timeout: Duration) -> Inbound {
        tokio::time::timeout(timeout, self.inbox.recv())
            .await
            .expect("timed out waiting for a message from the daemon")
            .expect("the connection closed")
    }
}

/// Connects, initializes at this build's extension version, and panics if
/// the daemon refuses.
pub async fn connect_initialized(stream: UnixStream, client_name: &str) -> AcpClient {
    let client = connect_acp(stream).await;
    client
        .initialize(client_name)
        .await
        .expect("initialize should succeed at a matching extension version");
    client
}

/// Sends `_horizon/drain` on a connection that never initialized -- the
/// recovery a client whose `initialize` was refused uses. The daemon exits
/// inside the call.
pub async fn drain_uninitialized(stream: UnixStream) {
    let (cx_tx, cx_rx) = oneshot::channel();
    let transport = byte_streams(stream);
    let task = tokio::spawn(async move {
        let _ = Client
            .builder()
            .without_acp_version_guard()
            .connect_with(transport, async move |cx| {
                let _ = cx_tx.send(cx.clone());
                cx.incoming_closed().await;
                Ok(())
            })
            .await;
    });
    if let Ok(cx) = cx_rx.await {
        let _ = tokio::time::timeout(
            DRAIN_TIMEOUT,
            cx.send_request(acp::DrainRequest {}).block_task(),
        )
        .await;
    }
    task.abort();
}
