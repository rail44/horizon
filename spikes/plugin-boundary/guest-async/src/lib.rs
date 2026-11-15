//! The question: can a guest have a task of its own, run by the host a
//! little at a time, with plain calls into the guest in between?

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
});

use std::cell::{Cell, RefCell};
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::{Poll, Waker};

type Task = Pin<Box<dyn Future<Output = ()>>>;

thread_local! {
    static COUNTED: Cell<u32> = const { Cell::new(0) };
    /// Tasks asked for by a plain call and not started yet, and what wakes
    /// the task that starts them.
    static ASKED: RefCell<Vec<Task>> = const { RefCell::new(Vec::new()) };
    static STARTER: RefCell<Option<Waker>> = const { RefCell::new(None) };
    /// Whether the relay's second task has run, and who waits for it.
    static RELAYED: RefCell<(bool, Option<Waker>)> = const { RefCell::new((false, None)) };
}

struct Component;

impl Guest for Component {
    /// Comes back at once; the task it started goes on.
    async fn start(times: u32, milliseconds: u32) {
        wit_bindgen::spawn_local(async move {
            for turn in 0..times {
                wait(milliseconds).await;
                note(&format!("turn {turn}"));
                COUNTED.set(turn + 1);
            }
        });
    }

    fn burst(kind: u32, times: u32) {
        for turn in 0..times {
            match kind {
                0 => nothing(turn),
                1 => named(turn, "class", "row selected"),
                _ => {
                    std::hint::black_box(several(8));
                }
            }
        }
    }

    fn counted() -> u32 {
        COUNTED.get()
    }

    async fn tasks() {
        wit_bindgen::spawn_local(poll_fn(|context| {
            STARTER.with(|starter| *starter.borrow_mut() = Some(context.waker().clone()));
            for task in ASKED.with(|asked| std::mem::take(&mut *asked.borrow_mut())) {
                wit_bindgen::spawn_local(task);
            }
            Poll::<()>::Pending
        }));
    }

    fn later(milliseconds: u32) {
        let task = async move {
            wait(milliseconds).await;
            note("later");
            COUNTED.set(COUNTED.get() + 100);
        };
        ASKED.with(|asked| asked.borrow_mut().push(Box::pin(task)));
        if let Some(starter) = STARTER.with(|starter| starter.borrow().clone()) {
            starter.wake();
        }
    }

    async fn relay(milliseconds: u32) {
        wit_bindgen::spawn_local(async {
            poll_fn(|context| {
                RELAYED.with(|relayed| {
                    let mut relayed = relayed.borrow_mut();
                    if relayed.0 {
                        return Poll::Ready(());
                    }
                    relayed.1 = Some(context.waker().clone());
                    Poll::Pending
                })
            })
            .await;
            note("relayed");
            COUNTED.set(COUNTED.get() + 1000);
        });
        wit_bindgen::spawn_local(async move {
            wait(milliseconds).await;
            let waiting = RELAYED.with(|relayed| {
                let mut relayed = relayed.borrow_mut();
                relayed.0 = true;
                relayed.1.take()
            });
            if let Some(waiting) = waiting {
                waiting.wake();
            }
        });
    }

    fn size_of(path: String) -> Option<u64> {
        std::fs::read(path).ok().map(|bytes| bytes.len() as u64)
    }

    fn stuck() {
        let mut turns = 0u64;
        loop {
            turns = std::hint::black_box(turns.wrapping_add(1));
        }
    }
}

export!(Component);
