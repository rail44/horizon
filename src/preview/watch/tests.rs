use super::*;
use gpui::TestAppContext;
use std::cell::Cell;
use std::rc::Rc;

fn start_debounce(
    cx: &mut TestAppContext,
) -> (mpsc::UnboundedSender<()>, Rc<Cell<usize>>, Task<()>) {
    let (sender, receiver) = mpsc::unbounded();
    let reloads = Rc::new(Cell::new(0));
    let count = reloads.clone();
    let task = cx.update(|cx| {
        cx.spawn(async move |cx| {
            debounce_loop(receiver, cx, &mut |_| count.set(count.get() + 1)).await;
        })
    });
    (sender, reloads, task)
}

#[gpui::test]
fn a_burst_reloads_once_after_the_last_event_and_accepts_later_bursts(cx: &mut TestAppContext) {
    let (sender, reloads, _task) = start_debounce(cx);
    sender.unbounded_send(()).unwrap();
    cx.executor().run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    sender.unbounded_send(()).unwrap();
    cx.executor().run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.executor().run_until_parked();
    assert_eq!(reloads.get(), 0, "the relink restarts the quiet interval");
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.executor().run_until_parked();
    assert_eq!(reloads.get(), 1);
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.executor().run_until_parked();
    assert_eq!(reloads.get(), 1, "idle time must not reload repeatedly");
    sender.unbounded_send(()).unwrap();
    cx.executor().run_until_parked();
    cx.executor().advance_clock(RELOAD_DEBOUNCE);
    cx.executor().run_until_parked();
    assert_eq!(reloads.get(), 2);
}

#[gpui::test]
fn closing_the_watch_source_discards_a_pending_reload(cx: &mut TestAppContext) {
    let (sender, reloads, _task) = start_debounce(cx);
    sender.unbounded_send(()).unwrap();
    cx.executor().run_until_parked();
    drop(sender);
    cx.executor().run_until_parked();
    cx.executor().advance_clock(RELOAD_DEBOUNCE);
    cx.executor().run_until_parked();
    assert_eq!(reloads.get(), 0);
}

#[gpui::test]
fn dropping_the_watch_task_discards_a_pending_reload(cx: &mut TestAppContext) {
    let (sender, reloads, task) = start_debounce(cx);
    sender.unbounded_send(()).unwrap();
    cx.executor().run_until_parked();
    drop(task);
    cx.executor().run_until_parked();
    cx.executor().advance_clock(RELOAD_DEBOUNCE);
    cx.executor().run_until_parked();
    assert_eq!(reloads.get(), 0);
    assert!(sender.unbounded_send(()).is_err());
}
