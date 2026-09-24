use super::*;
use embedded_gpui::{encode, Receipt};
use gpui::TestAppContext;

#[gpui::test]
fn retiring_a_load_cancels_its_pending_names(cx: &mut TestAppContext) {
    let pane = cx.new(|cx| PreviewPane::new(None, "sample".into(), cx));
    let (old_sender, old_names) = Receipt::channel();
    let old = pane.update(cx, |_, cx| PreviewNames::new(old_names.decoded(), cx));
    let retired_values = old.values.clone();
    cx.executor().run_until_parked();
    drop(old);

    let (new_sender, new_names) = Receipt::channel();
    let current = pane.update(cx, |_, cx| PreviewNames::new(new_names.decoded(), cx));
    old_sender.send(Ok(encode(&vec!["stale"]).unwrap())).ok();
    new_sender
        .send(Ok(encode(&vec!["current"]).unwrap()))
        .unwrap();
    cx.executor().run_until_parked();
    assert!(retired_values.borrow().is_empty());
    assert_eq!(*current.values.borrow(), vec!["current"]);
    pane.read_with(cx, |pane, _| assert_eq!(pane.status(), Status::Empty));
}

#[gpui::test]
fn unavailable_preview_names_keep_the_list_empty(cx: &mut TestAppContext) {
    let pane = cx.new(|cx| PreviewPane::new(None, "sample".into(), cx));
    let names = pane.update(cx, |_, cx| PreviewNames::new(Receipt::dropped(), cx));
    cx.executor().run_until_parked();
    assert!(names.values.borrow().is_empty());
}

#[gpui::test]
fn reload_cancels_an_inflight_load(cx: &mut TestAppContext) {
    let pane = cx.new(|cx| PreviewPane::new(None, "sample".into(), cx));
    let (sender, pending) = Receipt::channel();
    let pending: Receipt<Vec<String>> = pending.decoded();
    let completed = Rc::new(RefCell::new(false));
    let observed = completed.clone();
    pane.update(cx, |pane, cx| {
        pane.load = LoadState::Loading {
            _task: cx.spawn(async move |_, _| {
                let _ = pending.await;
                *observed.borrow_mut() = true;
            }),
        };
    });
    cx.executor().run_until_parked();
    pane.update(cx, |pane, cx| pane.reload(cx));
    sender.send(Ok(encode(&Vec::<String>::new()).unwrap())).ok();
    cx.executor().run_until_parked();
    assert!(!*completed.borrow());
    pane.read_with(cx, |pane, _| assert_eq!(pane.status(), Status::Empty));
}

#[gpui::test]
#[ignore = "requires the built preview component; run scripts/check-preview-plugin.sh"]
fn preview_plugin_pane_releases_loaded_resources_on_reload_and_retarget(cx: &mut TestAppContext) {
    use crate::preview::e2e::ProbeTextSystem;
    use embedded_gpui::PluginInstance;

    let artifact = PathBuf::from(std::env::var_os("HORIZON_PREVIEW_WASM").unwrap());
    let pane = cx.new(|cx| PreviewPane::new(None, "sample".into(), cx));
    for retarget in [false, true] {
        let text_system = Arc::new(ProbeTextSystem);
        let lifetime = Arc::downgrade(&text_system);
        let instance = PluginInstance::new(&artifact, PluginOptions::new(text_system)).unwrap();
        let host = cx.new(|cx| PluginHost::new(instance, cx));
        pane.update(cx, |pane, cx| pane.attach(host, cx));
        assert!(lifetime.upgrade().is_some());
        pane.read_with(cx, |pane, _| {
            assert!(matches!(pane.status(), Status::Loaded { .. }))
        });
        pane.update(cx, |pane, cx| {
            if retarget {
                // No global text system is installed: the new target fails,
                // and must still retire all resources from the previous load.
                pane.retarget(artifact.clone(), "next".into(), cx);
            } else {
                pane.reload(cx);
            }
        });
        cx.executor().run_until_parked();
        assert!(
            lifetime.upgrade().is_none(),
            "retired guest still owns its text system"
        );
        pane.read_with(cx, |pane, _| {
            if retarget {
                assert!(matches!(pane.status(), Status::Failed(_)));
            } else {
                assert_eq!(pane.status(), Status::Empty);
            }
        });
    }
    // Exercise the real asynchronous completion too: retiring the Loading
    // state's task from inside its own callback must leave a usable guest.
    cx.update(|cx| cx.set_global(PreviewTextSystem(Arc::new(ProbeTextSystem))));
    pane.update(cx, |pane, cx| pane.retarget(artifact, "sample".into(), cx));
    cx.executor().run_until_parked();
    pane.read_with(cx, |pane, _| {
        assert!(
            matches!(pane.status(), Status::Loaded { .. }),
            "{:?}",
            pane.status()
        );
    });
    pane.update(cx, |pane, cx| pane.reload(cx));
    cx.executor().run_until_parked();
    pane.read_with(cx, |pane, _| {
        assert!(
            matches!(pane.status(), Status::Loaded { .. }),
            "{:?}",
            pane.status()
        );
    });
}
