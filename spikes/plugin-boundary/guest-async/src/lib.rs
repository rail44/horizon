//! The question: can a guest have a task of its own, run by the host a
//! little at a time, with plain calls into the guest in between?

wit_bindgen::generate!({
    path: "wit",
    world: "guest",
});

use std::cell::Cell;

thread_local! {
    static COUNTED: Cell<u32> = const { Cell::new(0) };
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
}

export!(Component);
