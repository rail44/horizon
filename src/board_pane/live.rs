use super::*;
pub(super) trait LineSource {
    fn next_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Option<String>>> + 'a>>;
}

impl LineSource for SubscribeStream {
    fn next_line<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Option<String>>> + 'a>> {
        Box::pin(SubscribeStream::next_line(self))
    }
}

/// Why the pump stopped. The pane treats all reasons the same (the pump just
/// ends); the variants exist so the teardown paths are individually testable.
pub(super) enum PumpStop {
    /// The pane closed (`Drop` fired the shutdown oneshot).
    Shutdown,
    /// logd closed the connection (drain/shutdown) or end-of-file.
    EndOfStream,
    /// A read error on the subscribe socket.
    Error,
    /// The foreground poke consumer is gone (the pane dropped its receiver).
    ReceiverDropped,
}

/// The pump's core loop: forwards one unit per line from `lines` onto
/// `poke_tx` until `shutdown` fires, the source ends, or the receiver drops.
/// `biased` so `shutdown` is checked first -- a close wins even while a read
/// is blocked, which is the whole point of the oneshot.
pub(super) async fn pump_lines<L: LineSource>(
    lines: &mut L,
    poke_tx: &mpsc::UnboundedSender<()>,
    mut shutdown: oneshot::Receiver<()>,
) -> PumpStop {
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return PumpStop::Shutdown,
            line = lines.next_line() => match line {
                Ok(Some(_)) => {
                    if poke_tx.unbounded_send(()).is_err() {
                        return PumpStop::ReceiverDropped;
                    }
                }
                Ok(None) => return PumpStop::EndOfStream,
                Err(_) => return PumpStop::Error,
            },
        }
    }
}

/// The background thread body: owns a `current_thread` tokio runtime, connects
/// to logd (`Store::subscribe` connect-or-spawns it), and runs the pump. Bails
/// quietly on any setup error (no root, no logd, a closed socket) -- the pane
/// simply gets no live updates and keeps working off its open-time read.
pub(super) fn run_subscribe_loop(
    root: PathBuf,
    poke_tx: mpsc::UnboundedSender<()>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(_) => return,
    };
    runtime.block_on(async move {
        let store = match Store::from_dir(&root) {
            Ok(s) => s,
            Err(_) => return,
        };
        // Subscribe (connect-or-spawn logd); race it against shutdown so a
        // close during connect still ends the loop promptly.
        let mut stream = tokio::select! {
            biased;
            _ = &mut shutdown_rx => return,
            stream = store.subscribe(None) => match stream {
                Ok(s) => s,
                Err(_) => return,
            },
        };
        // The first line is the cursor-on-connect (the current seq), not a
        // poke; discard it so the open-time read stays the sole "current
        // state" read.
        let _ = stream.next_line().await;
        let _ = pump_lines(&mut stream, &poke_tx, shutdown_rx).await;
    });
}

/// The pump's owned handles, held by the pane so closing it ends both halves
/// (see [`BoardPaneView`]'s `Drop` impl). The task is *not* detached.
pub(super) struct LiveUpdates {
    _pump_task: Task<()>,
    pub(super) shutdown: oneshot::Sender<()>,
}

/// Starts the live-update pump for `root` (the pane's store root): spawns the
/// background subscribe thread and a foreground `cx.spawn` consumer that
/// turns each poke into an `on_poke` re-read. Returns the handles the pane
/// owns for teardown. Called only when a root was resolved; a pane with no
/// root gets no live updates (matching its no-read empty state).
pub(super) fn start_live_updates(
    root: &std::path::Path,
    cx: &mut Context<BoardPaneView>,
) -> LiveUpdates {
    let (poke_tx, poke_rx) = mpsc::unbounded::<()>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let root = root.to_path_buf();
    std::thread::spawn(move || run_subscribe_loop(root, poke_tx, shutdown_rx));
    let _pump_task = cx.spawn(async move |this, cx| {
        let mut poke_rx = poke_rx;
        while let Some(()) = poke_rx.next().await {
            if this.update(cx, |view, cx| view.on_poke(cx)).is_err() {
                return;
            }
        }
    });
    LiveUpdates {
        _pump_task,
        shutdown: shutdown_tx,
    }
}

// ---------------------------------------------------------------------------
// The pane view
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    struct MockLines(futures::channel::mpsc::UnboundedReceiver<String>);

    impl super::LineSource for MockLines {
        fn next_line<'a>(
            &'a mut self,
        ) -> std::pin::Pin<
            std::boxed::Box<dyn std::future::Future<Output = std::io::Result<Option<String>>> + 'a>,
        > {
            use futures::StreamExt as _;
            std::boxed::Box::pin(async move { Ok(self.0.next().await) })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pump_forwards_lines_until_end_of_stream() {
        use futures::StreamExt as _;
        let (poke_tx, mut poke_rx) = futures::channel::mpsc::unbounded::<()>();
        let (line_tx, line_rx) = futures::channel::mpsc::unbounded::<String>();
        line_tx
            .unbounded_send(r#"{"log":"board","seq":1}"#.to_string())
            .unwrap();
        line_tx
            .unbounded_send(r#"{"log":"board","seq":2}"#.to_string())
            .unwrap();
        drop(line_tx);
        let mut src = MockLines(line_rx);
        let (_shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
        let stop = super::pump_lines(&mut src, &poke_tx, shutdown_rx).await;
        assert!(matches!(stop, super::PumpStop::EndOfStream));
        // Close the sender so the receiver sees end-of-stream after the two
        // forwarded pokes.
        drop(poke_tx);
        assert_eq!(poke_rx.next().await, Some(()));
        assert_eq!(poke_rx.next().await, Some(()));
        assert_eq!(poke_rx.next().await, None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pump_stops_on_shutdown_while_source_is_idle() {
        let (poke_tx, _poke_rx) = futures::channel::mpsc::unbounded::<()>();
        let (_line_tx, line_rx) = futures::channel::mpsc::unbounded::<String>();
        let mut src = MockLines(line_rx);
        let (shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
        // The source is idle (no lines), so the read blocks -- the only way
        // out is the shutdown oneshot, fired from a concurrent task the way
        // the pane's `Drop` fires it on the UI thread.
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = shutdown_tx.send(());
        });
        let stop = super::pump_lines(&mut src, &poke_tx, shutdown_rx).await;
        assert!(matches!(stop, super::PumpStop::Shutdown));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pump_stops_when_poke_receiver_is_gone() {
        let (poke_tx, poke_rx) = futures::channel::mpsc::unbounded::<()>();
        let (line_tx, line_rx) = futures::channel::mpsc::unbounded::<String>();
        line_tx
            .unbounded_send(r#"{"log":"board","seq":1}"#.to_string())
            .unwrap();
        drop(line_tx);
        let mut src = MockLines(line_rx);
        let (_shutdown_tx, shutdown_rx) = futures::channel::oneshot::channel::<()>();
        // The pane (foreground consumer) is already gone.
        drop(poke_rx);
        let stop = super::pump_lines(&mut src, &poke_tx, shutdown_rx).await;
        assert!(matches!(stop, super::PumpStop::ReceiverDropped));
    }
}
