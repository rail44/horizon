//! The question: can a guest be driven by one future that goes on for as
//! long as the guest does, polled from the UI thread, with the host's
//! calls into the guest made through it?
//!
//! `host-tasks` drove a guest two ways at once: a plain call was a future
//! polled to its end, and the guest's own tasks were given "turns" by a
//! second future that came back on its second poll. Both were polled with
//! one waker, so a wake meant either "poll this call on" or "a task wants a
//! turn", and nothing told the two apart.
//!
//! Here there is one future ([`Driver`]): the store's `run_concurrent`,
//! around a loop that takes the host's calls from a queue and makes them
//! through the accessor. The guest's tasks go on whenever that future is
//! polled. A wake means one thing, "poll the future", and a future with
//! nothing to do wakes nobody.
//!
//! This stands for the UI thread: no async runtime, a loop of frames that
//! polls when woken, and calls whose answers are wanted there and then.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::Thread;
use std::time::{Duration, Instant};

use wasmtime::component::{Accessor, Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, UpdateDeadline};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../guest-async/wit",
    world: "guest",
    imports: { default: trappable },
    // Every call is made through the accessor, beside the guest's tasks.
    exports: { default: async | store },
});

/// What the host counts of what the guest does.
#[derive(Default)]
pub struct Seen {
    /// Plain calls out of the guest.
    pub calls: AtomicU64,
    /// What the guest noted, in order.
    pub notes: Mutex<Vec<String>>,
}

pub struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    seen: Arc<Seen>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A timer with no runtime behind it: a thread sleeps and wakes whoever
/// waits.
struct Timer(Arc<Mutex<(bool, Option<Waker>)>>);

impl Timer {
    fn after(time: Duration) -> Self {
        let shared = Arc::new(Mutex::new((false, None::<Waker>)));
        let told = shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(time);
            let mut told = told.lock().unwrap();
            told.0 = true;
            if let Some(waker) = told.1.take() {
                waker.wake();
            }
        });
        Self(shared)
    }
}

impl Future for Timer {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        let mut shared = self.0.lock().unwrap();
        match shared.0 {
            true => Poll::Ready(()),
            false => {
                shared.1 = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl GuestImports for State {
    fn nothing(&mut self, value: u32) -> wasmtime::Result<()> {
        std::hint::black_box(value);
        self.seen.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn named(&mut self, node: u32, name: String, value: String) -> wasmtime::Result<()> {
        std::hint::black_box((node, name, value));
        self.seen.calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn several(&mut self, count: u32) -> wasmtime::Result<Vec<u64>> {
        self.seen.calls.fetch_add(1, Ordering::Relaxed);
        Ok((0..count.into()).collect())
    }

    fn note(&mut self, text: String) -> wasmtime::Result<()> {
        self.seen.notes.lock().unwrap().push(text);
        Ok(())
    }
}

impl<T: Send + 'static> GuestImportsWithStore<T> for HasSelf<State> {
    async fn wait(_accessor: &Accessor<T, Self>, milliseconds: u32) -> wasmtime::Result<()> {
        Timer::after(Duration::from_millis(milliseconds.into())).await;
        Ok(())
    }
}

/// The one waker: something wants the future polled. It notes that, and
/// unparks the UI thread in case it is waiting for a call's answer.
pub struct Woken {
    asked: AtomicBool,
    /// How many times it was woken, ever.
    pub wakes: AtomicU64,
    thread: Thread,
}

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.asked.store(true, Ordering::Release);
        self.wakes.fetch_add(1, Ordering::Relaxed);
        self.thread.unpark();
    }
}

/// A call into the guest, made through the accessor.
pub type Calling<'a> = Pin<Box<dyn Future<Output = wasmtime::Result<()>> + 'a>>;
type Job = Box<dyn for<'a> FnOnce(&'a Guest, &'a Accessor<State>) -> Calling<'a>>;

/// The calls asked for and not yet taken, and who waits for the next.
#[derive(Default)]
struct Asked {
    jobs: VecDeque<Job>,
    waiting: Option<Waker>,
}

/// Comes back with the next call asked for.
struct Next(Rc<RefCell<Asked>>);

impl Future for Next {
    type Output = Job;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Job> {
        let mut asked = self.0.borrow_mut();
        match asked.jobs.pop_front() {
            Some(job) => Poll::Ready(job),
            None => {
                asked.waiting = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// Moves the engine's epoch on when a poll of the future outlasts the
/// limit, so that wasm that does not come back is ended.
struct Watchdog {
    polls: mpsc::Sender<bool>,
}

impl Watchdog {
    fn over(engine: Engine, limit: Duration) -> Self {
        let (polls, told) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            let mut polling = false;
            loop {
                let wait = if polling { limit } else { Duration::from_secs(3600) };
                match told.recv_timeout(wait) {
                    Ok(now) => polling = now,
                    Err(RecvTimeoutError::Timeout) if polling => engine.increment_epoch(),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Self { polls }
    }
}

type Driving = Pin<Box<dyn Future<Output = wasmtime::Result<()>>>>;

/// A guest, driven from this thread.
pub struct Driver {
    /// The guest's whole life: gone once it has ended.
    driving: Option<Driving>,
    asked: Rc<RefCell<Asked>>,
    pub woken: Arc<Woken>,
    watchdog: Watchdog,
    /// When the poll under way began, for the store's epoch callback.
    began: Arc<Mutex<Option<Instant>>>,
    pub seen: Arc<Seen>,
    /// What the guest said if it trapped.
    pub failed: Option<String>,
    /// How many times the future was polled, and how many times a call
    /// had to wait for a wake before its answer came.
    pub polls: u64,
    pub parks: u64,
}

impl Driver {
    /// The guest at `path`, which may run for `limit` at a time and read
    /// what is under `lent`.
    pub fn new(
        path: &std::path::Path,
        limit: Duration,
        lent: Option<&std::path::Path>,
    ) -> wasmtime::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let component = Component::from_file(&engine, path)?;
        let mut linker = Linker::<State>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        Guest::add_to_linker::<_, HasSelf<State>>(&mut linker, |state| state)?;
        let seen = Arc::new(Seen::default());
        let mut wasi = WasiCtxBuilder::new();
        if let Some(lent) = lent {
            wasi.preopened_dir(lent, "/lent", FsPerms::ReadOnly)?;
        }
        let mut store = Store::new(
            &engine,
            State {
                wasi: wasi.build(),
                table: ResourceTable::new(),
                seen: seen.clone(),
            },
        );
        let began = Arc::new(Mutex::new(None::<Instant>));
        let polled = began.clone();
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            match *polled.lock().unwrap() {
                Some(began) if began.elapsed() >= limit => {
                    Err(wasmtime::Error::msg("the guest took too long"))
                }
                _ => Ok(UpdateDeadline::Continue(1)),
            }
        });
        let asked = Rc::new(RefCell::new(Asked::default()));
        let taken = asked.clone();
        // The store is the future's own: it is borrowed for as long as the
        // guest lives, by the one thing that drives it.
        let driving: Driving = Box::pin(async move {
            let mut store = store;
            let guest = Guest::instantiate_async(&mut store, &component, &linker).await?;
            store
                .run_concurrent(async |accessor| -> wasmtime::Result<()> {
                    loop {
                        let job = Next(taken.clone()).await;
                        job(&guest, accessor).await?;
                    }
                })
                .await?
        });
        Ok(Self {
            driving: Some(driving),
            asked,
            woken: Arc::new(Woken {
                asked: AtomicBool::new(true),
                wakes: AtomicU64::new(0),
                thread: std::thread::current(),
            }),
            watchdog: Watchdog::over(engine, limit),
            began,
            seen,
            failed: None,
            polls: 0,
            parks: 0,
        })
    }

    /// Polls the future once.
    fn poll(&mut self) {
        let Some(driving) = self.driving.as_mut() else {
            return;
        };
        self.polls += 1;
        *self.began.lock().unwrap() = Some(Instant::now());
        let _ = self.watchdog.polls.send(true);
        let waker = Waker::from(self.woken.clone());
        let polled = driving.as_mut().poll(&mut Context::from_waker(&waker));
        let _ = self.watchdog.polls.send(false);
        *self.began.lock().unwrap() = None;
        if let Poll::Ready(ended) = polled {
            self.failed = Some(match ended {
                Ok(()) => "the guest ended".to_string(),
                Err(error) => format!("{error:#}"),
            });
            self.driving = None;
        }
    }

    /// The UI thread's turn: polls the future for as long as something
    /// asks for it. How many times it polled.
    pub fn turn(&mut self) -> u64 {
        let before = self.polls;
        while self.driving.is_some() && self.woken.asked.swap(false, Ordering::AcqRel) {
            self.poll();
        }
        self.polls - before
    }

    /// Whether something has asked for the future to be polled since the
    /// last turn.
    pub fn asked(&self) -> bool {
        self.woken.asked.load(Ordering::Acquire)
    }

    /// Calls into the guest and waits here for the answer; `None` if the
    /// guest has trapped.
    pub fn call<R: 'static>(
        &mut self,
        call: impl for<'a> FnOnce(
            &'a Guest,
            &'a Accessor<State>,
        ) -> Pin<Box<dyn Future<Output = wasmtime::Result<R>> + 'a>>
        + 'static,
    ) -> Option<R> {
        let answer = Rc::new(RefCell::new(None::<R>));
        let told = answer.clone();
        let job: Job = Box::new(move |guest, accessor| {
            Box::pin(async move {
                *told.borrow_mut() = Some(call(guest, accessor).await?);
                Ok(())
            })
        });
        let waiting = {
            let mut asked = self.asked.borrow_mut();
            asked.jobs.push_back(job);
            asked.waiting.take()
        };
        if let Some(waiting) = waiting {
            waiting.wake();
        }
        self.woken.asked.store(true, Ordering::Release);
        loop {
            self.driving.as_ref()?;
            if self.woken.asked.swap(false, Ordering::AcqRel) {
                self.poll();
            } else {
                // The answer waits on something outside this thread.
                self.parks += 1;
                std::thread::park_timeout(Duration::from_millis(100));
            }
            if let Some(answer) = answer.borrow_mut().take() {
                return Some(answer);
            }
        }
    }
}
