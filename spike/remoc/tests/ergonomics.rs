//! Item 4: ergonomics of `#[rtc::remote]` with channel-bearing return
//! structs, and what each channel half reports when the connection dies.
//!
//! Run with `cargo test --test ergonomics -- --nocapture`.

use std::{sync::Arc, time::Duration};

use horizon_terminal_core::TerminalFrame;
use remoc::{codec, prelude::*};
use remoc_spike::{
    frames::synth_frames,
    svc::{
        TerminalAttachment, TerminalInput, TerminalService, TerminalServiceClient,
        TerminalServiceServerSharedMut,
    },
};
use tokio::{net::UnixStream, sync::RwLock};

struct Sessiond {
    frames: Vec<TerminalFrame>,
    frame_tx: Option<rch::watch::Sender<TerminalFrame>>,
    input_log: Arc<std::sync::Mutex<Vec<String>>>,
}

impl TerminalService for Sessiond {
    async fn attach_terminal(
        &mut self,
    ) -> Result<TerminalAttachment, remoc_spike::svc::AttachError> {
        let (ftx, frx) = rch::watch::channel(self.frames[0].clone());
        self.frame_tx = Some(ftx);

        let (itx, mut irx) = rch::mpsc::channel::<TerminalInput, _>(16);
        let log = self.input_log.clone();
        tokio::spawn(async move {
            loop {
                match irx.recv().await {
                    Ok(Some(input)) => log.lock().unwrap().push(format!("input: {input:?}")),
                    Ok(None) => {
                        log.lock().unwrap().push("input channel closed".into());
                        break;
                    }
                    Err(err) => {
                        log.lock().unwrap().push(format!("input recv error: {err}"));
                        break;
                    }
                }
            }
        });

        Ok(TerminalAttachment {
            frames: frx,
            input: itx,
        })
    }
}

#[tokio::test]
async fn rtc_attach_and_disconnect() {
    let frames = synth_frames(80, 24, 4);
    let input_log = Arc::new(std::sync::Mutex::new(Vec::new()));

    let (a, b) = UnixStream::pair().unwrap();
    let (a_r, a_w) = a.into_split();
    let (b_r, b_w) = b.into_split();

    // ---- server (sessiond) side ----
    let obj = Arc::new(RwLock::new(Sessiond {
        frames: frames.clone(),
        frame_tx: None,
        input_log: input_log.clone(),
    }));
    let (server, client) = TerminalServiceServerSharedMut::<_, codec::Default>::new(obj.clone(), 1);

    // Both Connect::io handshakes must run concurrently.
    let (server_side, client_side): (
        Result<
            (
                _,
                rch::base::Sender<TerminalServiceClient<codec::Default>>,
                rch::base::Receiver<()>,
            ),
            _,
        >,
        Result<
            (
                _,
                rch::base::Sender<()>,
                rch::base::Receiver<TerminalServiceClient<codec::Default>>,
            ),
            _,
        >,
    ) = tokio::join!(
        remoc::Connect::io(remoc::Cfg::default(), a_r, a_w),
        remoc::Connect::io(remoc::Cfg::default(), b_r, b_w),
    );
    let (conn, mut tx_base, _rx_base) = server_side.unwrap();
    let server_conn = tokio::spawn(conn);
    tx_base.send(client).await.unwrap();
    let server_task = tokio::spawn(async move { server.serve(true).await });

    // ---- client (UI) side ----
    let (conn, _tx_base, mut rx_base) = client_side.unwrap();
    tokio::spawn(conn);
    let mut client = rx_base.recv().await.unwrap().unwrap();

    // Call the remote method; get back a struct with two live channels.
    let mut attachment = client.attach_terminal().await.unwrap();

    // Frame path works.
    obj.write()
        .await
        .frame_tx
        .as_ref()
        .unwrap()
        .send(frames[1].clone())
        .unwrap();
    attachment.frames.changed().await.unwrap();
    assert_eq!(*attachment.frames.borrow_and_update().unwrap(), frames[1]);

    // Input path works.
    drop(
        attachment
            .input
            .send(TerminalInput::Bytes(b"ls\n".to_vec()))
            .await
            .unwrap(),
    );
    drop(
        attachment
            .input
            .send(TerminalInput::Resize {
                rows: 50,
                cols: 200,
            })
            .await
            .unwrap(),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    println!("input log at server: {:?}", input_log.lock().unwrap());

    // ---- sever the connection (daemon dies) ----
    server_conn.abort();
    server_task.abort();
    drop(obj);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // What does the watch receiver report?
    match attachment.frames.changed().await {
        Ok(()) => {
            let v = attachment.frames.borrow_and_update();
            println!(
                "after disconnect: watch changed() -> Ok, borrow: {:?}",
                v.map(|f| f.lines.len())
            );
        }
        Err(err) => println!("after disconnect: watch changed() error: {err}"),
    }
    // Last known value should still be borrowable (or an error — record it).
    match attachment.frames.borrow() {
        Ok(f) => println!(
            "after disconnect: watch borrow() still serves last frame ({} lines)",
            f.lines.len()
        ),
        Err(err) => println!("after disconnect: watch borrow() error: {err}"),
    }

    // What does the mpsc sender report?
    match attachment
        .input
        .send(TerminalInput::Bytes(b"x".to_vec()))
        .await
    {
        Ok(_) => println!("after disconnect: input send unexpectedly Ok"),
        Err(err) => println!(
            "after disconnect: input send error: {err} (closed={}, disconnected={})",
            err.is_closed(),
            err.is_disconnected()
        ),
    }

    // What does an rtc call report?
    match client.attach_terminal().await {
        Ok(_) => println!("after disconnect: rtc call unexpectedly Ok"),
        Err(err) => println!("after disconnect: rtc call error: {err:?}"),
    }
}
