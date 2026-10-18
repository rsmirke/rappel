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

/// A real PNG of `w` × `h` grey pixels, encoded by GDK (needs GTK).
pub fn png_bytes(w: i32, h: i32) -> Vec<u8> {
    use gtk::gdk;
    use gtk::prelude::*;
    let pixels = vec![0x80_u8; usize::try_from(w * h * 4).expect("a small image")];
    let texture = gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(pixels),
        usize::try_from(w * 4).expect("a small image"),
    );
    texture.save_to_png_bytes().to_vec()
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

/// The calling thread's CPU time (user and system) from
/// `/proc/thread-self/schedstat`, whose first field is its `sum_exec_runtime`
/// in nanoseconds (Linux `fs/proc/base.c`, `proc_pid_schedstat`), or `None`
/// where that cannot be read. Unlike a wall clock it does not grow while
/// the thread waits for a CPU, so a bound on it holds on a loaded machine
/// (#67, ADR 0049). The kernel brings that sum up to date at each scheduler
/// tick and context switch, not on the read, so a reading can be up to one
/// tick old (1 ms at `CONFIG_HZ=1000`, 4 ms at 250): time work of a few
/// milliseconds in batches.
fn schedstat_time() -> Option<Duration> {
    let schedstat = std::fs::read_to_string("/proc/thread-self/schedstat").ok()?;
    let nanos = schedstat.split_whitespace().next()?.parse().ok()?;
    Some(Duration::from_nanos(nanos))
}

/// The calling thread's CPU time from the utime and stime of
/// `/proc/thread-self/stat`, in `USER_HZ` clock ticks of 10 ms, or `None`
/// where that cannot be read. Coarser than [`schedstat_time`], and sampled
/// at scheduler ticks.
fn stat_time() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    // The command name, field 2, is in parentheses and may hold spaces;
    // utime and stime are fields 14 and 15, the 12th and 13th after it.
    let after_name = &stat[stat.rfind(')')? + 1..];
    let mut fields = after_name.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(Duration::from_millis((utime + stime) * 10))
}

/// Both readings of the thread's CPU time, taken at one moment.
#[derive(Clone, Copy, Debug)]
pub struct CpuReading {
    pub schedstat: Option<Duration>,
    pub stat: Option<Duration>,
}

/// Reads [`CpuReading`] for the calling thread.
pub fn thread_cpu_time() -> CpuReading {
    CpuReading {
        schedstat: schedstat_time(),
        stat: stat_time(),
    }
}

/// Wall-clock time after which a [`schedstat_time`] that has not moved is
/// taken as dead (an old kernel prints `0 0 0`). Work running that long
/// either saw a scheduler tick (at any `CONFIG_HZ` of 100 or more) or was
/// switched out, and both bring the sum up to date.
const SCHEDSTAT_DEAD_AFTER: Duration = Duration::from_millis(50);

/// The same for [`stat_time`], which only counts the ticks the thread was
/// running at, so it may miss a short run on a loaded machine.
const STAT_DEAD_AFTER: Duration = Duration::from_secs(1);

/// The CPU time between two readings, from the first clock that works: the
/// precise [`schedstat_time`], else [`stat_time`], else the wall clock. A
/// clock that did not move while `wall` passed its dead-after time is taken
/// as not working, so a dead clock makes the bounds use the wall clock
/// (stricter under load) instead of passing everything silently.
pub fn cpu_time_between(start: CpuReading, end: CpuReading, wall: Duration) -> Duration {
    let moved = |start: Option<Duration>, end: Option<Duration>, dead_after: Duration| {
        let (start, end) = (start?, end?);
        (end > start || wall < dead_after).then(|| end.saturating_sub(start))
    };
    moved(start.schedstat, end.schedstat, SCHEDSTAT_DEAD_AFTER)
        .or_else(|| moved(start.stat, end.stat, STAT_DEAD_AFTER))
        .unwrap_or(wall)
}

/// The CPU time `f` used on this thread, and its result
/// ([`cpu_time_between`]: the thread's CPU time, else the wall clock).
/// For time bounds on work that has to run on the main-context thread
/// (GTK), where [`within`] cannot be used. The same as `rappel-core`'s
/// `test_support::cpu_time_of`, which is not exported.
pub fn cpu_time_of<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let wall = Instant::now();
    let start = thread_cpu_time();
    let value = f();
    let used = cpu_time_between(start, thread_cpu_time(), wall.elapsed());
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
/// once divided by the [`slowdown`] measured now, on this thread ([`judge`]).
/// Only a time over the bound pays for the measurement, and the measurement
/// is taken in the conditions the work just ran in.
pub fn cpu_within(used: Duration, limit: Duration) -> Result<(), String> {
    judge(used, limit, slowdown)
}

/// The verdict of [`cpu_within`], with the slowdown given: `used` within
/// `limit` is Ok without calling `slowdown`; otherwise `used` divided by
/// `slowdown()`, never by less than 1, must be within `limit`.
fn judge(used: Duration, limit: Duration, slowdown: impl FnOnce() -> f64) -> Result<(), String> {
    if used <= limit {
        return Ok(());
    }
    let slowdown = slowdown().max(1.0);
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

/// The reference work takes a few milliseconds of measured CPU time, so a
/// CPU clock that reads zero or does not move fails here. The same check as
/// `rappel-core`'s `test_support::reference_work_is_long_enough_to_time`.
#[test]
fn reference_work_is_long_enough_to_time() {
    let least = (0..5)
        .map(|_| cpu_time_of(reference_work).0)
        .min()
        .unwrap_or_default();
    assert!(least >= Duration::from_millis(5), "{least:?}");
    assert!(slowdown() >= 1.0);
}

#[test]
fn a_time_within_its_limit_passes_without_measuring_the_slowdown() {
    let limit = Duration::from_millis(100);
    let never = || -> f64 { panic!("the slowdown is measured only over the limit") };
    assert_eq!(judge(Duration::from_millis(100), limit, never), Ok(()));
    assert_eq!(judge(Duration::ZERO, limit, never), Ok(()));
}

#[test]
fn a_time_over_its_limit_is_divided_by_the_slowdown() {
    let limit = Duration::from_millis(100);
    let measured = std::cell::Cell::new(0);
    let slowdown = |s: f64| {
        let measured = &measured;
        move || {
            measured.set(measured.get() + 1);
            s
        }
    };
    // 300 ms at three times slower is 100 ms: within.
    assert_eq!(
        judge(Duration::from_millis(300), limit, slowdown(3.0)),
        Ok(())
    );
    // Still over once divided.
    assert!(judge(Duration::from_millis(301), limit, slowdown(3.0)).is_err());
    assert!(judge(Duration::from_millis(400), limit, slowdown(2.0)).is_err());
    assert_eq!(measured.get(), 3);
    // Never divided by less than 1: a fast machine is held to the limit.
    assert!(judge(Duration::from_millis(101), limit, || 0.5).is_err());
    assert_eq!(
        judge(Duration::from_millis(150), limit, || 0.5).map_err(|m| m.contains("1.00")),
        Err(true)
    );
}

#[test]
fn a_cpu_clock_that_does_not_move_is_not_trusted() {
    let ms = Duration::from_millis;
    let at = |schedstat: Option<u64>, stat: Option<u64>| CpuReading {
        schedstat: schedstat.map(ms),
        stat: stat.map(ms),
    };
    // schedstat moved: it is used, whatever the wall clock says.
    assert_eq!(
        cpu_time_between(at(Some(5), Some(0)), at(Some(25), Some(10)), ms(900)),
        ms(20)
    );
    // Short work may not move it: zero, not the wall clock.
    assert_eq!(
        cpu_time_between(at(Some(5), Some(0)), at(Some(5), Some(0)), ms(3)),
        Duration::ZERO
    );
    // schedstat dead (`0 0 0`) on long work: stat instead.
    assert_eq!(
        cpu_time_between(at(Some(0), Some(10)), at(Some(0), Some(90)), ms(100)),
        ms(80)
    );
    // No schedstat at all: stat.
    assert_eq!(
        cpu_time_between(at(None, Some(10)), at(None, Some(30)), ms(25)),
        ms(20)
    );
    // Both dead, or neither there: the wall clock.
    assert_eq!(
        cpu_time_between(at(Some(0), Some(0)), at(Some(0), Some(0)), ms(2_000)),
        ms(2_000)
    );
    assert_eq!(
        cpu_time_between(at(None, None), at(None, None), ms(7)),
        ms(7)
    );
}
