//! `AsyncRead`/`AsyncWrite` wrappers that count the bytes flowing
//! through them, for measuring real wire bytes (chmux framing included).

use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub struct CountingRead<T> {
    inner: T,
    pub count: Arc<AtomicU64>,
}

impl<T> CountingRead<T> {
    pub fn new(inner: T) -> (Self, Arc<AtomicU64>) {
        let count = Arc::new(AtomicU64::new(0));
        (
            Self {
                inner,
                count: count.clone(),
            },
            count,
        )
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for CountingRead<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let res = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &res {
            let read = buf.filled().len() - before;
            self.count.fetch_add(read as u64, Ordering::Relaxed);
        }
        res
    }
}

pub struct CountingWrite<T> {
    inner: T,
    pub count: Arc<AtomicU64>,
}

impl<T> CountingWrite<T> {
    pub fn new(inner: T) -> (Self, Arc<AtomicU64>) {
        let count = Arc::new(AtomicU64::new(0));
        (
            Self {
                inner,
                count: count.clone(),
            },
            count,
        )
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for CountingWrite<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let res = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &res {
            self.count.fetch_add(*n as u64, Ordering::Relaxed);
        }
        res
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
