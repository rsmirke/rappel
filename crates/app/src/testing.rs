//! One thread for the tests that need the default main context or GTK.
//!
//! Only the thread that owns the default main context may iterate it, and
//! `gtk::init` takes it for good: gtk4-rs 0.11 (`rt.rs`, `init`) calls
//! `g_main_context_acquire` and never releases it, and refuses to start GTK
//! again from another thread. The test harness runs each test on a thread
//! of its own, so two such tests on their own threads fail or crash. They
//! send their body here instead, and the bodies run one at a time.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use gtk::glib;

type Job = Box<dyn FnOnce() + Send>;

thread_local! {
    /// Whether this thread is the one that owns the default main context.
    static ON_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// The queue of the thread that owns the default main context, started on
/// first use.
fn queue() -> &'static mpsc::Sender<Job> {
    static QUEUE: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (sender, jobs) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("main-context".to_owned())
            .spawn(move || {
                ON_WORKER.set(true);
                let context = glib::MainContext::default();
                let _owner = context
                    .acquire()
                    .expect("every test that uses the default main context goes through here");
                for job in jobs {
                    job();
                }
            })
            .expect("tests can start a thread");
        sender
    })
}

/// Runs `f` on the thread that owns the default main context and returns
/// its result. A panic in `f` fails the calling test (a panic inside a C
/// callback that `f` runs aborts the test binary instead: it cannot unwind
/// through C). Called from that thread, it runs `f` directly: queueing it
/// would wait for itself.
pub fn on_main_context<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    if ON_WORKER.get() {
        return f();
    }
    let (sender, answer) = mpsc::channel();
    let job: Job = Box::new(move || {
        // The caller may have stopped waiting; nothing to do then.
        let _ = sender.send(panic::catch_unwind(AssertUnwindSafe(f)));
    });
    queue()
        .send(job)
        .expect("the main-context thread runs for the whole test run");
    match answer
        .recv()
        .expect("the main-context thread answers every job")
    {
        Ok(value) => value,
        Err(payload) => panic::resume_unwind(payload),
    }
}

/// Like [`on_main_context`], with GTK started first. Fails without a display:
/// run the tests in a desktop session or under `xvfb-run -a` (ADR 0024).
pub fn with_gtk<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    on_main_context(|| {
        gtk::init().expect("GTK needs a display for this test; run it under `xvfb-run -a`");
        f()
    })
}

/// Like [`with_gtk`], with libadwaita started and the app's resources (the
/// templates of its widgets) registered, once per test run, for tests that
/// build its widgets.
pub fn with_widgets<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    static RESOURCES: std::sync::Once = std::sync::Once::new();
    with_gtk(|| {
        adw::init().expect("libadwaita starts once GTK has");
        RESOURCES.call_once(|| {
            gtk::gio::resources_register_include!("rappel.gresource")
                .expect("the resources are built into the test binary");
        });
        f()
    })
}

/// A new database in a temporary directory, for tests that need the
/// store; the directory goes when the returned guard drops.
pub fn temporary_state() -> (tempfile::TempDir, crate::state::AppState) {
    let tmp = tempfile::tempdir().expect("a temporary directory");
    let state =
        crate::state::AppState::open(tmp.path().join("data")).expect("a new database opens");
    (tmp, state)
}

/// Runs the default main context until `done` holds or `timeout` has
/// passed, and says whether `done` held. For waiting on GTK (a widget being
/// mapped), not for timing: the timeout only stops a test that would hang.
pub fn settle_until(done: impl Fn() -> bool, timeout: Duration) -> bool {
    let context = glib::MainContext::default();
    let end = Instant::now() + timeout;
    loop {
        while context.iteration(false) {}
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// The CPU time the calling thread has used so far (user and system), from
/// `/proc/thread-self/stat`, or `None` where that cannot be read. Unlike a
/// wall clock it does not grow while the thread waits for a CPU, so a bound
/// on it holds on a loaded machine (#67). Counted in clock ticks, which
/// Linux reports at `USER_HZ` = 100 a second, so it is precise to 10 ms.
pub fn thread_cpu_time() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    // The command name, field 2, is in parentheses and may hold spaces;
    // utime and stime are fields 14 and 15, the 12th and 13th after it.
    let after_name = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_name.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(Duration::from_millis((utime + stime) * 10))
}

/// The stack hostile input is handled on in tests: a sixteenth of the main
/// thread's usual 8 MiB, where the app draws cards, so a recursion that only
/// just passes here has a wide margin in the app. A stack overflow aborts
/// the process; it cannot be caught. The same as `rappel-core`'s
/// `test_support::SMALL_STACK`, which is not exported.
pub const SMALL_STACK: usize = 512 * 1024;

/// Runs `f` on a new thread with a [`SMALL_STACK`] and returns its result,
/// failing the test if it takes longer than `limit`. A hang fails the test
/// instead of blocking the run (the thread is left behind; the test process
/// ends anyway). `RAPPEL_TIMINGS=1` prints how long it took. The same as
/// `rappel-core`'s `test_support::within`, which is not exported.
pub fn within<T: Send + 'static>(
    limit: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    within_stack(SMALL_STACK, limit, what, f)
}

/// Like [`within`], on a stack of `stack` bytes.
pub fn within_stack<T: Send + 'static>(
    stack: usize,
    limit: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, answer) = mpsc::channel();
    let start = Instant::now();
    thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            // The receiver is gone only if the test already failed.
            let _ = sender.send(f());
        })
        .expect("tests can start a thread");
    match answer.recv_timeout(limit) {
        Ok(value) => {
            print_timing(what, start.elapsed());
            value
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what} took longer than {limit:?}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
    }
}

/// Prints `elapsed` for `what` when `RAPPEL_TIMINGS` is set, for tests that
/// time work that must stay on the main-context thread.
pub fn print_timing(what: &str, elapsed: Duration) {
    if std::env::var_os("RAPPEL_TIMINGS").is_some() {
        eprintln!("{what}: {elapsed:?}");
    }
}
