//! Helpers shared by the tests of several modules. Test builds only.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::domain::{CardState, Grade, ReviewEvent, Schedule};
pub(crate) use crate::scheduling::DAY_MS;
use crate::storage::Store;

/// 2026-10-15T12:34:56.789Z, the "now" most tests start from.
pub(crate) const T0: i64 = 1_792_067_696_789;

/// The schedule of a card that must be scheduled.
pub(crate) fn scheduled(state: CardState) -> Schedule {
    match state {
        CardState::Scheduled(s) => s,
        CardState::New => panic!("expected a scheduled card"),
    }
}

/// Records `grade` for `card` at `at`, answered in 3 seconds.
pub(crate) fn answer(store: &mut Store, card: &str, grade: Grade, at: i64) -> ReviewEvent {
    store
        .record_review(card, grade, Duration::from_secs(3), at)
        .unwrap()
}

/// A `card_schedule` row: card id, phase, due, stability and difficulty as
/// bits, last review, reps, lapses.
pub(crate) type ScheduleRow = (String, String, i64, u64, u64, i64, i64, i64);

/// Every `card_schedule` row, with the floats as bits so equality is exact.
pub(crate) fn schedule_table(store: &Store) -> Vec<ScheduleRow> {
    let mut stmt = store
        .conn
        .prepare(
            "SELECT card_id, phase, due_at, stability, difficulty, last_review_at, reps, lapses
             FROM card_schedule ORDER BY card_id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get::<_, f64>(3)?.to_bits(),
            r.get::<_, f64>(4)?.to_bits(),
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
        ))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// `path` with `suffix` appended to its file name, e.g. the `-wal` file of
/// a database.
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The stack hostile input is parsed on in tests: a quarter of what a
/// spawned thread gets by default (2 MiB) and a sixteenth of the main
/// thread's usual 8 MiB, so a recursion that only just passes here has a
/// wide margin in the app. A stack overflow aborts the process; it cannot
/// be caught.
pub(crate) const SMALL_STACK: usize = 512 * 1024;

/// The CPU time the calling thread has used so far, or `None` where Linux's
/// `/proc` cannot be read. Unlike a wall clock it does not grow while the
/// thread waits for a CPU, so a bound on it holds on a loaded machine (#67,
/// ADR 0049). Read from `/proc/thread-self/schedstat`, whose first field
/// is the thread's `sum_exec_runtime` in nanoseconds (Linux
/// `fs/proc/base.c`, `proc_pid_schedstat`). The kernel brings that sum up
/// to date at each scheduler tick and context switch, not on the read, so
/// a reading can be up to one tick old (1 ms at `CONFIG_HZ=1000`, 4 ms at
/// 250): time work of a few milliseconds in batches. Where that file is
/// missing, it falls back to the utime and stime of
/// `/proc/thread-self/stat`, in `USER_HZ` clock ticks of 10 ms.
pub(crate) fn thread_cpu_time() -> Option<Duration> {
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

/// How long [`within`] waits before it calls the work hung: ten times its
/// CPU-time bound, and at least 30 s. It is a wall clock, so it grows with
/// the load on the machine; a thread that gets a third of a CPU (three busy
/// threads per core) still finishes far within it.
pub(crate) fn hang_limit(limit: Duration) -> Duration {
    limit.saturating_mul(10).max(Duration::from_secs(30))
}

/// The CPU time `f` used on this thread, and its result: the thread's CPU
/// time where `/proc` can be read ([`thread_cpu_time`]), else the wall
/// clock.
pub(crate) fn cpu_time_of<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let wall = Instant::now();
    let start = thread_cpu_time();
    let value = f();
    let used = start
        .zip(thread_cpu_time())
        .map_or_else(|| wall.elapsed(), |(start, end)| end.saturating_sub(start));
    (used, value)
}

/// A fixed piece of work that allocates, formats, sorts and looks up, as
/// the code under test does: the yardstick of [`slowdown`]. Larger in a
/// release build, so it stays well above the CPU clock's precision.
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
/// even in CPU time (a busy thread on the same core, a lower clock speed,
/// a slower core, shared caches): measured at three busy threads per CPU,
/// the reference took 2.4 times as long and `study_input` 3.4 times, so
/// the ratio of the two moved by 1.4 instead of 3.4 (ADR 0049). Never less
/// than 1, so a faster machine is held to the bounds as they are.
pub(crate) fn slowdown() -> f64 {
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
pub(crate) fn cpu_within(used: Duration, limit: Duration) -> Result<(), String> {
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
/// `limit` ([`cpu_within`]). Call it on the thread that did the work.
#[track_caller]
pub(crate) fn assert_cpu_within(used: Duration, limit: Duration, what: &str) {
    if let Err(message) = cpu_within(used, limit) {
        panic!("{what}: {message}");
    }
}

/// Runs `f` on a new thread with a [`SMALL_STACK`] and returns its result.
/// Two bounds, kept apart (ADR 0049):
/// - **Performance:** the test fails if `f` used more than `limit` of CPU
///   time ([`cpu_within`], on that thread). Waiting for a CPU on a loaded
///   machine does not count, and a thread slowed by the load is measured
///   against a reference, so the bound holds under load and still fails a
///   blow-up.
/// - **Hang:** the test fails if `f` has not returned after
///   [`hang_limit`] of wall-clock time, so a hang, or work blocked on a
///   pipe that uses no CPU at all, fails the test instead of blocking the
///   run (the thread is left behind; the test process ends anyway).
///
/// `RAPPEL_TIMINGS=1` prints the CPU and wall-clock times.
pub(crate) fn within<T: Send + 'static>(
    limit: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let wall = Instant::now();
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let (cpu, value) = cpu_time_of(f);
            let verdict = cpu_within(cpu, limit);
            // The receiver is gone only if the test already failed.
            let _ = tx.send((cpu, verdict, value));
        })
        .unwrap();
    let hang = hang_limit(limit);
    match rx.recv_timeout(hang) {
        Ok((cpu, verdict, value)) => {
            if std::env::var_os("RAPPEL_TIMINGS").is_some() {
                eprintln!("{what}: {cpu:?} CPU, {:?} wall", wall.elapsed());
            }
            if let Err(message) = verdict {
                panic!("{what}: {message}");
            }
            value
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what} hung: no result after {hang:?}")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("{what} panicked"),
    }
}

/// The least CPU time of `runs` runs of `f` on this thread
/// ([`cpu_time_of`]), and its last result. CPU time leaves out the time
/// spent waiting for a CPU; the best of several runs also leaves out a run
/// slowed by a cold cache. Check it with [`assert_cpu_within`] on the same
/// thread. With `RAPPEL_TIMINGS=1` the time is printed (ADR 0025, ADR
/// 0049).
pub(crate) fn fastest<T>(what: &str, runs: usize, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut value = None;
    for _ in 0..runs {
        let (used, v) = cpu_time_of(&mut f);
        best = best.min(used);
        value = Some(v);
    }
    if std::env::var_os("RAPPEL_TIMINGS").is_some() {
        eprintln!("{what}: {best:?} CPU");
    }
    (best, value.expect("`runs` is at least 1"))
}

/// [`REFERENCE_CPU`] is what this machine measures: run with
/// `RAPPEL_TIMINGS=1 cargo test -p rappel-core reference_work -- --nocapture`
/// on an unloaded machine to see it. The test only checks that the
/// reference is long enough for the CPU clock's precision (one tick).
#[test]
fn reference_work_is_long_enough_to_time() {
    let mut times: Vec<Duration> = (0..5).map(|_| cpu_time_of(reference_work).0).collect();
    times.sort_unstable();
    if std::env::var_os("RAPPEL_TIMINGS").is_some() {
        eprintln!(
            "reference work: least {:?}, median {:?}",
            times[0], times[2]
        );
    }
    assert!(REFERENCE_CPU >= Duration::from_millis(10));
    assert!(slowdown() >= 1.0);
}
