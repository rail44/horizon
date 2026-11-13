//! The question: can a guest's own task be run from the UI thread's loop,
//! a frame at a time, with plain calls into the guest in between?
//!
//! The guest (`guest-async`) starts a task that waits on this host's timer
//! and notes each turn. This program stands for the UI thread: it has no
//! async runtime, and a loop of "frames". In each frame it calls the guest
//! plainly, then gives the store's tasks a turn and no more.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use wasmtime::component::{Accessor, Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../guest-async/wit",
    world: "guest",
    // A store with async imports takes every call as an async one.
    exports: { default: async },
});

struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    started: Instant,
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
    fn nothing(&mut self, value: u32) {
        std::hint::black_box(value);
    }

    fn named(&mut self, node: u32, name: String, value: String) {
        std::hint::black_box((node, name, value));
    }

    fn several(&mut self, count: u32) -> Vec<u64> {
        (0..count.into()).collect()
    }

    fn note(&mut self, text: String) {
        println!("  {:>4} ms  the guest notes: {text}", self.started.elapsed().as_millis());
    }
}

impl<T: Send + 'static> GuestImportsWithStore<T> for HasSelf<State> {
    async fn wait(_accessor: &Accessor<T, Self>, milliseconds: u32) {
        Timer::after(Duration::from_millis(milliseconds.into())).await;
    }
}

/// What the UI thread's waker is: a note that something wants a turn.
struct Woken(AtomicBool);

impl Wake for Woken {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Polls `future` once.
fn turn<F: Future>(future: Pin<&mut F>, woken: &Arc<Woken>) -> Poll<F::Output> {
    let waker = Waker::from(woken.clone());
    future.poll(&mut Context::from_waker(&waker))
}

/// Runs `future` to its end on this thread, sleeping between polls.
fn finish<F: Future>(future: F, woken: &Arc<Woken>) -> F::Output {
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = turn(future.as_mut(), woken) {
            return output;
        }
        while !woken.0.swap(false, Ordering::Relaxed) {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// Comes back the second time it is polled: one turn for everything else.
struct OneTurn(bool);

impl Future for OneTurn {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        context.waker().wake_by_ref();
        Poll::Pending
    }
}

fn main() -> wasmtime::Result<()> {
    let path = std::env::args().nth(1).expect("the guest's path");
    let engine = Engine::new(&Config::new())?;
    let component = Component::from_file(&engine, &path)?;
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    Guest::add_to_linker::<_, HasSelf<State>>(&mut linker, |state| state)?;
    let mut store = Store::new(
        &engine,
        State {
            wasi: WasiCtxBuilder::new().inherit_stdout().inherit_stderr().build(),
            table: ResourceTable::new(),
            started: Instant::now(),
        },
    );
    let woken = Arc::new(Woken(AtomicBool::new(false)));
    let guest = finish(Guest::instantiate_async(&mut store, &component, &linker), &woken)?;

    // The guest's task is started: the call comes back, the task goes on.
    finish(
        store.run_concurrent(async |accessor| guest.call_start(accessor, 12, 10).await),
        &woken,
    )??;
    println!("start came back at {} ms", store.data().started.elapsed().as_millis());

    // Frames. Each: a plain call into the guest, then one turn for the
    // store's tasks if something asked for one.
    let mut frames = 0;
    let mut turns = 0;
    let mut longest = Duration::ZERO;
    loop {
        frames += 1;
        std::thread::sleep(Duration::from_millis(4));
        // (A wake that arrives during this call is not to be dropped: the
        // note of it is left as it is, and the turn below is given.)
        if frames % 8 == 0 {
            let counted = finish(guest.call_counted(&mut store), &woken)?;
            println!("frame {frames}: a plain call says the task has counted {counted}");
            if counted >= 12 {
                break;
            }
        }
        if woken.0.swap(false, Ordering::Relaxed) {
            turns += 1;
            let took = Instant::now();
            finish(store.run_concurrent(async |_| OneTurn(false).await), &woken)?;
            longest = longest.max(took.elapsed());
        }
        assert!(frames < 2000, "the task did not finish");
    }
    println!(
        "done at {} ms: {frames} frames, {turns} of them gave the tasks a turn, the longest turn {longest:?}",
        store.data().started.elapsed().as_millis()
    );

    // With the task over, nothing asks for a turn.
    let mut idle = 0;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(4));
        idle += usize::from(woken.0.swap(false, Ordering::Relaxed));
    }
    println!("50 frames after the task ended: {idle} asked for a turn");

    // A task that only waits, for a long time: how often is a turn asked
    // for while it does?
    finish(
        store.run_concurrent(async |accessor| guest.call_start(accessor, 1, 1000).await),
        &woken,
    )??;
    let (mut asked, mut waited) = (0, 0);
    while finish(guest.call_counted(&mut store), &woken)? != 1 {
        // (That call's own wakes are not the waiting task's.)
        woken.0.store(false, Ordering::Relaxed);
        for _ in 0..25 {
            std::thread::sleep(Duration::from_millis(4));
            waited += 1;
            if woken.0.swap(false, Ordering::Relaxed) {
                asked += 1;
                finish(store.run_concurrent(async |_| OneTurn(false).await), &woken)?;
            }
        }
        assert!(waited < 2000, "the waiting task did not finish");
    }
    println!("a task waiting one second: {waited} frames, {asked} of them asked for a turn");

    // What a plain call out of the guest costs through this store.
    let calls = 1_000_000;
    for (kind, what) in [(0, "a number"), (1, "two short strings"), (2, "a list of 8 back")] {
        let took = Instant::now();
        finish(guest.call_burst(&mut store, kind, calls), &woken)?;
        println!("a call to the host with {what}: {:?} each", took.elapsed() / calls);
    }

    // What a plain call into the guest costs through this store.
    let calls = 100_000;
    let took = Instant::now();
    for _ in 0..calls {
        finish(guest.call_counted(&mut store), &woken)?;
    }
    println!("{calls} plain calls: {:?} each", took.elapsed() / calls);
    Ok(())
}
