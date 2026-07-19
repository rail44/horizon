//! Item 1: hot-path benchmark — frame streaming over a unix socket.
//!
//! Compares, over `tokio::net::UnixStream::pair()`:
//!   * baseline: serde_json + newline framing (current horizon wire),
//!   * remoc `rch::mpsc` with Postbag (default codec) and CBOR,
//!   * remoc `rch::watch` with Postbag (+ latest-value-skip demo).
//!
//! Also measures pure per-frame encode/decode CPU and encoded sizes for
//! serde_json / Postbag / CBOR on the same synthesized frame sequences.
//!
//! Run: `cargo run --release --bin bench`

use std::{
    hint::black_box,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use horizon_terminal_core::{TerminalCursor, TerminalCursorShape, TerminalFrame};
use remoc::{
    codec::{Ciborium, Codec, Json, Postbag, PostbagSlim},
    rch,
};
use remoc_spike::{frames::synth_frames, io_count};
use tokio::net::UnixStream;

const MICRO_FRAMES: usize = 300;
const MICRO_PASSES: usize = 5;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    for (cols, rows, n_e2e) in [(80usize, 24usize, 2000usize), (200, 50, 600)] {
        println!("\n=== frame size {cols}x{rows} ===");
        let frames = synth_frames(cols, rows, MICRO_FRAMES);

        codec_micro::<Json>("json (serde_json)", &frames);
        codec_micro::<Postbag>("postbag (full)", &frames);
        codec_micro::<PostbagSlim>("postbag (slim)", &frames);
        codec_micro::<Ciborium>("cbor", &frames);

        // Owned copies for the send loops, cloned outside the clock.
        let seq: Vec<TerminalFrame> = (0..n_e2e)
            .map(|i| frames[i % frames.len()].clone())
            .collect();

        rt.block_on(e2e_jsonl(&seq));
        rt.block_on(e2e_remoc_mpsc::<Postbag>("remoc mpsc postbag", &seq));
        rt.block_on(e2e_remoc_mpsc::<PostbagSlim>("remoc mpsc pb-slim", &seq));
        rt.block_on(e2e_remoc_mpsc::<Ciborium>("remoc mpsc cbor", &seq));
        rt.block_on(e2e_remoc_mpsc::<Json>("remoc mpsc json", &seq));
        rt.block_on(e2e_remoc_watch::<Postbag>("remoc watch postbag", &seq));
    }

    rt.block_on(watch_skip_demo());
}

/// Pure per-frame encode / decode CPU time and encoded size.
///
/// serde_json is measured through remoc's `codec::Json`, which is a thin
/// wrapper over `serde_json::to_writer`/`from_reader` — same crate the
/// current JSONL wire uses.
fn codec_micro<C: Codec>(name: &str, frames: &[TerminalFrame]) {
    // Warmup + size measurement.
    let encoded: Vec<Vec<u8>> = frames
        .iter()
        .map(|f| {
            let mut buf = Vec::new();
            <C as Codec>::serialize(&mut buf, f).unwrap();
            buf
        })
        .collect();
    let bytes: usize = encoded.iter().map(Vec::len).sum();

    let start = Instant::now();
    for _ in 0..MICRO_PASSES {
        for f in frames {
            let mut buf = Vec::with_capacity(64 * 1024);
            <C as Codec>::serialize(&mut buf, f).unwrap();
            black_box(&buf);
        }
    }
    let enc = start.elapsed() / (MICRO_PASSES * frames.len()) as u32;

    let start = Instant::now();
    for _ in 0..MICRO_PASSES {
        for e in &encoded {
            let f: TerminalFrame = <C as Codec>::deserialize(&e[..]).unwrap();
            black_box(&f);
        }
    }
    let dec = start.elapsed() / (MICRO_PASSES * frames.len()) as u32;

    println!(
        "codec {name:<20} encode {enc:>10.1?}/frame  decode {dec:>10.1?}/frame  size {:>7} B/frame",
        bytes / frames.len()
    );
}

/// Baseline: serde_json + b'\n' framing over a unix socket, flushed per
/// frame (the current horizon delivery pattern).
async fn e2e_jsonl(seq: &[TerminalFrame]) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

    let n = seq.len();
    let (a, b) = UnixStream::pair().unwrap();
    let seq = seq.to_vec();

    let writer = tokio::spawn(async move {
        let mut w = BufWriter::new(a);
        let mut bytes = 0u64;
        let start = Instant::now();
        for f in &seq {
            let buf = serde_json::to_vec(f).unwrap();
            bytes += buf.len() as u64 + 1;
            w.write_all(&buf).await.unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
        }
        (start, bytes)
    });

    let reader = tokio::spawn(async move {
        let mut r = BufReader::with_capacity(256 * 1024, b);
        let mut buf = Vec::new();
        for _ in 0..n {
            buf.clear();
            r.read_until(b'\n', &mut buf).await.unwrap();
            let f: TerminalFrame = serde_json::from_slice(&buf[..buf.len() - 1]).unwrap();
            black_box(&f);
        }
        Instant::now()
    });

    let (start, bytes) = writer.await.unwrap();
    let end = reader.await.unwrap();
    report("jsonl baseline", n, end - start, bytes, 0);
}

/// remoc: frames through an `rch::mpsc` channel whose receiver half was
/// transported over the base channel, like a real `attach_terminal`.
async fn e2e_remoc_mpsc<C: Codec>(name: &str, seq: &[TerminalFrame]) {
    let n = seq.len();
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();
    let (b_r, fwd_bytes) = io_count::CountingRead::new(b_r);
    let (b_w, rev_bytes) = io_count::CountingWrite::new(b_w);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let seq = seq.to_vec();

    // Side A: sessiond — opens the frame channel and pumps frames.
    let side_a = tokio::spawn(async move {
        let (conn, mut tx_base, _rx_base): (
            _,
            rch::base::Sender<rch::mpsc::Receiver<TerminalFrame, C>, C>,
            rch::base::Receiver<(), C>,
        ) = remoc::Connect::io_buffered(remoc::Cfg::default(), a_r, a_w, 256 * 1024)
            .await
            .unwrap();
        tokio::spawn(conn);

        let (ftx, frx) = rch::mpsc::channel::<TerminalFrame, C>(64);
        tx_base.send(frx).await.unwrap();

        ready_rx.await.unwrap();
        let start = Instant::now();
        for f in seq {
            drop(ftx.send(f).await.unwrap());
        }
        start
    });

    // Side B: UI — receives the channel, then counts frames.
    let side_b = tokio::spawn(async move {
        let (conn, _tx_base, mut rx_base): (
            _,
            rch::base::Sender<(), C>,
            rch::base::Receiver<rch::mpsc::Receiver<TerminalFrame, C>, C>,
        ) = remoc::Connect::io_buffered(remoc::Cfg::default(), b_r, b_w, 256 * 1024)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut frx = rx_base.recv().await.unwrap().unwrap();
        ready_tx.send(()).unwrap();
        for _ in 0..n {
            let f = frx.recv().await.unwrap().unwrap();
            black_box(&f);
        }
        Instant::now()
    });

    let start = side_a.await.unwrap();
    let end = side_b.await.unwrap();
    // Let in-flight flow-control traffic quiesce before reading counters.
    tokio::time::sleep(Duration::from_millis(100)).await;
    report(
        name,
        n,
        end - start,
        fwd_bytes.load(Ordering::Relaxed),
        rev_bytes.load(Ordering::Relaxed),
    );
}

/// remoc: frames through an `rch::watch` channel under overload — the
/// sender replaces the value continuously for a fixed duration; the
/// conflating channel delivers only the freshest values. Reported rate
/// is *observed updates per second* (the effective frame-rate ceiling of
/// the watch approach) next to how many values the producer wrote.
async fn e2e_remoc_watch<C: Codec>(name: &str, seq: &[TerminalFrame]) {
    const RUN: Duration = Duration::from_secs(2);
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();
    let (b_r, fwd_bytes) = io_count::CountingRead::new(b_r);
    let (b_w, rev_bytes) = io_count::CountingWrite::new(b_w);

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (sent_tx, sent_rx) = tokio::sync::oneshot::channel();
    let seq = seq.to_vec();
    let sentinel = TerminalFrame::from_text(String::new());

    let side_a = tokio::spawn(async move {
        let (conn, mut tx_base, _rx_base): (
            _,
            rch::base::Sender<rch::watch::Receiver<TerminalFrame, C>, C>,
            rch::base::Receiver<(), C>,
        ) = remoc::Connect::io_buffered(remoc::Cfg::default(), a_r, a_w, 256 * 1024)
            .await
            .unwrap();
        tokio::spawn(conn);

        let first = seq[0].clone();
        let (wtx, wrx) = rch::watch::channel::<TerminalFrame, C>(first);
        tx_base.send(wrx).await.unwrap();

        ready_rx.await.unwrap();
        let start = Instant::now();
        let mut sent = 0u64;
        while start.elapsed() < RUN {
            let mut f = seq[(sent as usize) % seq.len()].clone();
            f.cursor = Some(TerminalCursor {
                row: sent as usize,
                col: 0,
                shape: TerminalCursorShape::Block,
            });
            wtx.send(f).unwrap();
            sent += 1;
            // Yield so the forwarding task interleaves with us.
            tokio::task::yield_now().await;
        }
        wtx.send(sentinel).unwrap();
        let _ = sent_tx.send(sent);
        // Keep the sender half alive until the sentinel is delivered.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let side_b = tokio::spawn(async move {
        let (conn, _tx_base, mut rx_base): (
            _,
            rch::base::Sender<(), C>,
            rch::base::Receiver<rch::watch::Receiver<TerminalFrame, C>, C>,
        ) = remoc::Connect::io_buffered(remoc::Cfg::default(), b_r, b_w, 256 * 1024)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut wrx = rx_base.recv().await.unwrap().unwrap();
        ready_tx.send(()).unwrap();
        let start = Instant::now();
        let mut observed = 0u64;
        let mut last_seq = None;
        loop {
            wrx.changed().await.unwrap();
            let f = wrx.borrow_and_update().unwrap();
            if f.lines.is_empty() {
                break;
            }
            observed += 1;
            last_seq = f.cursor.map(|c| c.row);
            black_box(&*f);
        }
        (start.elapsed(), observed, last_seq)
    });

    let (dur, observed, last_seq) = side_b.await.unwrap();
    let sent = sent_rx.await.unwrap();
    side_a.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!(
        "e2e   {name:<20} producer wrote {sent:>7}, receiver observed {observed:>6} in {dur:.2?} \
         = {:>6.0} obs/s (last seq {last_seq:?})  wire fwd {:>6} B/obs  rev {:>5} B/obs",
        observed as f64 / dur.as_secs_f64(),
        fwd_bytes.load(Ordering::Relaxed) / observed.max(1),
        rev_bytes.load(Ordering::Relaxed) / observed.max(1),
    );
}

/// Demonstrates watch conflation with a deliberately slow receiver:
/// sender paces at ~1 ms/frame, receiver takes ~5 ms per observation.
async fn watch_skip_demo() {
    println!("\n=== watch latest-value-skip demo (sender 1 ms/frame, receiver 5 ms/obs) ===");
    let n = 500usize;
    let frames = synth_frames(80, 24, 50);
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    let side_a = tokio::spawn(async move {
        let (conn, mut tx_base, _rx_base): (
            _,
            rch::base::Sender<rch::watch::Receiver<TerminalFrame>, Postbag>,
            rch::base::Receiver<(), Postbag>,
        ) = remoc::Connect::io_buffered(remoc::Cfg::default(), a_r, a_w, 256 * 1024)
            .await
            .unwrap();
        tokio::spawn(conn);

        let (wtx, wrx) = rch::watch::channel::<TerminalFrame, _>(frames[0].clone());
        tx_base.send(wrx).await.unwrap();

        for i in 0..n {
            let mut f = frames[i % frames.len()].clone();
            f.cursor = Some(TerminalCursor {
                row: i,
                col: 0,
                shape: TerminalCursorShape::Block,
            });
            wtx.send(f).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        wtx.send(TerminalFrame::from_text(String::new())).unwrap();
        // Keep the sender alive until the receiver saw the sentinel.
        tokio::time::sleep(Duration::from_secs(10)).await;
    });

    let side_b = tokio::spawn(async move {
        let (conn, _tx_base, mut rx_base): (
            _,
            rch::base::Sender<(), Postbag>,
            rch::base::Receiver<rch::watch::Receiver<TerminalFrame>, Postbag>,
        ) = remoc::Connect::io_buffered(remoc::Cfg::default(), b_r, b_w, 256 * 1024)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut wrx = rx_base.recv().await.unwrap().unwrap();
        let mut observed_seqs = Vec::new();
        loop {
            wrx.changed().await.unwrap();
            let (empty, seq) = {
                let f = wrx.borrow_and_update().unwrap();
                (f.lines.is_empty(), f.cursor.map(|c| c.row))
            };
            if empty {
                break;
            }
            observed_seqs.push(seq.unwrap());
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        observed_seqs
    });

    let observed = side_b.await.unwrap();
    side_a.abort();
    println!(
        "sent {n} frames; slow receiver observed {} updates, first 10 seqs {:?}, last seq {:?} \
         (skips intermediate values, terminal sentinel delivered)",
        observed.len(),
        &observed[..observed.len().min(10)],
        observed.last(),
    );
}

fn report(name: &str, n: usize, dur: Duration, fwd_bytes: u64, rev_bytes: u64) {
    println!(
        "e2e   {name:<20} {n:>5} frames in {dur:>8.2?}  {:>9.0} fps  wire fwd {:>6} B/frame  rev {:>5} B/frame",
        n as f64 / dur.as_secs_f64(),
        fwd_bytes / n as u64,
        rev_bytes / n as u64,
    );
}
