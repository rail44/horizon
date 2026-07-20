//! Item 2: version-skew behavior of the Postbag codec (remoc default),
//! at codec level and through live rch channels.
//!
//! Run with `cargo test --test skew -- --nocapture` to see the exact
//! error messages recorded in the spike report.

use remoc::{
    codec::{Codec, DeserializationError, Postbag},
    rch,
};
use remoc_spike::skew::{
    CommandV1, CommandV1Defended, CommandV2, FrameMetaV1, FrameMetaV2, FrameMetaV2Strict,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::net::UnixStream;

fn pb_encode<T: Serialize>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    <Postbag as Codec>::serialize(&mut buf, value).unwrap();
    buf
}

fn pb_decode<T: DeserializeOwned>(buf: &[u8]) -> Result<T, DeserializationError> {
    <Postbag as Codec>::deserialize(buf)
}

fn meta_v1() -> FrameMetaV1 {
    FrameMetaV1 {
        rows: 24,
        cols: 80,
        title: "zsh".into(),
    }
}

fn meta_v2() -> FrameMetaV2 {
    FrameMetaV2 {
        rows: 24,
        cols: 80,
        title: "zsh".into(),
        zoom: Some(1.5),
        alt_screen: true,
    }
}

/// (a) V2 sender -> V1 receiver: unknown fields must be skipped.
#[test]
fn a_field_added_v2_to_v1() {
    let decoded: FrameMetaV1 = pb_decode(&pb_encode(&meta_v2())).expect("V2 -> V1 must decode");
    assert_eq!(decoded, meta_v1());
    println!("(a) V2 -> V1: OK, extra fields (zoom, alt_screen) ignored: {decoded:?}");
}

/// (b) V1 sender -> V2 receiver: missing fields must take serde defaults.
#[test]
fn b_field_missing_v1_to_v2() {
    let decoded: FrameMetaV2 = pb_decode(&pb_encode(&meta_v1())).expect("V1 -> V2 must decode");
    assert_eq!(decoded.zoom, None);
    assert!(!decoded.alt_screen);
    println!("(b) V1 -> V2: OK, #[serde(default)] applied: {decoded:?}");
}

/// (b') V1 sender -> V2 receiver where the added field has NO
/// #[serde(default)]: expected to fail; records the exact error.
#[test]
fn b_field_missing_without_default() {
    let res: Result<FrameMetaV2Strict, _> = pb_decode(&pb_encode(&meta_v1()));
    match res {
        Ok(v) => println!("(b') V1 -> V2Strict unexpectedly decoded: {v:?}"),
        Err(err) => println!("(b') V1 -> V2Strict error (as expected): {err}"),
    }
}

/// (c) new enum variant sent to the old side: must error; and the
/// #[serde(other)] fallback, if it works under Postbag, must catch it.
#[test]
fn c_new_enum_variant_to_old_side() {
    // Known variants stay interchangeable.
    let known: CommandV1 = pb_decode(&pb_encode(&CommandV2::Resize {
        rows: 50,
        cols: 200,
    }))
    .expect("shared variant must decode");
    assert_eq!(
        known,
        CommandV1::Resize {
            rows: 50,
            cols: 200
        }
    );

    // Unknown variant -> plain V1.
    let res: Result<CommandV1, _> = pb_decode(&pb_encode(&CommandV2::Scroll(-3)));
    match &res {
        Ok(v) => println!("(c) Scroll -> V1 unexpectedly decoded: {v:?}"),
        Err(err) => println!("(c) Scroll -> V1 error (as expected): {err}"),
    }
    assert!(
        res.is_err(),
        "unknown variant should not decode into plain V1"
    );

    // Unknown variant -> V1 with #[serde(other)] catch-all.
    let res: Result<CommandV1Defended, _> = pb_decode(&pb_encode(&CommandV2::Scroll(-3)));
    match &res {
        Ok(v) => println!("(c) Scroll -> V1Defended decoded as: {v:?}"),
        Err(err) => println!("(c) Scroll -> V1Defended error: {err}"),
    }
}

/// (c2) same skew but through a live rch::mpsc channel: does one unknown
/// variant kill the channel, or can the receiver keep consuming?
#[tokio::test]
async fn c2_enum_skew_through_mpsc_channel() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    // Old side (V1 receiver).
    let old = tokio::spawn(async move {
        let (conn, _tx, mut rx): (
            _,
            rch::base::Sender<(), Postbag>,
            rch::base::Receiver<rch::mpsc::Receiver<CommandV1, Postbag>, Postbag>,
        ) = remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut cmds = rx.recv().await.unwrap().unwrap();
        let mut log = Vec::new();
        loop {
            match cmds.recv().await {
                Ok(Some(cmd)) => log.push(format!("ok: {cmd:?}")),
                Ok(None) => break,
                Err(err) => log.push(format!("recv error: {err}")),
            }
            if log.len() > 8 {
                break;
            }
        }
        log
    });

    // New side (V2 sender).
    let (conn, mut tx, _rx): (
        _,
        rch::base::Sender<rch::mpsc::Receiver<CommandV2, Postbag>, Postbag>,
        rch::base::Receiver<(), Postbag>,
    ) = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (ctx, crx) = rch::mpsc::channel::<CommandV2, Postbag>(8);
    tx.send(crx).await.unwrap();
    for cmd in [
        CommandV2::Key("a".into()),
        CommandV2::Scroll(-3),
        CommandV2::Paste("after unknown".into()),
    ] {
        drop(ctx.send(cmd).await.unwrap());
    }
    drop(ctx);

    let log = old.await.unwrap();
    println!("(c2) V2 -> V1 over rch::mpsc, receive log:");
    for entry in &log {
        println!("     {entry}");
    }
    // Recorded outcome is asserted loosely; the printed log is the datum.
    assert!(
        log.iter().any(|l| l.contains("Key")),
        "known variant must arrive"
    );
}

// ---- (d) structs that carry rch channel halves ------------------------

#[derive(Debug, Serialize, Deserialize)]
struct AttachV1 {
    frames: rch::mpsc::Receiver<u32, Postbag>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AttachV2 {
    frames: rch::mpsc::Receiver<u32, Postbag>,
    #[serde(default)]
    label: Option<String>,
}

/// (d) field skew on a struct that itself transports an rch channel:
/// V2 -> V1 (field ignored) and the channel must still work end to end.
#[tokio::test]
async fn d_channel_bearing_struct_v2_to_v1() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    let old = tokio::spawn(async move {
        let (conn, _tx, mut rx): (
            _,
            rch::base::Sender<(), Postbag>,
            rch::base::Receiver<AttachV1, Postbag>,
        ) = remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut attach = rx.recv().await.unwrap().unwrap();
        let mut got = Vec::new();
        while let Ok(Some(v)) = attach.frames.recv().await {
            got.push(v);
        }
        got
    });

    let (conn, mut tx, _rx): (
        _,
        rch::base::Sender<AttachV2, Postbag>,
        rch::base::Receiver<(), Postbag>,
    ) = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (ftx, frx) = rch::mpsc::channel::<u32, Postbag>(8);
    tx.send(AttachV2 {
        frames: frx,
        label: Some("ignored by V1".into()),
    })
    .await
    .unwrap();
    for v in [1u32, 2, 3] {
        drop(ftx.send(v).await.unwrap());
    }
    drop(ftx);

    let got = old.await.unwrap();
    assert_eq!(got, vec![1, 2, 3]);
    println!("(d) V2 struct w/ channel -> V1: OK, extra field ignored, channel live: {got:?}");
}

/// (d') the reverse: V1 -> V2, added field defaulted, channel still live.
#[tokio::test]
async fn d_channel_bearing_struct_v1_to_v2() {
    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    let new = tokio::spawn(async move {
        let (conn, _tx, mut rx): (
            _,
            rch::base::Sender<(), Postbag>,
            rch::base::Receiver<AttachV2, Postbag>,
        ) = remoc::Connect::io(remoc::Cfg::default(), b_r, b_w)
            .await
            .unwrap();
        tokio::spawn(conn);

        let mut attach = rx.recv().await.unwrap().unwrap();
        let mut got = Vec::new();
        while let Ok(Some(v)) = attach.frames.recv().await {
            got.push(v);
        }
        (attach.label, got)
    });

    let (conn, mut tx, _rx): (
        _,
        rch::base::Sender<AttachV1, Postbag>,
        rch::base::Receiver<(), Postbag>,
    ) = remoc::Connect::io(remoc::Cfg::default(), a_r, a_w)
        .await
        .unwrap();
    tokio::spawn(conn);

    let (ftx, frx) = rch::mpsc::channel::<u32, Postbag>(8);
    tx.send(AttachV1 { frames: frx }).await.unwrap();
    for v in [7u32, 8] {
        drop(ftx.send(v).await.unwrap());
    }
    drop(ftx);

    let (label, got) = new.await.unwrap();
    assert_eq!(label, None);
    assert_eq!(got, vec![7, 8]);
    println!("(d') V1 struct w/ channel -> V2: OK, label defaulted to None, channel live: {got:?}");
}
