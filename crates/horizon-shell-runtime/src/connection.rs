//! Process creation belongs to the production connector. Unit tests provide
//! their own in-process peers, including replacements after a simulated drain;
//! a temporarily absent listener must never start a real daemon with user data.

#[cfg(not(test))]
pub(super) use horizon_wire::spawn::{
    connect_or_spawn_agentd_retrying, connect_or_spawn_terminald_retrying,
};

#[cfg(test)]
pub(super) use connect_to_stub_retrying as connect_or_spawn_agentd_retrying;
#[cfg(test)]
pub(super) use connect_to_stub_retrying as connect_or_spawn_terminald_retrying;

#[cfg(test)]
pub(super) async fn connect_to_stub_retrying(
    socket_path: &std::path::Path,
    _control_socket: &std::path::Path,
) -> Result<tokio::net::UnixStream, String> {
    let mut delay = std::time::Duration::from_millis(50);
    loop {
        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(_) => tokio::time::sleep(delay).await,
        }
        delay = (delay * 2).min(std::time::Duration::from_secs(1));
    }
}
