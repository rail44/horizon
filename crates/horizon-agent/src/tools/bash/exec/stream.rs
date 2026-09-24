//! Shared output collection policy for Tokio pipes and blocking sandbox pipes.
//! Process waiting, cancellation and drain deadlines remain with each caller.
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};

type Buffer = Arc<Mutex<Vec<u8>>>;

pub(super) async fn pump(mut reader: impl AsyncRead + Unpin, buffer: Buffer) {
    let mut chunk = [0; 8192];
    loop {
        let result = reader.read(&mut chunk).await;
        if !collect_read(result, &chunk, &buffer) {
            break;
        }
    }
}

// A std Child owns blocking pipes. The caller retains the join handle to
// bound draining; a thread cannot be aborted like the Tokio pump's task.
pub(super) fn spawn_blocking_pump(
    mut reader: impl Read + Send + 'static,
) -> (Buffer, std::thread::JoinHandle<()>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let output = buffer.clone();
    let handle = std::thread::spawn(move || {
        let mut chunk = [0; 8192];
        loop {
            let result = reader.read(&mut chunk);
            if !collect_read(result, &chunk, &output) {
                break;
            }
        }
    });
    (buffer, handle)
}

fn collect_read(result: io::Result<usize>, chunk: &[u8], buffer: &Buffer) -> bool {
    match result {
        Ok(0) => false,
        Ok(n) => {
            buffer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(&chunk[..n]);
            true
        }
        Err(error) => error.kind() == io::ErrorKind::Interrupted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct Reader(VecDeque<io::Result<Vec<u8>>>);

    impl Read for Reader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let bytes = self
                .0
                .pop_front()
                .expect("must stop after EOF or a permanent error")?;
            output[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }
    }

    impl AsyncRead for Reader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let n = Read::read(&mut *self, output.initialize_unfilled())?;
            output.advance(n);
            Poll::Ready(Ok(()))
        }
    }

    fn reader(terminal_error: bool) -> Reader {
        Reader(VecDeque::from([
            Err(io::ErrorKind::Interrupted.into()),
            Ok(b"first".to_vec()),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(vec![0, 255]),
            if terminal_error {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(Vec::new())
            },
        ]))
    }

    #[tokio::test]
    async fn both_pumps_retry_interruptions_and_keep_bytes_until_eof_or_error() {
        for terminal_error in [false, true] {
            let (blocking, handle) = spawn_blocking_pump(reader(terminal_error));
            let asynchronous = Arc::new(Mutex::new(Vec::new()));
            pump(reader(terminal_error), asynchronous.clone()).await;
            handle.join().unwrap();
            for buffer in [blocking, asynchronous] {
                assert_eq!(*buffer.lock().unwrap(), b"first\x00\xff");
            }
        }
    }
}
