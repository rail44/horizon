//! Item 3: cross-version experiment, new side — remoc **0.18.3**
//! (chmux protocol version 3, default codec Postbag).
//!
//! Listens on the unix socket given as argv[1] and accepts one
//! connection from `old_peer` (remoc 0.16.1).
//!
//! Default mode uses the JSON codec explicitly (both remoc versions
//! carry it): receives the hello struct with its live channel, drains
//! the 100 numbers, responds. `--postbag` mode keeps remoc 0.18's
//! *default* codec instead, demonstrating what happens when a 0.18
//! default-configuration binary meets a pre-0.18 default-configuration
//! binary: chmux connects, data decoding fails.

use remoc::{codec::Json, codec::Postbag, rch};
use serde::{Deserialize, Serialize};
use tokio::net::UnixListener;

#[derive(Serialize, Deserialize)]
struct HelloJson {
    text: String,
    nums: rch::mpsc::Receiver<u32, Json>,
}

#[derive(Serialize, Deserialize)]
struct HelloPostbag {
    text: String,
    nums: rch::mpsc::Receiver<u32, Postbag>,
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: new_peer <socket> [--postbag]");
    let mismatch = args.next().as_deref() == Some("--postbag");

    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind");
    let (socket, _) = listener.accept().await.expect("accept");
    let (r, w) = socket.into_split();

    if mismatch {
        // remoc 0.18 defaults (Postbag) vs remoc 0.16 defaults (JSON).
        let (conn, _tx, mut rx): (
            _,
            rch::base::Sender<String, Postbag>,
            rch::base::Receiver<HelloPostbag, Postbag>,
        ) = remoc::Connect::io(remoc::Cfg::default(), r, w)
            .await
            .expect("remoc 0.18 connect (chmux layer)");
        tokio::spawn(conn);
        println!("NEW-PEER: chmux connection established (codec mismatch mode)");

        match rx.recv().await {
            Ok(v) => println!(
                "NEW-PEER: unexpectedly decoded hello: {:?}",
                v.map(|h| h.text)
            ),
            Err(err) => println!("NEW-PEER: EXPECTED-MISMATCH-ERROR: {err}"),
        }
        return;
    }

    let (conn, mut tx, mut rx): (
        _,
        rch::base::Sender<String, Json>,
        rch::base::Receiver<HelloJson, Json>,
    ) = remoc::Connect::io(remoc::Cfg::default(), r, w)
        .await
        .expect("remoc 0.18 connect");
    tokio::spawn(conn);

    let mut hello = rx.recv().await.expect("recv hello").expect("hello");
    let mut sum = 0u64;
    let mut count = 0u32;
    while let Some(v) = hello.nums.recv().await.expect("recv num") {
        sum += u64::from(v);
        count += 1;
    }

    tx.send(format!(
        "remoc 0.18.3 (chmux protocol v{}) got '{}' and {count} nums (sum {sum}) over the 0.16-opened channel",
        remoc::chmux::PROTOCOL_VERSION,
        hello.text
    ))
    .await
    .expect("send response");

    println!("NEW-PEER: received hello text: {}", hello.text);
    println!("NEW-PEER: nums count {count} sum {sum}");
    println!("NEW-PEER: PASS");

    // Give the response time to flush before the process exits.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}
