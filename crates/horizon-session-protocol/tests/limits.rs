//! Boundary tests for the per-purpose wire size caps
//! (`FRAME_MAX_ITEM_BYTES` and friends), exercising both enforcement
//! topologies rch actually has (see `CappedReceiver`'s doc):
//!
//! - a **transported receiver** carries its cap as a const type parameter
//!   (the daemon→UI direction: updates/events/host-tools/skipped-lines);
//! - a **transported sender** carries the runtime cap its creator set
//!   before the handover (the UI→daemon direction: command channels).
//!
//! In both, an item under the cap is delivered; an item over it fails
//! *per-item* — the channel survives and later items still arrive, which
//! is what lets the receive loops treat it as a skip (adoption condition
//! 2's size-cap sibling).

use horizon_session_protocol::WireCodec;
use remoc::prelude::*;
use tokio::net::UnixStream;

/// The cap under test — small so the test payloads stay cheap; the
/// production constants differ only in magnitude.
const TEST_CAP: usize = 16 * 1024;

type Conn = remoc::Connect<'static, std::io::Error, std::io::Error>;
type CappedTestReceiver =
    rch::mpsc::Receiver<Vec<u8>, WireCodec, { rch::DEFAULT_BUFFER }, TEST_CAP>;

type ReceiverGiverSide = (
    Conn,
    rch::base::Sender<CappedTestReceiver, WireCodec>,
    rch::base::Receiver<(), WireCodec>,
);
type ReceiverTakerSide = (
    Conn,
    rch::base::Sender<(), WireCodec>,
    rch::base::Receiver<CappedTestReceiver, WireCodec>,
);

/// Daemon→UI shape: the channel's creator keeps the sender and transports
/// a receiver whose cap is in its type. Pins the *latch* semantics the
/// daemon pumps are written against: the oversized item's send fails
/// item-specifically, and the local sender is latched — every later send
/// fails too — so the pump's correct move is to close the attachment,
/// not skip-and-continue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transported_receivers_type_cap_fails_oversized_items_and_latches() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    let taker = tokio::spawn(async move {
        let (conn, _tx, mut rx): ReceiverTakerSide =
            remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
                .await
                .unwrap();
        tokio::spawn(conn);
        let mut items = rx.recv().await.unwrap().unwrap();
        let mut delivered = Vec::new();
        let mut skipped = 0;
        loop {
            match items.recv().await {
                Ok(Some(item)) => delivered.push(item.len()),
                Ok(None) => break,
                Err(err) if err.is_final() => break,
                Err(_skip) => skipped += 1,
            }
        }
        (delivered, skipped)
    });

    let (conn, mut tx, _rx): ReceiverGiverSide =
        remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
            .await
            .unwrap();
    tokio::spawn(conn);

    let (item_tx, item_rx) = rch::mpsc::channel::<Vec<u8>, WireCodec>(8);
    let item_rx = item_rx.set_max_item_size::<TEST_CAP>();
    tx.send(item_rx).await.unwrap();

    // An under-cap item is delivered.
    drop(item_tx.send(vec![1_u8; TEST_CAP / 2]).await.unwrap());

    // The oversized item fails item-specifically — either on the send
    // call itself (the error report can race ahead) or on its `Sending`
    // handle.
    let oversized_failed = match item_tx.send(vec![2_u8; TEST_CAP * 2]).await {
        Err(error) => {
            assert!(
                error.is_item_specific(),
                "got a channel-level error: {error}"
            );
            true
        }
        Ok(sending) => sending.await.is_err(),
    };
    assert!(
        oversized_failed,
        "an item twice the cap must not be delivered"
    );

    // ...and the failure LATCHES: the sender is dead from here on (this
    // is why the daemon-side pumps close the attachment on any send
    // error instead of skipping).
    let mut followup_failed = false;
    for _ in 0..50 {
        match item_tx.send(vec![3_u8; 64]).await {
            Err(error) => {
                assert!(error.is_item_specific(), "the latched error keeps its kind");
                followup_failed = true;
                break;
            }
            // The latch is reported asynchronously; a send slipping
            // through before it lands is possible — retry briefly.
            Ok(_sending) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
        }
    }
    assert!(
        followup_failed,
        "the remote-send error must latch onto subsequent sends"
    );
    drop(item_tx);

    let (delivered, _skipped) = taker.await.unwrap();
    assert_eq!(
        delivered.first(),
        Some(&(TEST_CAP / 2)),
        "the under-cap item before the oversized one arrives"
    );
}

type SenderGiverSide = (
    Conn,
    rch::base::Sender<rch::mpsc::Sender<Vec<u8>, WireCodec>, WireCodec>,
    rch::base::Receiver<(), WireCodec>,
);
type SenderTakerSide = (
    Conn,
    rch::base::Sender<(), WireCodec>,
    rch::base::Receiver<rch::mpsc::Sender<Vec<u8>, WireCodec>, WireCodec>,
);

/// UI→daemon shape: the channel's creator keeps the receiver and
/// transports a sender pre-capped with `set_max_item_size` — the cap
/// becomes the creator-side receive limit, and an oversized item from the
/// remote peer is a per-item (non-final) receive error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transported_senders_runtime_cap_fails_oversized_items_alone() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    // The remote peer: receives the pre-capped sender and pushes under,
    // over, under.
    let peer = tokio::spawn(async move {
        let (conn, _tx, mut rx): SenderTakerSide =
            remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
                .await
                .unwrap();
        tokio::spawn(conn);
        let item_tx = rx.recv().await.unwrap().unwrap();
        drop(item_tx.send(vec![1_u8; TEST_CAP / 2]).await.unwrap());
        drop(item_tx.send(vec![2_u8; TEST_CAP * 2]).await.unwrap());
        drop(item_tx.send(vec![3_u8; 64]).await.unwrap());
        // Give the mux a moment to flush before dropping the sender.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });

    let (conn, mut tx, _rx): SenderGiverSide = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (mut item_tx, mut item_rx) = rch::mpsc::channel::<Vec<u8>, WireCodec>(8);
    item_tx.set_max_item_size(TEST_CAP);
    tx.send(item_tx).await.unwrap();

    let mut delivered = Vec::new();
    loop {
        match item_rx.recv().await {
            Ok(Some(item)) => delivered.push(item.len()),
            Ok(None) => break,
            Err(err) if err.is_final() => break,
            // Measured: the oversized item does not even surface here as a
            // per-item error — the creator-side pump discards it before
            // recv (the error is reported towards the remote sender). The
            // arm stays for genuine decode failures.
            Err(_skip) => {}
        }
    }
    peer.await.unwrap();

    assert_eq!(
        delivered,
        vec![TEST_CAP / 2, 64],
        "exactly the under-cap items arrive, in order — the channel survived"
    );
}
