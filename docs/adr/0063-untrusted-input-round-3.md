# 63. Untrusted input, round 3: images, audio, archives and the Anki import; batched syncs; time bounds

- Status: Accepted
- Date: 2026-10-19
- Amends: ADR 0054 (an import's apply syncs its new media files in batches before naming them), ADR 0025, ADR 0050 (rows 50 on of the untrusted-input table)
- Refers to: SPEC §4.1, §4.3, §4.9, §4.11, §4.10; `docs/briefs/phase-2.md` (slice 10); ADR 0062 (the GStreamer allow-list, found and fixed in the same slice)

## Context

Phase 2 added images (ADR 0055), audio (ADR 0056), media in the archive (ADR 0054), the streaming import (ADR 0053) and the Anki import (ADR 0061). Each came with its own hostile tests. This round, as rounds 1 and 2 did (ADR 0025, ADR 0050), writes the attacks the brief lists as tests first, runs them against the code as it was, fixes what fails, and records every attack with its result. It also takes the open items earlier slices left for it: the cost of syncing every media file of an import, time bounds that failed under load, two unexplained deaths of the app's test process, pasting a file that is neither an image nor audio, and the first audio play's wait.

## Decision

### Method

As ADR 0050: each attack is a test; a test that passes at once means the defence already held, and says so below; for every fix, its test was run against a scratch copy of the code with the fix reverted (`git archive` unpacked under `target/tmp`, never the working tree) and failed there. Time bounds are CPU time at the reference machine's speed (ADR 0049).

### Batched syncs for an import's media (slice 10, item C1)

**Before:** an import's apply wrote each new media file to a temporary file, synced it (`fsync`), gave it its name, and synced the folder once at the end. Each `fsync` waits for a journal commit: 10,000 small files took **68 s** to apply, and 99,998 took over 17 minutes (measured in slice 2).

**Now** (`media::MediaBatch`): each file is copied into a temporary file and checked exactly as before (kind, cap, structure, hash), but not synced alone. When the batch holds 128 files (or 256 MiB), and at the end, its files are synced together on up to 64 threads, then each is named (`give_name`, unchanged), and at the end the folder is synced once. Parallel `fsync`s share journal commits: on this machine (ext4 on an SSD, debug build, a scratch data folder under `target/tmp`):

| 10,000 small files | Apply |
|---|---|
| Before (one `fsync` after another) | 68.2 s |
| 1 sync thread | 66.3 s |
| 8 threads | 16.0 s |
| 32 threads | 4.8 s |
| **64 threads (chosen)** | **2.9 s** |
| 128 threads | 2.0 s |
| No `fsync` at all (not kept, for comparison) | 1.0 s |

99,998 files now apply in **28.9 s**. A batch of 512 files is no faster than 128, and each pending file is an open file, so 128 keeps far from the usual limit of 1,024. 64 threads, each with a 64 KiB stack, only wait.

**Durability, the argument of ADR 0054 kept:** a media row commits only after its file's content and its name are on disk. A file is named only after its content is synced (the batch's sync comes before any of its names); every name is synced (the folder) before `finish` returns; `finish` returns before the transaction commits; any error drops the transaction. So after a crash at any point: no row without its file, and no media name with incomplete content, as before. What changes: a crash before the commit can leave up to one batch (128) of `.incoming-…` temporary files instead of one, which a clean-up leaves alone as before (ADR 0057); and an import that fails or is cancelled removes the files of the batch it has not named (`Drop`), where before every file copied stayed. Tests: `media::tests::a_batch_names_its_files_once_synced_and_leaves_no_temporary_file`, `a_failed_or_dropped_batch_removes_what_it_has_not_named`, and every import test (both formats). The speed itself is measured, not pinned by a test: a bound on the time spent waiting for a disk would fail on a slow one, and ADR 0049 times CPU only.

## Consequences

- An import with many media files applies tens of times faster; nothing else changes for the user.
