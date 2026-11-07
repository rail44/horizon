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

// --- the watcher's pure decisions ---------------------------------------

#[test]
fn a_watch_is_placed_on_the_files_directory_not_the_file() {
    let file = Path::new("/tmp/plugin/target/wasm32-wasip2/quick/p.wasm");
    assert_eq!(
        watch_root(file),
        Some(Path::new("/tmp/plugin/target/wasm32-wasip2/quick"))
    );
}

#[test]
fn a_bare_filename_has_no_directory_to_watch() {
    assert_eq!(watch_root(Path::new("p.wasm")), None);
}

#[test]
fn only_events_naming_the_file_count() {
    let file = PathBuf::from("/tmp/out/p.wasm");
    let sibling = PathBuf::from("/tmp/out/p.d");
    assert!(event_touches(&[sibling.clone(), file.clone()], &file));
    assert!(!event_touches(&[sibling], &file));
    assert!(!event_touches(&[], &file));
}

#[test]
fn reading_the_file_does_not_count_as_a_change() {
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, RemoveKind, RenameMode,
    };
    // Loading a plugin or a document opens and reads the file. If those
    // access events scheduled a reload, every load would start the next one.
    for kind in [
        EventKind::Access(AccessKind::Open(AccessMode::Any)),
        EventKind::Access(AccessKind::Close(AccessMode::Read)),
        EventKind::Access(AccessKind::Close(AccessMode::Write)),
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
        EventKind::Any,
        EventKind::Other,
    ] {
        assert!(!event_changes_content(&kind), "{kind:?}");
    }
    // What a cargo rebuild or an editor's atomic save produces: the old
    // file unlinked or renamed over, the new one linked or written in its
    // place.
    for kind in [
        EventKind::Remove(RemoveKind::File),
        EventKind::Create(CreateKind::File),
        EventKind::Modify(ModifyKind::Data(DataChange::Any)),
        EventKind::Modify(ModifyKind::Name(RenameMode::To)),
    ] {
        assert!(event_changes_content(&kind), "{kind:?}");
    }
}
