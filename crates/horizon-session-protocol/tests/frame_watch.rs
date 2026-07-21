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
use serde::{Deserialize, Serialize};
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

// --- non-final error self-heal (review fix 1) ---------------------------------
//
// A skewed peer publishes a value the reader cannot decode: remoc's watch
// `recv_impl` surfaces that as a *non-final* `RecvError` (base
// `Deserialize`/`MaxItemSizeExceeded` are `is_final() == false`), published
// as the receiver's current value while the channel stays open. The client's
// steady-state loop (`src/sessiond/connection.rs`) must therefore treat a
// non-final `borrow` error as "skip and wait for the next frame", not as a
// close — exactly what the seed and events paths already do. This pins that
// contract at the transport level with an asymmetric type pair (the §7
// V1/V2 method): the sender publishes `SenderMsg`, the reader decodes
// `ReaderMsg`, and one variant's payload is deliberately narrower on the
// wire than the reader's shape, so decoding it is a per-item error.

/// Frozen sender shape: `Skewed` carries only `a` on the wire.
#[derive(Clone, Serialize, Deserialize)]
enum SenderMsg {
    Value(u32),
    Skewed { a: u16 },
}

/// Reader shape: `Skewed` needs a second field `b` the sender never wrote,
/// so decoding a `SenderMsg::Skewed` as `ReaderMsg::Skewed` is a per-item
/// `Deserialize` error (missing field) — the same skew skew.rs proves for
/// mpsc, here on a watch. Only ever decoded off the wire, never constructed
/// in Rust, so its variants/fields read as dead.
#[allow(dead_code)]
#[derive(Clone, Serialize, Deserialize)]
enum ReaderMsg {
    Value(u32),
    Skewed { a: u16, b: u16 },
}

type SkewReaderSide = (
    Conn,
    rch::base::Sender<(), WireCodec>,
    rch::base::Receiver<rch::watch::Receiver<ReaderMsg, WireCodec>, WireCodec>,
);
type SkewWriterSide = (
    Conn,
    rch::base::Sender<rch::watch::Receiver<SenderMsg, WireCodec>, WireCodec>,
    rch::base::Receiver<(), WireCodec>,
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_final_watch_error_is_skipped_and_the_next_frame_recovers() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    let reader = tokio::spawn(async move {
        let (conn, _tx, mut rx): SkewReaderSide =
            remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
                .await
                .unwrap();
        tokio::spawn(conn);
        let mut frames = rx.recv().await.unwrap().unwrap();

        // The seed (`Value(1)`) decodes cleanly.
        assert!(
            frames.borrow_and_update().is_ok(),
            "the seed value must decode"
        );

        // The skewed value: a non-final decode error, published as the
        // current value while the channel survives.
        frames
            .changed()
            .await
            .expect("a new (skewed) value must arrive");
        match frames.borrow_and_update() {
            Ok(_) => panic!("the skewed value must fail to decode"),
            Err(err) => assert!(
                !err.is_final(),
                "a decode error must be non-final (skippable), got a final error: {err}"
            ),
        }

        // Recovery: the very next good value decodes, proving the channel was
        // never torn down by the skewed one.
        frames
            .changed()
            .await
            .expect("a recovery value must arrive");
        assert!(
            frames.borrow_and_update().is_ok(),
            "the channel must self-heal on the next good frame"
        );
        true
    });

    let (conn, mut tx, _rx): SkewWriterSide = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (sender, receiver) = rch::watch::channel::<SenderMsg, WireCodec>(SenderMsg::Value(1));
    tx.send(receiver).await.unwrap();

    // Pace the three values so the reader observes each discretely — a watch
    // coalesces, so without pacing the poison would be overwritten by the
    // recovery before the reader ever sees it (the same latest-value skip the
    // test above relies on). In-process the forwarding is sub-millisecond, so
    // these waits are ~1000x margin, not a correctness knob.
    tokio::time::sleep(Duration::from_millis(50)).await;
    sender.send(SenderMsg::Skewed { a: 7 }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    sender.send(SenderMsg::Value(2)).unwrap();

    assert!(reader.await.unwrap());
    drop(sender);
}
