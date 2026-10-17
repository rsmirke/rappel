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

/// The CPU time the calling thread has used so far (user and system), or
/// `None` where Linux's `/proc` cannot be read. Unlike a wall clock it does
/// not grow while the thread waits for a CPU, so a bound on it holds on a
/// loaded machine (#67, ADR 0049). Read from `/proc/thread-self/schedstat`,
/// whose first field is the thread's `sum_exec_runtime` in nanoseconds
/// (Linux `fs/proc/base.c`, `proc_pid_schedstat`). The kernel brings that
/// sum up to date at each scheduler tick and context switch, not on the
/// read, so a reading can be up to one tick old (1 ms at `CONFIG_HZ=1000`,
/// 4 ms at 250). Where that file is missing, it falls back to the utime and
/// stime of `/proc/thread-self/stat`, in `USER_HZ` clock ticks of 10 ms.
/// The same as `rappel-core`'s `test_support::thread_cpu_time`, which is
/// not exported.
pub fn thread_cpu_time() -> Option<Duration> {
    if let Ok(schedstat) = std::fs::read_to_string("/proc/thread-self/schedstat")
        && let Some(nanos) = schedstat
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
    {
        return Some(Duration::from_nanos(nanos));
    }
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    // The command name, field 2, is in parentheses and may hold spaces;
    // utime and stime are fields 14 and 15, the 12th and 13th after it.
    let after_name = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_name.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(Duration::from_millis((utime + stime) * 10))
}

/// The CPU time `f` used on this thread, and its result: the thread's CPU
/// time where `/proc` can be read ([`thread_cpu_time`]), else the wall
/// clock. For time bounds on work that has to run on the main-context
/// thread (GTK), where [`within`] cannot be used.
pub fn cpu_time_of<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let wall = Instant::now();
    let start = thread_cpu_time();
    let value = f();
    let used = start
        .zip(thread_cpu_time())
        .map_or_else(|| wall.elapsed(), |(start, end)| end.saturating_sub(start));
    (used, value)
}

/// How long [`within`] waits before it calls the work hung: ten times its
/// CPU-time bound, and at least 30 s. It is a wall clock, so it grows with
/// the load on the machine; a thread that gets a third of a CPU (three busy
/// threads per core) still finishes far within it. The same as
/// `rappel-core`'s `test_support::hang_limit`.
pub fn hang_limit(limit: Duration) -> Duration {
    limit.saturating_mul(10).max(Duration::from_secs(30))
}

/// A fixed piece of work that allocates, formats, sorts and looks up: the
/// yardstick of [`slowdown`]. Larger in a release build, so it stays well
/// above the CPU clock's precision. The same as `rappel-core`'s
/// `test_support::reference_work`, so [`REFERENCE_CPU`] is the same.
fn reference_work() {
    let n: u64 = if cfg!(debug_assertions) {
        20_000
    } else {
        100_000
    };
    let mut words: Vec<String> = (0..n)
        .map(|i| format!("{:016x}", i.wrapping_mul(0x9E37_79B9_7F4A_7C15)))
        .collect();
    words.sort_unstable();
    let index: std::collections::BTreeMap<&str, usize> = words
        .iter()
        .enumerate()
        .map(|(i, w)| (w.as_str(), i))
        .collect();
    let sum: usize = words.iter().map(|w| index[w.as_str()]).sum();
    std::hint::black_box(sum);
}

/// The CPU time of [`reference_work`] on the machine the time bounds were
/// set on (an i7-12700H laptop, unloaded, best of 30 runs), in a debug and
/// a release test build. The bounds are in CPU time on that machine.
const REFERENCE_CPU: Duration = if cfg!(debug_assertions) {
    Duration::from_millis(17)
} else {
    Duration::from_millis(21)
};

/// How many times slower this thread runs now than the machine the bounds
/// were set on, never less than 1: the least CPU time of three runs of
/// [`reference_work`], over [`REFERENCE_CPU`]. Load makes a thread slower
/// even in CPU time (a busy thread on the same core, a lower clock speed, a
/// slower core, shared caches), and this measures by how much (ADR 0049).
/// Never less than 1, so a faster machine is held to the bounds as they
/// are. The same as `rappel-core`'s `test_support::slowdown`.
pub fn slowdown() -> f64 {
    let best = (0..3)
        .map(|_| cpu_time_of(reference_work).0)
        .min()
        .unwrap_or(REFERENCE_CPU);
    let slowdown = (best.as_secs_f64() / REFERENCE_CPU.as_secs_f64()).max(1.0);
    if std::env::var_os("RAPPEL_TIMINGS").is_some() {
        eprintln!("slowdown: {slowdown:.2} (reference work: {best:?} CPU)");
    }
    slowdown
}

/// Whether `used`, a CPU time of this thread, is within `limit` on the
/// machine the bounds were set on: at once if `used` is within it, else
/// once divided by the [`slowdown`] measured now, on this thread. Only a
/// time over the bound pays for the measurement, and the measurement is
/// taken in the conditions the work just ran in.
pub fn cpu_within(used: Duration, limit: Duration) -> Result<(), String> {
    if used <= limit {
        return Ok(());
    }
    let slowdown = slowdown();
    let scaled = used.div_f64(slowdown);
    if scaled <= limit {
        Ok(())
    } else {
        Err(format!(
            "{used:?} of CPU time, {scaled:?} at this machine's slowdown of {slowdown:.2}, \
             more than {limit:?}"
        ))
    }
}

/// Fails the test unless `used`, a CPU time of this thread, is within
/// `limit` ([`cpu_within`]). Call it on the thread that did the work: for
/// GTK work, inside the [`with_gtk`] or [`with_widgets`] body.
#[track_caller]
pub fn assert_cpu_within(used: Duration, limit: Duration, what: &str) {
    if let Err(message) = cpu_within(used, limit) {
        panic!("{what}: {message}");
    }
}

/// The stack hostile input is handled on in tests: a sixteenth of the main
/// thread's usual 8 MiB, where the app draws cards, so a recursion that only
/// just passes here has a wide margin in the app. A stack overflow aborts
/// the process; it cannot be caught. The same as `rappel-core`'s
/// `test_support::SMALL_STACK`, which is not exported.
pub const SMALL_STACK: usize = 512 * 1024;

/// Runs `f` on a new thread with a [`SMALL_STACK`] and returns its result.
/// Two bounds, kept apart (ADR 0049):
/// - **Performance:** the test fails if `f` used more than `limit` of CPU
///   time ([`cpu_within`], on that thread). Waiting for a CPU on a loaded
///   machine does not count, and a thread slowed by the load is measured
///   against a reference, so the bound holds under load and still fails a
///   blow-up.
/// - **Hang:** the test fails if `f` has not returned after [`hang_limit`]
///   of wall-clock time, so a hang fails the test instead of blocking the
///   run (the thread is left behind; the test process ends anyway).
///
/// `RAPPEL_TIMINGS=1` prints the CPU and wall-clock times. The same as
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
    let wall = Instant::now();
    thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let (cpu, value) = cpu_time_of(f);
            let verdict = cpu_within(cpu, limit);
            // The receiver is gone only if the test already failed.
            let _ = sender.send((cpu, verdict, value));
        })
        .expect("tests can start a thread");
    let hang = hang_limit(limit);
    match answer.recv_timeout(hang) {
        Ok((cpu, verdict, value)) => {
            print_timing(&format!("{what} (wall)"), wall.elapsed());
            print_timing(&format!("{what} (CPU)"), cpu);
            if let Err(message) = verdict {
                panic!("{what}: {message}");
            }
            value
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what} hung: no result after {hang:?}"),
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
