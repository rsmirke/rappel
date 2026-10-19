# Phase 2: final report

This report covers batch mode's run through `docs/briefs/phase-2.md`, and it is written for the maintainer.

**How each slice was done:**
- I planned it here, and a fresh sub-agent implemented it.
- It was checked with `scripts/check.sh` under Xvfb.
- The `reviewer` agent reviewed it until it had no blocking findings. For the riskier slices, the reviewer also reverted each fix in a scratch copy, to confirm that its test fails without it.
- It then went through a PR into `phase-2`, waited for a green CI `check`, and was squash-merged.
- Nothing was merged into, pushed to or aimed at `main`, except the draft PR `phase-2` → `main` that follows this report.

**Version:** **0.3.0-beta.1** in `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md` and the AppStream metainfo. It is **not tagged**.

## What changed, per slice

| Slice | PR | ADR | In short |
|---|---|---|---|
| — | #87, #88 | — | Phase 1's open #56 (Reset Progress is off for new cards), re-implemented on phase 2; the screenshot harness `scripts/screenshot.py`. |
| 0. Phase 1 questions | #89 | 0052 | A damaged setting can be reset after a backup. A restore keeps a waiting restore copy aside. Suggestions and filters offer only tags that are in use. |
| 1. Streaming import | #90 | 0053 | Rappel's own bounded zip reader. Memory stays bounded at any archive size. |
| 2. Media store | #91 | 0054 | Migration 0011 (`media` table), `media:<sha256>.<ext>` references, media in export format v2. |
| 3. Images | #92 | 0055 | Images shown through glycin's sandboxed loaders; Insert Image…, drop and paste; full-size view. |
| 4. Audio | #93 | 0056 | Playback through GTK's media stream, a whole-file structure check, R to replay, Insert Audio…. |
| 5. Clean Up Media | #94 | 0057 | Unused files move to a dated trash, are purged after 30 days, and come back if needed. |
| 6. Phone and touch | #95 | 0058 | Usable at 360×640 and 640×360; the on-screen keyboard never hides the focused field; touch-sized pills; short toasts with Details; no swipe-to-grade. |
| 7. Translation | #96 | 0059 | gettext, every string marked, plurals, French numbers and dates, and a full French catalog that **needs your proofreading**. |
| 8. Desktop drafts | #97 | 0060 | `.desktop` and AppStream metainfo (validated in the gate), a placeholder icon, five store screenshots. No Flatpak manifest. |
| 9. Anki import | #98 | 0061 | `.apkg` and `.colpkg` in all three formats, with history, schedule, decks, tags, media and a report of what was skipped. |
| 10. Clean-up and hardening 3 | #99 | 0062, 0063 | The decoder allow-list (SPEC §4.9 is now met), a **crash fix for tagged MP3s**, image and archive attacks, imports with many media files about 35× faster. |
| 11. Beta preparation | #100 | — | 0.3.0-beta.1, this report. The draft PR `phase-2` → `main` follows it. |

Each PR's description has the full **What**, **Decisions**, **Reviewer** and **Try it** sections. Below are the parts you most need.

### Highlights
- **Media.**
  - Cards show images and play audio from a content-addressed store in the data folder. Media files travel in exports (format v2) and in imports.
  - Nothing is ever fetched from the network, SVG is never rendered, and every file is checked by its bytes before anything decodes it.
  - Unused files can be cleaned up from Preferences → Storage. They go to a dated trash and are deleted only after 30 days, and only if no backup uses them.
- **Phone and touch.**
  - Every screen was audited at 360×640 and 640×360, in light and dark (the table is in ADR 0058).
  - When the window gets shorter, as an on-screen keyboard makes it, the focused field scrolls back into view.
  - Tag pills are touch-sized and names are never cut.
  - Long toasts get a short title plus **Details**.
- **French.** Every string goes through gettext, with plural forms and French typography ("5 210", "91 %", « … »).
  - Run in French: see README → Translations.
  - Tests keep the catalog complete. They also check that French toast titles still fit, which caught several.
- **Anki import.** Import… accepts Anki packages.
  - Each Anki card becomes one Rappel card; clozes become front/back cards.
  - HTML becomes safe Markdown and MathJax becomes Rappel's math.
  - Review history is kept (Again → Fail, the rest → Pass) and the schedule rebuilt from it.
  - "A::B" decks become "A › B".
  - Before anything is written, a dialog lists what will be left out or changed.
- **Hardening.**
  - Images, audio, zip archives, the streaming import and Anki packages were attacked with tests.
  - The ADR 0063 table lists every attack, the layer that stops it, and its test.

### Bugs found and fixed along the way
- **Playing an MP3 with an ID3 tag closed Rappel** (since #93, fixed in #99). GStreamer 1.28.7 aborts the process on such a stream. The tests had only played WAVs. The player now gets only the audio frames.
- **On small windows, Enter in an alert ran the destructive response** (delete a card, start fresh, restore, clean up). libadwaita focused the last response. Fixed for every alert in #94.
- **A hostile image's colour profile** grew Rappel's process by about 900 MiB, because glycin parses profiles in Rappel's own process. Profiles are now stripped (#99).
- **Anki packages:** the review found eight blocking problems, all fixed before merging (#98):
  - a 64 KB package could use 1.4 GB;
  - SQL size checks loaded the whole value;
  - virtual tables and generated columns ran code;
  - templates had unbounded work;
  - a clock-dependent plan made some packages impossible to import;
  - two tests passed without their fixes.

## Open questions (`docs/briefs/QUESTIONS.md`, Phase 2)
1. **Four ignored "unmaintained" advisories in glycin's tree** (`paste`, `zvariant`, `zvariant_derive`, `zbus_names`).
   - None is a vulnerability, and none has a fixed version.
   - `zvariant` parses the loader process's replies, so it sits on the sandbox boundary.
   - Question: keep the ignores (to review on every glycin release, by 2027-01-16 at the latest), or decode images another way?
2. *Answered in #99:* the audio decoder allow-list now exists without new packages (ADR 0062).

No stop condition was hit in this phase. The brief pre-approved the migration (0011), export format v2 and the media trash.

## Things to know
- **French needs you.**
  - `po/fr.po` says "NEEDS PROOFREADING BY THE MAINTAINER". Put your name in `translator-credits`, which says "Claude (Anthropic)" until then.
  - Error details from `rappel-core` (the text behind Details) stay in English.
  - GTK's and libadwaita's own labels need the system's French language pack.
- **Audio formats.** FFmpeg's GStreamer plugin is not allowed, because it would allow hundreds of decoders at once. So these show "Could not be played":
  - Apple Lossless M4A;
  - AAC on a system whose only AAC decoder is FFmpeg's. The Flatpak runtime must ship `faad` or `fdkaac`.

  Also, MP3s with cut frames, chained Ogg files and fragmented MP4s are refused by the structure check (#93 lists them).
- **Pictures are shown without their colour profile** (as sRGB). Some photos may look slightly less saturated.
- **The first audio play** takes about 0.1–0.35 s while GStreamer builds Rappel's private registry in `~/.cache/rappel/`.
- **The store listing:**
  - The metainfo names you as "Robert Smirke", with the id `io.github.rsmirke`. Please confirm.
  - Its screenshot URLs point at `main`, so they work once this reaches `main`. Pin them to a tag at release time.
  - Flathub wants window shadows: retake the five screenshots on GNOME (Alt+Print) with `fill_scratch_db --demo` before submitting.
  - The icon is a placeholder.
- **The Anki import detects a re-import by content** (same deck, front and back), without a migration. So a card you edit in Rappel after importing it would come in again on a second import.
- **English wording changes:**
  - numbers are always grouped ("1,000 years");
  - Undo toasts are short ("3 cards tagged", "12 files trashed", "Deck deleted").
- **Known limits from the PRs:**
  - **An export whose media add up to more than 2 GiB cannot be imported again** (#91, ADR 0054): the archive limit. Exports are how you back up, so keep that in mind with large media collections.
  - Where bubblewrap cannot make a sandbox, glycin decodes images **without a sandbox and without its memory limit** (#92). Rappel's own size checks (#99) still apply first.
  - A GIF cut off in the middle of a frame is refused as damaged (#99). Before this phase it showed its first frame.
  - Pasting a copied non-media file as its path (#99): only the drop is tested end to end, because GTK's own paste did not insert text under the test harness.
  - Programs that Rappel starts inherit its GStreamer variables (#99).
  - In French, All Cards' filter drop-downs cut their labels. Header subtitles are cut at 360 px, but only when minimise and maximise buttons are shown (#96).
  - In the harness, shrinking the window with Add Tags open turned the typed name into a pill (#95). Check this on a phone.
  - Refusing a 256 MiB file takes about 21 s on a worker that has no Cancel (#90).
- **Unchecked here:**
  - a real on-screen keyboard (Phosh);
  - the GTK file chooser through the portal;
  - a real Anki collection of yours: please try one (Anki: File › Export, "Anki Deck Package", with scheduling).
- **Not reproduced:** twice, an app test process died once without naming a test (in #93 and #96). It did not happen again in 29 runs under load (#99). The likely cause in #96, a race between the C library's locale and gettext, was removed.
- **Version wording to settle:**
  - 0.2.0 itself was never released; only `v0.2.0-beta.1` is tagged, and phase 2 went on from it. SPEC still says phase 1 "ends in v0.2.0". Whether 0.2.0 ships is your call.
  - `CLAUDE.md` still says "v0.2 is in progress". I did not edit it, because it is yours.
- **The machine:** memory ran critically low several times (the desktop session plus an editor's rust-analyzer). Two session restarts interrupted slice 10, and its work was recovered from the tree.

## New dependencies in this phase
All licences were already allowed by `deny.toml`.
- glycin 3.1 (#92), for images. It brings the four unmaintained advisories of question 1.
- gettext-rs 0.8 with the system libintl (#96). It pulls in `locale_config` 0.3, unmaintained since 2019, which also brings `regex`.
- ruzstd 0.9 and twox-hash 2.1 (#98), for the zstd in current Anki packages.
- sha2, flate2 and crc32fast (#90), for the zip reader and the hashes.
- New features of rusqlite, an existing dependency: `collation` and `limits` (#98), and `hooks` (#99).
- New system packages, for tests and CI only: `locales` (#96), `appstream` and `desktop-file-utils` (#97), and `gstreamer1.0-plugins-bad` (#99, for the AAC decoder).

## Screenshots
All are local, in `docs/briefs/screenshots/` (git-ignored). Each PR lists its own shots; the main ones:
- **Media:** `s3-*`, images in the review page and editor; `s4-*`, audio; `s5-prefs-*`, Storage and Clean Up.
- **Phone and touch** (`s6-*`):
  - the final tour `s6-tour-{a..k}-{light,dark}-{360x640,640x360}-*`;
  - before and after: `s6-before-osk-*` / `s6-osk-*`, `s6-before-tags-*` / `s6-after-tags-*`, `s6-d-details-*`.
- **French:** `s7-*` in light, and `s7r-dark-{1000x720,360x640,640x360}-*` in dark.
- **Store:** `s8-store-{review,decks,editor,statistics,phone}.png`. These are copies of the committed `data/screenshots/`.
- **Anki import:**
  - `s9-import-{light,dark}-{1000x720,360x640,640x360}-{plan,done}.png`;
  - `s9-import-{light,dark,fr}-360x640-details.png`;
  - `s9-cloze-*`, `s9-science-*` and `s9-decks-*`.

## Checklist for the maintainer
Scratch data:
```sh
D=$(mktemp -d)
cargo run -p rappel-core --example fill_scratch_db -- "$D" 30 --due 5 --tags --media --unused-media
RAPPEL_DATA_DIR="$D" cargo run -p rappel
```
For the Anki and store checks, use `--demo` instead. Each PR's **Try it** has the detailed steps.

1. **Look at the screenshots** above and tell me what looks wrong.
2. **Images (Media deck):**
   - Images show on the review page and in the editor preview; click one for full size.
   - Insert Image…, a drop from Files and a paste each add an image.
   - A missing image says so.
3. **Audio:**
   - Play a **real tagged MP3** of yours. It must play and Rappel must stay open.
   - R replays. The sound stops on the next card, on Undo and on Edit.
   - Try an M4A of your own (it needs `faad` or `fdkaac` installed, or it says "Could not be played"), and an Ogg.
   - Copy a `.txt` file in Files and press Ctrl+V in the editor: its path goes in.
4. **Clean Up Media:** Preferences → Storage → Clean Up…. Cancel is focused, and Enter cancels. Clean up, then Undo.
5. **Phone sizes,** at 360×640 and 640×360, light and dark:
   - Tour every screen.
   - With the focus in Tags, make the window about 300 px tall: the field stays visible.
   - Long toasts have Details, and Details opens the whole text.
6. **Touch, if you have a touch screen:**
   - Tag pills can be removed with a finger.
   - A swipe on the card never grades.
7. **French** (README → Translations; follow both commands there: generating the locale once with `localedef`, then running):
   - Proofread `po/fr.po`, then put your name in `translator-credits`.
   - Check the screens at 360 px.
   - Numbers read "5 210", "91 %" and "1,5"; plurals read "1 carte" and "2 cartes".
8. **Desktop files:**
   - `appstreamcli validate --no-net --pedantic data/io.github.rsmirke.Rappel.metainfo.xml.in` passes.
   - Read the description and release notes.
   - Confirm the developer name.
   - Look at the placeholder icon.
9. **Anki:**
   - Import `crates/core/src/interchange/anki/fixtures/current.apkg` and read the report.
   - Import the same file again: nothing is added.
   - **Then import an export of your own** and note anything left out or converted badly.
10. **Imports with many files:** ADR 0063 measured 10,000 media files at 2.9 s, against 68 s before. No test pins the time. If you have a large media export, import it and see.
11. **Keyboard only and Orca:**
    - The new controls have names: the audio button, the Tags row in Card Information, the Details alert, and the import report.
    - The progress toasts' Cancel can be reached.
12. **Answer QUESTIONS.md Phase 2 question 1** (glycin advisories).
13. **Merge the draft PR `phase-2` → `main`** when you are satisfied. Tag `v0.3.0-beta.1` yourself if you want a release; I did not tag. The CHANGELOG's compare links point at that tag, so they give a 404 until it exists.

## Decisions, with reasons
The full list is in each PR's **Decisions** section and its ADR. The ones that change behaviour you might not expect:
- **#90:** Rappel uses its own zip reader instead of the `zip` crate, because the review measured unbounded memory and CPU in the crate on hostile archives.
- **#94:** dated trash folders instead of a table (no migration). The purge never deletes a file that any backup or the waiting restore copy uses.
- **#95:**
  - touch sizes follow libadwaita's own stylesheet: 34 px buttons, and at least 44 px for primary targets (the grade buttons are 48);
  - "+N" tag pills in list rows stay small, and the full names are one tap away in Card Information;
  - no `AdwBottomSheet`, since dialogs already become bottom sheets on narrow windows.
- **#96:**
  - `gettext-rs` with the system libintl, and no `setlocale` of Rappel's own (it would be `unsafe`; `gtk_init` sets the locale);
  - `.mo` files are built by Rappel's own small writer, because no GNU gettext tools were available;
  - number separators and the percent pattern are translatable strings.
- **#97:**
  - pre-releases are written `0.3.0~beta.1` in the metainfo, because `appstreamcli` sorts `-beta.1` after the release;
  - release notes are not translated;
  - store screenshots are committed under `data/screenshots/`, the only screenshots in git, because the metainfo links to them.
- **#98:**
  - `ruzstd` (pure Rust) rather than the C zstd binding, and a hand-written HTML tokenizer rather than `html5ever`;
  - `<div>` becomes a line break, not a paragraph, because Anki writes every line as a div;
  - only http, https and mailto links are kept;
  - a deck name that already exists gets the cards merged into it;
  - `.colpkg` is accepted too.
- **#99:**
  - the allow-list works per plugin, and a user's own whitelist variable is replaced, never widened;
  - colour profiles are stripped rather than size-capped;
  - a slow time bound is measured up to 3 times before it fails.
