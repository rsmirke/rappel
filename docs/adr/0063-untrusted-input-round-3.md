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

### Entry points

Numbers continue those of ADR 0050 and the phase 2 ADRs (rows 39 to 49). "Held" means the attack's test passed on the code as it was.

| # | Attack | Layer | Result | Test |
|---|---|---|---|---|
| 50 | **A small image whose header claims a huge size** (PNG 100,000 × 100,000, 16,385 px wide, 81 MP; JPEG 60,000 × 60,000; WebP canvas 2^24 × 2^24; GIF 65,535 × 65,535) | app, before glycin | **Fixed, found by this test.** The size was checked only after glycin's loader had parsed the header (`within_limits` on its reply): without a sandbox (a container, ADR 0055) the loader has no memory limit and asked for 30 GB. Rappel now reads the size itself (`media::image_size`: PNG's `IHDR`; a JPEG's first `SOFn`, every segment before it stepped over by its length; WebP's `VP8 `, `VP8L` or `VP8X` canvas; a GIF's screen and first image, the larger) and sends nothing past 16,384 px a side or 64 MP, nor a file whose header cannot be read, to glycin. Real files of Pillow (PNG, baseline, progressive and EXIF JPEG, lossy, lossless and animated WebP, still and animated GIF) give their sizes; every cut of them never panics; 10 MB of segments or sub-blocks are stepped over in one pass. | `render::picture::tests::images_too_large_by_their_header_never_reach_the_decoder` (counts the loads sent to glycin), `media::dimensions::tests::*` |
| 51 | **Metadata bombs in an image**: a PNG `iCCP` (ICC profile) of 64 MiB compressed into 64 KiB, `zTXt` and `iTXt` (XMP) of 64 MiB compressed, 9 MiB of `eXIf`, a JPEG with 150 ICC segments of 64 KiB | glycin, then the app | **Fixed, found by this test: the ICC profile.** glycin's loader unpacks the profile in its sandbox, then sends it to the app's process, where glycin parses it with lcms2 and applies it (`glycin` 3.1, `dbus.rs`, whatever `apply_transformations` says): Rappel's process grew by **904 MiB** for the 64 KiB chunk (10 MB of it would be gigabytes), and an untrusted profile was parsed outside the sandbox. Rappel now sends the image without its profile (`media::without_color_profile`: PNG's `iCCP`, a JPEG's `ICC_PROFILE` APP2 segments, WebP's `ICCP` chunk with its `VP8X` flag; a structure that does not hold is not decoded). Shown as sRGB: a wide-gamut photo looks a little less saturated (accepted). **Held:** the text chunks are unpacked in the loader only (2 MiB in Rappel's process); EXIF reaches the process (orientation), bounded by the file's 10 MB (25 MiB). Now the whole test grows Rappel's peak by less than 128 MiB (bound). | `render::picture::tests::metadata_bombs_and_many_frames_stay_in_the_loader`, `media::color::tests::*` |
| 52 | **Many frames**: a GIF of 20,000 frames (slice 3), an animated PNG of 10,000 frames | glycin | **Held:** the first frame is shown in about 25 ms. | `a_gif_with_many_frames_shows_its_first`, `metadata_bombs_and_many_frames_stay_in_the_loader` |
| 53 | **Truncated and corrupt images**, SVG behind a PNG signature, a decompression bomb (768 MiB of pixels in under 1 MB) | glycin | **Held** (slice 3's tests): each a placeholder in 15 to 20 ms. | `hostile_images_are_placeholders_in_time` |
| 54 | **The unsandboxed fallback** (glycin's loader as a plain process, no memory limit) | app | **Bounded now** by rows 50 and 51: no image past the pixel limits and no ICC profile reaches the loader; what it can still allocate is about the decoded image (at most 64 MP, 256 MiB) and the file's own size. Not limited by Rappel: a decoder bug inside those bounds runs as the user (ADR 0055). | rows 50, 51 |

## Consequences

- An import with many media files applies tens of times faster; nothing else changes for the user.
- Images are shown without their colour profile, as sRGB.
