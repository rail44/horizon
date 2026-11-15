//! What the one future has to hold to, each as a test. The guest is
//! `guest-async`, built for wasm32-wasip2 first (`check-driver.sh`).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use host_driver::Driver;

fn guest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/wasm32-wasip2/debug/guest_async.wasm")
}

fn driver() -> Driver {
    Driver::new(&guest(), Duration::from_secs(5), None).expect("the guest")
}

fn counted(driver: &mut Driver) -> u32 {
    driver
        .call(|guest, accessor| Box::pin(guest.call_counted(accessor)))
        .expect("the count")
}

/// The UI thread's frames, until `met`: each one polls if something asked
/// for it. How many frames polled.
fn frames(driver: &mut Driver, what: &str, mut met: impl FnMut(&mut Driver) -> bool) -> u32 {
    let (started, mut polled) = (Instant::now(), 0);
    while !met(driver) {
        assert!(started.elapsed() < Duration::from_secs(20), "no {what}");
        std::thread::sleep(Duration::from_millis(2));
        if driver.asked() {
            polled += 1;
            driver.turn();
        }
    }
    polled
}

/// Frames in which nothing happens: none of them may ask for a poll.
fn still(driver: &mut Driver, what: &str) {
    // (What the last call left is taken first.)
    let settling = driver.turn();
    assert!(settling < 10, "{what}: {settling} polls to settle");
    let wakes = driver.woken.wakes.load(Ordering::Relaxed);
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(2));
        assert!(!driver.asked(), "{what}: asked for a poll with nothing to do");
    }
    assert_eq!(driver.woken.wakes.load(Ordering::Relaxed), wakes, "{what}");
}

#[test]
fn a_guest_s_task_goes_on_when_the_future_is_polled_and_then_nothing_asks() {
    let mut driver = driver();
    driver
        .call(|guest, accessor| Box::pin(guest.call_start(accessor, 5, 10)))
        .expect("started");
    // The call came back; the task goes on, a wake of the host's timer at
    // a time.
    assert_eq!(counted(&mut driver), 0);
    let polled = frames(&mut driver, "task", |driver| driver.seen.notes.lock().unwrap().len() == 5);
    assert_eq!(counted(&mut driver), 5);
    // (Each of the five waits wakes it once; a wake is not many polls.)
    assert!(polled <= 12, "{polled} frames polled for five waits");
    still(&mut driver, "after the task");
}

#[test]
fn a_task_that_only_waits_asks_for_nothing_while_it_does() {
    let mut driver = driver();
    driver
        .call(|guest, accessor| Box::pin(guest.call_start(accessor, 1, 400)))
        .expect("started");
    driver.turn();
    let before = driver.polls;
    let polled = frames(&mut driver, "task", |driver| driver.seen.notes.lock().unwrap().len() == 1);
    assert!(polled <= 2, "{polled} frames polled while one wait went on");
    assert!(driver.polls - before <= 6, "{} polls", driver.polls - before);
}

#[test]
fn a_call_is_answered_there_and_then_while_a_task_waits() {
    let mut driver = driver();
    driver
        .call(|guest, accessor| Box::pin(guest.call_start(accessor, 1, 300)))
        .expect("started");
    let parks = driver.parks;
    let took = Instant::now();
    for _ in 0..100 {
        assert_eq!(counted(&mut driver), 0);
    }
    // No call waited for anything outside this thread.
    assert_eq!(driver.parks, parks);
    assert!(took.elapsed() < Duration::from_millis(200), "{:?}", took.elapsed());
    still(&mut driver, "between calls, a task waiting");
}

#[test]
fn a_call_makes_its_plain_calls_out_to_the_host() {
    let mut driver = driver();
    for kind in 0..3 {
        driver
            .call(move |guest, accessor| Box::pin(guest.call_burst(accessor, kind, 1000)))
            .expect("the burst");
    }
    assert_eq!(driver.seen.calls.load(Ordering::Relaxed), 3000);
    still(&mut driver, "after plain calls");
}

#[test]
fn a_task_a_plain_call_asks_for_is_started_and_goes_on() {
    let mut driver = driver();
    driver
        .call(|guest, accessor| Box::pin(guest.call_tasks(accessor)))
        .expect("the starter");
    driver
        .call(|guest, accessor| Box::pin(guest.call_later(accessor, 20)))
        .expect("asked for");
    frames(&mut driver, "task", |driver| {
        driver.seen.notes.lock().unwrap().contains(&"later".to_string())
    });
    assert_eq!(counted(&mut driver), 100);
    still(&mut driver, "after the task a call asked for");
}

#[test]
fn a_task_woken_by_another_task_goes_on() {
    let mut driver = driver();
    driver
        .call(|guest, accessor| Box::pin(guest.call_relay(accessor, 20)))
        .expect("started");
    frames(&mut driver, "relay", |driver| {
        driver.seen.notes.lock().unwrap().contains(&"relayed".to_string())
    });
    assert_eq!(counted(&mut driver), 1000);
    still(&mut driver, "after the relay");
}

#[test]
fn a_call_that_does_not_come_back_is_ended_and_the_guest_let_go_of() {
    let mut driver = Driver::new(&guest(), Duration::from_millis(300), None).expect("the guest");
    assert_eq!(counted(&mut driver), 0);
    let took = Instant::now();
    let answer = driver.call(|guest, accessor| Box::pin(guest.call_stuck(accessor)));
    assert!(answer.is_none());
    assert!(took.elapsed() < Duration::from_secs(3), "{:?}", took.elapsed());
    let said = driver.failed.clone().expect("what it said");
    assert!(said.contains("took too long"), "{said}");
    // What is asked of it afterwards is answered with nothing, at once.
    assert!(driver
        .call(|guest, accessor| Box::pin(guest.call_counted(accessor)))
        .is_none());
}

#[test]
fn a_call_that_reads_a_file_is_answered_and_a_task_waiting_goes_on_after() {
    let lent = std::env::temp_dir().join(format!("host-driver-{}", std::process::id()));
    std::fs::create_dir_all(&lent).unwrap();
    std::fs::write(lent.join("file"), vec![7u8; 300_000]).unwrap();
    let mut driver = Driver::new(&guest(), Duration::from_secs(5), Some(&lent)).expect("the guest");
    driver
        .call(|guest, accessor| Box::pin(guest.call_start(accessor, 2, 10)))
        .expect("started");
    // WASI's own functions hold the store while they wait: the call's
    // answer may wait for a wake from outside this thread, and comes.
    let read = driver
        .call(|guest, accessor| Box::pin(guest.call_size_of(accessor, "/lent/file".to_string())))
        .expect("the call");
    assert_eq!(read, Some(300_000));
    let missing = driver
        .call(|guest, accessor| Box::pin(guest.call_size_of(accessor, "/lent/none".to_string())))
        .expect("the call");
    assert_eq!(missing, None);
    println!("reading a file: {} calls waited for a wake", driver.parks);
    frames(&mut driver, "task", |driver| driver.seen.notes.lock().unwrap().len() == 2);
    still(&mut driver, "after reading");
    let _ = std::fs::remove_dir_all(&lent);
}

/// What a call costs through the one future. Printed, not held to a
/// number: `cargo test -- --nocapture`.
#[test]
fn what_a_call_costs() {
    let mut driver = driver();
    let calls = 20_000;
    let (polls, took) = (driver.polls, Instant::now());
    for _ in 0..calls {
        counted(&mut driver);
    }
    println!(
        "{calls} calls into the guest: {:?} each, {:.2} polls each",
        took.elapsed() / calls,
        (driver.polls - polls) as f64 / f64::from(calls)
    );
    let calls = 200_000;
    for (kind, what) in [(0, "a number"), (1, "two short strings"), (2, "a list of 8 back")] {
        let took = Instant::now();
        driver
            .call(move |guest, accessor| Box::pin(guest.call_burst(accessor, kind, calls)))
            .expect("the burst");
        println!("a call to the host with {what}: {:?} each", took.elapsed() / calls);
    }
}
