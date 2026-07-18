//! `PlatformDispatcher` backed by winit's `EventLoopProxy`. Ported from the
//! spike (`spikes/gpui-winit/src/dispatcher.rs`, retired; lives in git
//! history) with no behavioral change —
//! see docs/research/winit-backend-spike.md §6.1 for why this mapping was
//! the "expected" part of the spike (unlike the `ActiveEventLoop`
//! reachability problem in `active_loop.rs`).
//!
//! gpui splits work into three lanes: background (any thread), main-thread
//! (must run on the window-system thread), and realtime (its own thread,
//! for audio-style deadlines we don't exercise here). gpui_linux solves the
//! main-thread lane with a calloop ping source woken from any thread; the
//! winit analogue is `EventLoopProxy::send_event`, which is documented as
//! safe to call from any thread and wakes `ApplicationHandler::user_event`
//! on the event-loop thread.
//!
//! Background work uses a small fixed thread pool draining a shared
//! priority queue (mirrors `gpui_linux`'s `LinuxDispatcher`); `dispatch_after`
//! spawns a one-shot sleeping thread rather than a proper timer wheel —
//! acceptable for the number/granularity of timers gpui itself schedules
//! (animation frame pacing, debounced UI callbacks), not a general-purpose
//! scheduler.

use gpui::{PlatformDispatcher, Priority, RunnableVariant};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use winit::event_loop::EventLoopProxy;

use crate::app_handler::WinitUserEvent;
use crate::queue::{PriorityQueueReceiver, PriorityQueueSender};

const BACKGROUND_THREADS: usize = 2;

pub(crate) struct WinitDispatcher {
    main_thread_id: thread::ThreadId,
    // `PriorityQueueSender` doesn't implement `Clone`; wrap it so
    // `dispatch_after`'s sleeper thread can hold its own handle.
    main_sender: Arc<PriorityQueueSender<RunnableVariant>>,
    main_receiver: Mutex<PriorityQueueReceiver<RunnableVariant>>,
    background_sender: PriorityQueueSender<RunnableVariant>,
    proxy: EventLoopProxy<WinitUserEvent>,
    _background_threads: Vec<thread::JoinHandle<()>>,
}

impl WinitDispatcher {
    pub(crate) fn new(proxy: EventLoopProxy<WinitUserEvent>) -> Self {
        let (main_sender, main_receiver) = PriorityQueueReceiver::new();
        let (background_sender, background_receiver): (_, PriorityQueueReceiver<RunnableVariant>) =
            PriorityQueueReceiver::new();

        let background_threads = (0..BACKGROUND_THREADS)
            .map(|i| {
                let receiver = background_receiver.clone();
                thread::Builder::new()
                    .name(format!("horizon-winit-worker-{i}"))
                    .spawn(move || {
                        for runnable in receiver.iter() {
                            let _ = runnable.run();
                        }
                    })
                    .expect("failed to spawn background worker thread")
            })
            .collect();

        Self {
            main_thread_id: thread::current().id(),
            main_sender: Arc::new(main_sender),
            main_receiver: Mutex::new(main_receiver),
            background_sender,
            proxy,
            _background_threads: background_threads,
        }
    }

    /// Runs every runnable currently queued for the main thread. Called
    /// from the winit `ApplicationHandler` after any callback that could
    /// have been woken by a `dispatch_on_main_thread` call (`user_event`,
    /// `about_to_wait`), plus once more before we hand control to
    /// `on_finish_launching` so early-queued work isn't stranded.
    pub(crate) fn drain_main_queue(&self) {
        loop {
            let popped = self
                .main_receiver
                .lock()
                .expect("main dispatch queue poisoned")
                .try_pop();
            match popped {
                Ok(Some(runnable)) => {
                    let _ = runnable.run();
                }
                Ok(None) | Err(_) => break,
            }
        }
    }
}

impl PlatformDispatcher for WinitDispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: RunnableVariant, priority: Priority) {
        self.background_sender
            .send(priority, runnable)
            .unwrap_or_else(|_| log::error!("dispatch: background queue receiver gone"));
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority) {
        if self.main_sender.send(priority, runnable).is_err() {
            log::error!("dispatch_on_main_thread: main queue receiver gone");
            return;
        }
        // Safe from any thread per winit's EventLoopProxy docs; wakes
        // ApplicationHandler::user_event on the event-loop thread, which
        // then calls drain_main_queue.
        if self.proxy.send_event(WinitUserEvent::Wake).is_err() {
            log::warn!("dispatch_on_main_thread: event loop already exited");
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        let main_sender = Arc::clone(&self.main_sender);
        let proxy = self.proxy.clone();
        thread::spawn(move || {
            thread::sleep(duration);
            if main_sender.send(Priority::High, runnable).is_ok() {
                proxy.send_event(WinitUserEvent::Wake).ok();
            }
        });
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        thread::spawn(f);
    }
}

// `EventLoop::new` refuses off-main-thread construction by default on
// Linux/Windows (a real cross-platform hazard for production code, see
// winit's own panic message) — but `cargo test`/nextest always run `#[test]`
// bodies on a harness-spawned thread, never the process's literal main
// thread, even one-test-per-process. `with_any_thread(true)` is the
// documented, platform-gated escape hatch for exactly this (test-harness)
// situation; there's no cross-platform equivalent (macOS's Cocoa loop has
// none), so this test — and the `winit`/`async-task` test-only imports it
// needs — stays Linux-only, matching where this crate's Wayland freeze (and
// the dispatcher-side hazard this test rules out) actually lives.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    //! Regression coverage for the wakeup invariant this crate's freeze
    //! investigation (docs/winit-backend-design.md's "Resolved incidents" ("Configure stall")
    //! section) needed to rule out: does `dispatch_on_main_thread` ever
    //! drop a post instead of waking the loop? Root-caused elsewhere (a
    //! Wayland frame-callback ordering bug in `window.rs`, not here), but
    //! this seam is exactly what a *dispatcher*-side lost wakeup would
    //! break, so it's worth pinning down directly: every post from any
    //! number of concurrent background threads — including ones landing
    //! while the loop is mid-poll, about to park, or right as another post
    //! is being drained — must eventually run.
    use super::*;
    use gpui::{PlatformDispatcher, Priority};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::platform::pump_events::EventLoopExtPumpEvents;
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    use winit::platform::x11::EventLoopBuilderExtX11;
    use winit::window::WindowId;

    /// One `RunnableVariant` that increments `counter` when run, then
    /// completes (never reschedules itself, so `schedule` is never called).
    fn counting_runnable(counter: Arc<AtomicUsize>) -> RunnableVariant {
        let (runnable, task) = async_task::Builder::new()
            .metadata(gpui::RunnableMeta::new_with_callers_location())
            .spawn(
                move |_meta| async move {
                    counter.fetch_add(1, AtomicOrdering::SeqCst);
                },
                |_runnable: RunnableVariant| {
                    unreachable!("a task with no .await points never reschedules");
                },
            );
        task.detach();
        runnable
    }

    /// Mirrors `WinitAppHandler`'s drain wiring (`app_handler.rs`) without
    /// depending on a real `WinitPlatform`/window: drain on every callback
    /// that could have been woken by `dispatch_on_main_thread`.
    struct DrainingHandler<'a> {
        dispatcher: &'a WinitDispatcher,
    }

    impl<'a> ApplicationHandler<WinitUserEvent> for DrainingHandler<'a> {
        fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
            self.dispatcher.drain_main_queue();
        }

        fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: WinitUserEvent) {
            self.dispatcher.drain_main_queue();
        }

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            _event: WindowEvent,
        ) {
        }

        fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
            self.dispatcher.drain_main_queue();
        }
    }

    #[test]
    fn concurrent_main_thread_posts_all_get_processed() {
        let mut builder = EventLoop::<WinitUserEvent>::with_user_event();
        // See the module-gating comment above `mod tests`: `#[test]` bodies
        // never run on the true process main thread. Both extension traits
        // are in scope (we don't know at compile time whether the test
        // machine's winit will pick Wayland or X11), so disambiguate with
        // fully qualified syntax rather than the ambiguous `builder.with_any_thread(true)`.
        EventLoopBuilderExtWayland::with_any_thread(&mut builder, true);
        EventLoopBuilderExtX11::with_any_thread(&mut builder, true);
        let mut event_loop = builder
            .build()
            .expect("failed to build a winit event loop for this test");
        let proxy = event_loop.create_proxy();
        let dispatcher = Arc::new(WinitDispatcher::new(proxy));
        let counter = Arc::new(AtomicUsize::new(0));

        const POSTS: usize = 64;
        let producers: Vec<_> = (0..POSTS)
            .map(|i| {
                let dispatcher = Arc::clone(&dispatcher);
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    // Stagger slightly so posts land throughout the pump
                    // loop's lifetime — including right as it's between
                    // iterations (parking/about-to-wait) — the exact
                    // "racing a configure" hazard: a post that arrives at
                    // that boundary must still wake the next iteration
                    // rather than being silently coalesced away.
                    thread::sleep(Duration::from_micros((i % 11) as u64 * 150));
                    dispatcher.dispatch_on_main_thread(counting_runnable(counter), Priority::High);
                })
            })
            .collect();

        let mut handler = DrainingHandler {
            dispatcher: &dispatcher,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while counter.load(AtomicOrdering::SeqCst) < POSTS && Instant::now() < deadline {
            event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut handler);
        }

        for producer in producers {
            producer.join().expect("producer thread panicked");
        }
        // One more pump/drain in case the last post(s) landed after the
        // loop's final iteration above but before the threads joined.
        event_loop.pump_app_events(Some(Duration::from_millis(50)), &mut handler);
        dispatcher.drain_main_queue();

        assert_eq!(
            counter.load(AtomicOrdering::SeqCst),
            POSTS,
            "a dispatch_on_main_thread post was dropped instead of waking the loop"
        );
    }
}
