//! The v11 frame-path semantics (`docs/remoc-adoption-design.md` §5 Option
//! A), pinned over a *live* `rch::watch<TerminalFrame>` transported across a
//! real remoc connection: a slow reader observes a skipping sequence — the
//! watch keeps only the latest value — and always converges on the final
//! frame. This is the property that makes backpressure a non-problem for the
//! frame path (a screen just wants the newest state), and the spike's §1c
//! finding promoted to a standing test.
//!
//! Adoption condition 3: both `Connect::io` handshakes are driven
//! concurrently (each end spawns its multiplexer task).

use std::time::Duration;

use horizon_session_protocol::{CappedWatchReceiver, WireCodec, FRAME_MAX_ITEM_BYTES};
use horizon_terminal_core::TerminalFrame;
use remoc::prelude::*;
use tokio::net::UnixStream;

// The `Connect::io` (conn, base-sender, base-receiver) triples, named to
// keep clippy's `type_complexity` lint quiet.
type Conn = remoc::Connect<'static, std::io::Error, std::io::Error>;
type Frames = CappedWatchReceiver<TerminalFrame, FRAME_MAX_ITEM_BYTES>;
type ReaderSide = (
    Conn,
    rch::base::Sender<(), WireCodec>,
    rch::base::Receiver<Frames, WireCodec>,
);
type WriterSide = (
    Conn,
    rch::base::Sender<Frames, WireCodec>,
    rch::base::Receiver<(), WireCodec>,
);

/// The number of frames the writer blasts; the reader must converge on the
/// last one having observed far fewer (the skip).
const FINAL: usize = 200;

fn frame_text(i: usize) -> String {
    format!("frame {i}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_watch_reader_skips_intermediates_and_converges_on_the_final_frame() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    // Reader side: reads deliberately slowly, so the writer's blast of
    // frames outpaces it and the watch collapses the backlog to its latest.
    let reader = tokio::spawn(async move {
        let (conn, _tx, mut rx): ReaderSide = remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut frames = rx.recv().await.unwrap().unwrap();
        let mut observed = 0usize;
        loop {
            // Deliberately slow: give the writer time to get ahead so the
            // watch has intermediate values to skip.
            tokio::time::sleep(Duration::from_millis(1)).await;
            let current = frames.borrow_and_update().unwrap().clone();
            observed += 1;
            if current.text() == frame_text(FINAL) {
                return observed;
            }
            // No new value yet -> wait for one (or the writer closing).
            if frames.changed().await.is_err() {
                panic!("the watch closed before the final frame arrived (observed {observed})");
            }
        }
    });

    // Writer side.
    let (conn, mut tx, _rx): WriterSide = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (frame_tx, frame_rx) =
        rch::watch::channel::<TerminalFrame, WireCodec>(TerminalFrame::empty())
            .with_max_item_size::<FRAME_MAX_ITEM_BYTES>();
    tx.send(frame_rx).await.unwrap();

    // Publish every frame as fast as possible — `rch::watch::Sender::send`
    // is synchronous and overwrites the latest value with no queue to bound.
    for i in 1..=FINAL {
        frame_tx
            .send(TerminalFrame::from_text(frame_text(i)))
            .unwrap();
    }

    let observed = reader.await.unwrap();
    assert!(
        observed < FINAL,
        "a watch reader must skip intermediate frames, but it observed {observed} of {FINAL}"
    );
    // Keep the sender alive until the reader has converged.
    drop(frame_tx);
}
