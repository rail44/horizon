//! `PlatformDispatcher` backed by winit's `EventLoopProxy`. Ported from the
//! spike (`spikes/gpui-winit/src/dispatcher.rs`) with no behavioral change —
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

use gpui::{
    PlatformDispatcher, Priority, PriorityQueueReceiver, PriorityQueueSender, RunnableVariant,
};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use winit::event_loop::EventLoopProxy;

use crate::app_handler::WinitUserEvent;

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
