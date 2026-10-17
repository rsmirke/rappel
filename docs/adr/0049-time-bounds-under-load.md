# 49. Time bounds that hold on a loaded machine

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0025 (how a time bound is measured: "No hang: a time bound on a separate thread", and the speed tables)
- Refers to: #67, #66, `docs/briefs/phase-1-finish.md` (slice 3)

## Context

ADR 0025 put a wall-clock bound on hostile input and on the work at 5,000 cards, at about ten times the measured time. A wall clock counts the time a thread waits for a CPU, so a loaded machine fails the bounds: in #66, `deck_reads_are_fast_at_five_thousand_cards` took 361 ms against 150 ms while builds ran. Run under load (three busy loops per CPU, 60 on the maintainer's 20-thread i7-12700H), `cargo test --workspace` failed **every time, with 7 to 11 tests per run** on `phase-1` (table below).

A first step, measuring the thread's CPU time instead of the wall clock, was not enough: one run in three still failed. Load slows a thread even in CPU time. A busy loop on the other hyperthread of the core, a lower clock speed with every core busy, a move to one of the slower E-cores, and shared caches made `study_input` cost 3.4 times as much CPU time, and up to 7.6 times in the full parallel run (198 ms against 26 ms unloaded). Raising the bounds by that much would let a regression of the same size through, which is what they are there to catch.

## Decision

Every time bound in a test checks **CPU time of the thread that did the work, at the speed of the machine the bounds were set on**, and a **separate wall-clock timeout catches hangs**. The helpers are in `crates/core/src/test_support.rs` and, the same, in `crates/app/src/testing.rs` (core's test helpers are not exported).

1. **CPU time** (`thread_cpu_time`, `cpu_time_of`). Read from `/proc/thread-self/schedstat`, the thread's `sum_exec_runtime` in nanoseconds, kept up to date at each scheduler tick and context switch (`proc_pid_schedstat` in Linux `fs/proc/base.c`), so a reading is at most one tick old (1 ms at `CONFIG_HZ=1000`). The fallback is `/proc/thread-self/stat` in 10 ms ticks, and then the wall clock. A clock that reads but does not move is treated as missing (`cpu_time_between`): an old kernel prints `0 0 0` in `schedstat`, which would make every bound pass. If `schedstat` has not moved after 50 ms of wall-clock time, `stat` is used, and if `stat` has not moved after 1 s, the wall clock is used. Work that runs that long has either seen a scheduler tick or been switched out, and both update the sum. `reference_work_is_long_enough_to_time` also checks that the measured reference takes at least 5 ms, so a clock that reads zero fails the gate. The check of a time against its bound is a pure function (`judge`), with unit tests. No new dependency and no `unsafe`: `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` would need `libc` or `rustix`. Waiting for a CPU no longer counts. Work shorter than a few ticks is timed in a batch (`decks` and `deck` 100 times each).
2. **Slowdown against a reference** (`slowdown`, `cpu_within`, `assert_cpu_within`). A fixed piece of work (format, sort and look up 20,000 strings in a debug build) took 17 ms of CPU on that machine unloaded (`REFERENCE_CPU`; 21 ms for its 100,000 strings in a release build). When a time is over its bound, the reference is run three times on the same thread, at once, and the time is divided by how much slower the best of them was, never by less than 1. Under the same load the reference got 2.4 times slower when `study_input` got 3.4 times slower, so the ratio moved by 1.4, well inside the bounds' margin of about ten. Only a time over its bound pays for the reference, so the gate is not slower. Never dividing by less than 1 means a faster machine is held to the bounds as they are, so the numbers in the tests and in ADR 0025 keep their meaning.
3. **Best of several runs** (`fastest`), as before, for reads that can be repeated: it leaves out a run with a cold cache.
4. **Hangs are a separate bound** (`within`, `hang_limit`). `within(limit, …)` still runs the work on a small stack and fails a stack overflow and a hang; the hang timeout is now a wall clock of ten times the limit, at least 30 s. A file that blocks the import (a FIFO) uses no CPU at all, so only that timeout catches it.
5. **The work is still asserted where it can be**: the browser page builds fewer than 500 rows for 5,000 cards, binding a row to 4,000 cards makes no widgets, a hostile side has no more nodes than bytes. These were already there and do not depend on time.

The bounds themselves are unchanged. What each one protects, and why this measure:

| Test | Bound | Protects against | Measure |
|---|---|---|---|
| core `storage::tests::perf::*` (deck reads, tagged list, import plan, browse at 5,000 and 20,000, bulk writes, search index) | 100 ms to 9 s | a query per card, a missing index, a quadratic loop | best of 3 (1 for writes), CPU, slowdown |
| core `perf::deck_reads…`: `decks`/`deck` ten times cheaper than `list_decks` | ratio | counting cards to find a deck | CPU, 100 calls per sample (a ratio needs no slowdown) |
| core `storage::stats::tests::stats_are_fast…` | 2 s, 100 ms | stats or card info replaying the whole log per card | best of 3, CPU, slowdown |
| core `content::tests::hostile_sides_*`, `repeated_random_syntax_*` | 6 s, 2 s | a parser blow-up on 1 MiB | `within` |
| core `content::tests::many_leading_hard_breaks_are_fast` | 20 s | the quadratic hard-break case (minutes) | CPU, slowdown |
| core `scheduling::tests::a_log_of_many_undos_rebuilds_in_time` | 2 s | each undo searching the whole log | `within` |
| core `interchange::tests::fuzz` (`PLAN_LIMIT`), `refused::only_regular_files_are_imported` | 5 s | a slow or blocking hostile file | `within` (the FIFO case is the hang timeout) |
| app `render::tests::the_longest_shown_side_lays_out_in_time` | 3 s, 200 ms | a 32 KiB side or a long run freezing the window | CPU, slowdown |
| app `render::breaks::…a_cluster_built_from_many_pieces_costs_little` | 500 ms | `last` growing without a cap (3 s) | `within` + CPU of the loop |
| app `render::math::…hostile_sources_at_the_limit…`, `random_sources…`, `repeated_random_syntax…`, `a_side_of_the_largest_valid_formulas…` | 650 ms, 2 s, 60 s | RaTeX blow-ups | `within_stack`; CPU, slowdown |
| app `deck_page::tests::finding_the_last_of_5000_cards_is_fast` | 20 ms | a scan that is not linear | `within` + CPU of the scans |
| app `browser_page` `five_thousand_cards_stay_fast_and_virtual` | 5 s, 2 s, 2 s | a slow load, search or select all | CPU, slowdown (and the row count) |
| app `card_editor::tests::a_huge_one_paragraph_side…` | 1 s, 2 s | a frozen editor on 1 MiB | CPU per main-loop step (`longest_stall`), slowdown. A step that blocks without using CPU, such as one waiting on a lock or on I/O, is no longer counted as a stall. |
| app `card_row::tests::binding_…_makes_no_widgets` (both) | 2 s | widgets made per bind | CPU, slowdown (and the widget counts) |
| app `review_page::tests::a_reveal_of_the_largest_backs_stays_cheap` | 2 s | an animation that relays out the back | CPU, slowdown |

Waits that are not bounds stay wall clock, because they wait for GTK or a timer and only stop a test that would hang: `testing::settle_until` (5 s or 20 s), the debounce tests, and the editor's `longest_stall` (20 s to settle). The review page's "just shown" key test now sets the time it checks right before the key, instead of when its window was made.

## Results

`cargo test --workspace --no-fail-fast` under Xvfb, with 60 busy loops (`while :; do :; done`) on the 20-thread machine, each run separately:

| Code | Run 1 | Run 2 | Run 3 |
|---|---|---|---|
| `phase-1` (wall clock) | 11 failed | 7 failed | 8 failed |
| CPU time only | 1 failed (`deck_reads…`, 198 + 23 ms of CPU against 150) | 0 | 0 |
| This ADR | 0 failed | 0 failed | 0 failed |

Unloaded, every run passes too. Under that load the reference took 3.0 to 6.8 times as long as unloaded, and it had to be measured once per run: every other time was within its bound in plain CPU time. Without load, the gate takes no longer than before.

The bounds still fail the regressions they are there for. Checked in a scratch copy, not in the repository, with this ADR's helpers:

| Regression | Test | Result |
|---|---|---|
| The `review_events_card_reviewed` index dropped | `deck_reads_are_fast…` | 4.29 s of CPU against 150 ms (3.83 s at a slowdown of 1.12). **Under the same load as above: 9.58 s of CPU, 3.81 s at a slowdown of 2.51**, the same figure as unloaded |
| The same | `bulk_writes…` | `undo_entries` 25.7 s against 9 s |
| `position_in` made quadratic | `finding_the_last_of_5000_cards_is_fast` | 3.08 s against 20 ms |
| `position_in` doing ten times the work per card, still linear | the same | 23 ms against 20 ms: a regression of the size the bound was set for fails |
| `breaks::Runs` no longer capping `last` | `a_cluster_built_from_many_pieces_costs_little` | 3.48 s against 500 ms |
| The import's regular-file check removed (a FIFO is opened) | `only_regular_files_are_imported` | "planning a special file hung: no result after 50s" |

## Consequences

- A time bound is "this much CPU time on the maintainer's laptop". A much slower CI machine is measured by its slowdown the same way, so it passes as long as the code is not slower than the bound there either.
- `REFERENCE_CPU` was measured on one machine. If the reference work, the build profile or the compiler changes its speed a lot, measure it again (`RAPPEL_TIMINGS=1 cargo test -p rappel-core reference_work -- --nocapture`). Too small a value only makes the bounds less strict under load; too large a value never makes them stricter than the plain CPU time, since the slowdown is never below 1.
- **Under load a bound is less strict.** In a run where the measured slowdown is s, a time is divided by s before it is compared with its bound. So a regression of up to about 10·s times the measured time (the bounds are about ten times the measured time) passes in that loaded run. An unloaded run, where s is 1, still catches it, and so does CI on an idle runner. The reference work is never divided, so a regression in the code under test is not hidden by the reference itself.
- The reference does not slow down exactly as the work does (1.4 apart under three busy threads per CPU). A load much heavier than that can still fail a bound that is close to its limit; most bounds are about ten times the measured time.
- Tests time work on the thread that did it, and check it there (`cpu_within` returns a verdict for code that runs inside `within` or on the GTK thread).
- Linux only, as before (`/proc`); elsewhere the wall clock is the fallback.
