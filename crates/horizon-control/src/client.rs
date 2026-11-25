//! One question to the control plane from a process that asks it one
//! thing: connect, `hello`, the request, its reply.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::contract::{CONTROL_VERSION, Envelope, EnvelopeBody, Hello, Query};
use crate::wire;

/// Asks `what` of the control plane at `socket` and returns the reply's
/// body. Every read and write waits at most `timeout`. An error reply, a
/// refused handshake and a broken connection are the `Err`, in words.
pub fn query(
    socket: &Path,
    what: &str,
    binary_id: &str,
    timeout: Duration,
) -> Result<EnvelopeBody, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|error| format!("cannot reach {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    let mut writer = stream;
    let mut ask = |id: u64, body: EnvelopeBody| -> Result<EnvelopeBody, String> {
        wire::write_envelope(&mut writer, &Envelope::new(id, body))
            .map_err(|error| error.to_string())?;
        let reply = wire::read_envelope(&mut reader)
            .map_err(|error| error.to_string())?
            .ok_or("the control plane closed the connection")?;
        match reply.body {
            EnvelopeBody::Error(error) => Err(error.message),
            EnvelopeBody::Rejected(rejected) => Err(rejected.reason),
            body => Ok(body),
        }
    };
    match ask(
        1,
        EnvelopeBody::Hello(Hello {
            control_version: CONTROL_VERSION,
            binary_id: binary_id.to_string(),
        }),
    )? {
        EnvelopeBody::HelloAck(_) => {}
        other => return Err(format!("unexpected reply to hello: {other:?}")),
    }
    ask(
        2,
        EnvelopeBody::Query(Query {
            what: what.to_string(),
        }),
    )
}

#[cfg(test)]
mod tests;
