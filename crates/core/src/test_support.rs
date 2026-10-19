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

/// A small PNG for media tests: the signature, an `IHDR` chunk and `n`
/// as the image's width, so files made with different `n` differ. Not a
/// valid image: only its kind matters to the media store (ADR 0054).
pub(crate) fn png_bytes(n: u32) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 1, 8, 6, 0, 0, 0]);
    out
}

/// A small WAV for media tests: a RIFF/WAVE header and `n` as the sample
/// rate, then `n % 64 + 1` bytes of silence (and the pad byte a chunk of
/// odd length has, so it is a whole WAV, `check_structure`).
pub(crate) fn wav_bytes(n: u32) -> Vec<u8> {
    let data = (n % 64 + 1) as usize;
    let pad = data % 2;
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(36 + data + pad).unwrap().to_le_bytes());
    out.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(b"\x01\0\x08\0data");
    out.extend_from_slice(&u32::try_from(data).unwrap().to_le_bytes());
    out.extend(std::iter::repeat_n(0x80, data));
    out.extend(std::iter::repeat_n(0, pad));
    out
}

/// The stack hostile input is parsed on in tests: a quarter of what a
/// spawned thread gets by default (2 MiB) and a sixteenth of the main
/// thread's usual 8 MiB, so a recursion that only just passes here has a
/// wide margin in the app. A stack overflow aborts the process; it cannot
/// be caught.
pub(crate) const SMALL_STACK: usize = 512 * 1024;

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
pub(crate) struct CpuReading {
    pub(crate) schedstat: Option<Duration>,
    pub(crate) stat: Option<Duration>,
}

/// Reads [`CpuReading`] for the calling thread.
pub(crate) fn thread_cpu_time() -> CpuReading {
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
pub(crate) fn cpu_time_between(start: CpuReading, end: CpuReading, wall: Duration) -> Duration {
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
pub(crate) fn cpu_time_of<T>(f: impl FnOnce() -> T) -> (Duration, T) {
    let wall = Instant::now();
    let start = thread_cpu_time();
    let value = f();
    let used = cpu_time_between(start, thread_cpu_time(), wall.elapsed());
    (used, value)
}

/// How long [`within`] waits before it calls the work hung: ten times its
/// CPU-time bound, and at least 30 s. It is a wall clock, so it grows with
/// the load on the machine; a thread that gets a third of a CPU (three busy
/// threads per core) still finishes far within it.
pub(crate) fn hang_limit(limit: Duration) -> Duration {
    limit.saturating_mul(10).max(Duration::from_secs(30))
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
/// once divided by the [`slowdown`] measured now, on this thread ([`judge`]).
/// Only a time over the bound pays for the measurement, and the measurement
/// is taken in the conditions the work just ran in.
pub(crate) fn cpu_within(used: Duration, limit: Duration) -> Result<(), String> {
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
/// `limit` ([`cpu_within`]). Call it on the thread that did the work.
#[track_caller]
/// [`cpu_within`] for work that can be run again from the same state:
/// `measure` runs it and returns the CPU time it used, on this thread. A
/// time over `limit`, even at the slowdown measured then, is measured
/// again, up to [`TRIES`] times in all, and the first verdict within the
/// bound passes. A burst of load during one run (the slowdown is measured
/// after the work, and the reference does not slow down exactly as the
/// work does, ADR 0049) then fails nothing, while a regression, slow every
/// time, still fails (ADR 0063).
pub(crate) fn cpu_within_retried(
    limit: Duration,
    mut measure: impl FnMut() -> Duration,
) -> Result<Duration, String> {
    let mut last = String::new();
    for _ in 0..TRIES {
        let used = measure();
        match cpu_within(used, limit) {
            Ok(()) => return Ok(used),
            Err(message) => last = message,
        }
    }
    Err(format!("{last} (measured {TRIES} times)"))
}

/// How many times [`cpu_within_retried`] measures before it fails.
pub(crate) const TRIES: usize = 3;

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
    // Measured, not only declared: a few milliseconds at least, so a CPU
    // clock that reads zero or does not move fails here.
    assert!(times[0] >= Duration::from_millis(5), "{times:?}");
    assert!(slowdown() >= 1.0);
}

/// A slow measurement is taken again; a time slow every time fails after
/// [`TRIES`] measurements.
#[test]
fn a_time_over_its_bound_is_measured_again() {
    let limit = Duration::from_millis(100);
    let mut calls = 0;
    let got = cpu_within_retried(limit, || {
        calls += 1;
        if calls < 3 {
            Duration::from_secs(1_000)
        } else {
            Duration::from_millis(50)
        }
    });
    assert_eq!(got, Ok(Duration::from_millis(50)));
    assert_eq!(calls, 3);
    let mut calls = 0;
    assert!(
        cpu_within_retried(limit, || {
            calls += 1;
            Duration::from_secs(1_000)
        })
        .is_err()
    );
    assert_eq!(calls, TRIES);
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

/// A test hook: code a test runs at a point of the code under test,
/// through a thread-local (`import::BEFORE_READ`, the Anki import's
/// `AFTER_CARD`, `BEFORE_COMMIT`, `BEFORE_CHECK`).
pub(crate) type Hook = std::cell::RefCell<Option<Box<dyn FnMut()>>>;

/// Sets `hook` to `f` on this thread until the returned guard is dropped,
/// even if the test panics meanwhile, so a later test on the thread never
/// runs it.
#[must_use = "the hook is set as long as the guard lives"]
pub(crate) fn set_hook(
    hook: &'static std::thread::LocalKey<Hook>,
    f: impl FnMut() + 'static,
) -> HookGuard {
    hook.with_borrow_mut(|h| *h = Some(Box::new(f)));
    HookGuard(hook)
}

/// Clears its hook when dropped ([`set_hook`]).
pub(crate) struct HookGuard(&'static std::thread::LocalKey<Hook>);

impl Drop for HookGuard {
    fn drop(&mut self) {
        self.0.with_borrow_mut(|h| *h = None);
    }
}
