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

use horizon_session_protocol::{WireCodec, TERMINAL_EVENT_MAX_ITEM_BYTES};
use horizon_terminal_core::{
    TerminalColor, TerminalLine, TerminalScrollWindow, TerminalSpan, TerminalUnderline,
};
use remoc::codec::Codec;
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

/// Review fix (High): a served scroll window
/// (`horizon_terminal_core::TerminalScrollWindow`, carried as one
/// `TerminalUpdate::ScrollWindow` on the events mpsc) must stay under
/// [`TERMINAL_EVENT_MAX_ITEM_BYTES`] even at worst-case decoration — otherwise
/// it trips the over-cap latch the transported-receiver test above pins and
/// tears the shared events channel down (dropping the pane's
/// `Exited`/`Error`/`Bell` and orphaning it). `TerminalCore::snapshot_window`
/// guarantees this by clamping a window to
/// `(TERMINAL_EVENT_MAX_ITEM_BYTES / 2) / (columns * 32)` rows. This is the
/// independent, real-codec (Postbag) proof of that budget: a window built at
/// the clamp's own maximum, every cell a distinct fully-styled span — fatter
/// than anything `snapshot_window` can actually emit, since it merges
/// same-style runs into one span — still serializes well under the cap across
/// a range of terminal widths.
#[test]
fn a_worst_case_scroll_window_stays_under_the_events_cap() {
    // Mirrors `core::render::{EVENTS_ITEM_CAP_BYTES, WORST_CASE_BYTES_PER_CELL,
    // max_window_rows}` (private to horizon-terminal-core) as an independent
    // check of the same budget under the actual wire codec. The worst span
    // below measures ~107 B/cell, so 128 is the conservative per-cell bound
    // the clamp is sized against. (The `.max(screen_lines)` floor the real
    // clamp applies is intentionally *not* modeled here: it only enlarges a
    // window when the byte budget already permits fewer rows than the
    // viewport, an envelope no tighter than the live-frame watch's own — see
    // `max_window_rows`.)
    const WORST_CASE_BYTES_PER_CELL: usize = 128;
    let budget = TERMINAL_EVENT_MAX_ITEM_BYTES / 2;

    let worst_span = || TerminalSpan {
        text: "M".to_string(),
        columns: 1,
        fg: TerminalColor::Rgb([1, 2, 3]),
        bg: TerminalColor::Rgb([4, 5, 6]),
        italic: true,
        strikethrough: true,
        underline: TerminalUnderline::Curl,
        underline_color: Some(TerminalColor::Rgb([7, 8, 9])),
    };

    for columns in [80usize, 200, 500, 1000] {
        let max_rows = (budget / (columns * WORST_CASE_BYTES_PER_CELL)).max(1);
        let row = TerminalLine {
            spans: (0..columns).map(|_| worst_span()).collect(),
        };
        let window = TerminalScrollWindow {
            lines: vec![row; max_rows],
            viewport_offset: 0,
            above: 0,
            below: 0,
        };

        let mut bytes = Vec::new();
        <WireCodec as Codec>::serialize(&mut bytes, &window).unwrap();
        assert!(
            bytes.len() < TERMINAL_EVENT_MAX_ITEM_BYTES,
            "worst-case {columns}-col window ({max_rows} rows) serialized to {} bytes, \
             over the {TERMINAL_EVENT_MAX_ITEM_BYTES}-byte events cap",
            bytes.len()
        );
    }
}
