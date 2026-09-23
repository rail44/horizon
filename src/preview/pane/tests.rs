use super::*;
use embedded_gpui::{encode, Receipt};
use gpui::TestAppContext;

#[gpui::test]
fn reload_discards_pending_preview_names_and_accepts_the_next_response(cx: &mut TestAppContext) {
    let pane = cx.new(|cx| PreviewPane::new(None, "sample".into(), cx));
    let (old_sender, old_names) = Receipt::channel();
    pane.update(cx, |pane, cx| {
        pane.receive_preview_names(old_names.decoded(), cx)
    });
    cx.executor().run_until_parked();

    pane.update(cx, |pane, cx| pane.reload(cx));
    old_sender.send(Ok(encode(&vec!["stale"]).unwrap())).ok();
    cx.executor().run_until_parked();
    pane.read_with(cx, |pane, _| assert_eq!(pane.status, Status::Empty));

    let (new_sender, new_names) = Receipt::channel();
    pane.update(cx, |pane, cx| {
        pane.receive_preview_names(new_names.decoded(), cx)
    });
    new_sender
        .send(Ok(encode(&vec!["current"]).unwrap()))
        .unwrap();
    cx.executor().run_until_parked();
    pane.read_with(cx, |pane, _| {
        assert_eq!(
            pane.status,
            Status::Loaded {
                previews: vec!["current".into()]
            }
        )
    });
}

#[gpui::test]
fn unavailable_preview_names_leave_the_existing_status_intact(cx: &mut TestAppContext) {
    let pane = cx.new(|cx| PreviewPane::new(None, "sample".into(), cx));
    pane.update(cx, |pane, cx| {
        pane.status = Status::Loaded {
            previews: Vec::new(),
        };
        pane.receive_preview_names(Receipt::dropped(), cx);
    });
    cx.executor().run_until_parked();
    pane.read_with(cx, |pane, _| {
        assert_eq!(
            pane.status,
            Status::Loaded {
                previews: Vec::new()
            }
        )
    });
}
