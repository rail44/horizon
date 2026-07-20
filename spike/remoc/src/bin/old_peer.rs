//! Item 3: cross-version experiment, old side — remoc **0.16.1**
//! (chmux protocol version 3, default codec JSON at the time).
//!
//! Connects to the unix socket given as argv[1], opens a base channel
//! with explicit JSON codec, sends a hello struct carrying a live
//! `rch::mpsc` receiver, pumps 0..100 through it, then waits for the
//! response string (skipped with `--no-response` for the codec-mismatch
//! run, where the new side is expected to fail decoding).

use remoc_016::{codec::Json, rch};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

#[derive(Serialize, Deserialize)]
struct Hello {
    text: String,
    nums: rch::mpsc::Receiver<u32, Json>,
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: old_peer <socket> [--no-response]");
    let no_response = args.next().as_deref() == Some("--no-response");

    let socket = UnixStream::connect(&path).await.expect("connect");
    let (r, w) = socket.into_split();

    let (conn, mut tx, mut rx): (
        _,
        rch::base::Sender<Hello, Json>,
        rch::base::Receiver<String, Json>,
    ) = remoc_016::Connect::io(remoc_016::Cfg::default(), r, w)
        .await
        .expect("remoc 0.16 connect");
    tokio::spawn(conn);

    let (ntx, nrx) = rch::mpsc::channel::<u32, Json>(16);
    let hello = Hello {
        text: format!(
            "hello from remoc {} (chmux protocol v{})",
            "0.16.1",
            remoc_016::chmux::PROTOCOL_VERSION
        ),
        nums: nrx,
    };
    if let Err(err) = tx.send(hello).await {
        panic!("send hello: {err}");
    }

    for i in 0..100u32 {
        ntx.send(i).await.expect("send num");
    }
    drop(ntx);

    if no_response {
        println!("OLD-PEER: hello + 100 nums sent (not awaiting response)");
        return;
    }

    let response = rx.recv().await.expect("recv response").expect("response");
    println!("OLD-PEER: got response: {response}");
    println!("OLD-PEER: PASS");
}
