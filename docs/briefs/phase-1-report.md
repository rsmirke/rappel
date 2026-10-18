# Phase 1 finish: final report

This report covers batch mode's run through `docs/briefs/phase-1-finish.md`, written for the maintainer. Every slice was planned here and implemented by a fresh sub-agent. Each was checked with `scripts/check.sh` (run under Xvfb) and reviewed by the `reviewer` agent until it had no blocking findings. Each then went through a PR into `phase-1`, waited for a green CI `check`, and was squash-merged. Nothing was merged into, pushed to or aimed at `main` except this report's draft PR.

Version: **0.2.0-beta.1** in `Cargo.toml`, `Cargo.lock` and `CHANGELOG.md`. Not tagged.

## What changed, per slice

| Slice | PRs | In short |
|---|---|---|
| 0. Prerequisite | #71 | #70 (Keyboard Shortcuts, About, remembered window) cherry-picked unchanged, because the brief restyles those dialogs. |
| 1. UI overhaul | #72–#78 | Shell and navigation, deck list, card rows and tag pills, card editor, All Cards, review page, dialogs, Statistics and the startup pages. |
| 2. Clean-up and refactor #2 | #79, #80 | Every UI file is a build input and every layout is built in a test; shared page, row, breakpoint and text helpers. |
| 3a. Flaky timing tests | #81 | Time bounds measure CPU time, adjusted for how loaded the machine is. Closes #67. |
| 3b. Hardening round 2 | #82, #83 | Attack tests on everything since round 1 (ADR 0050), in core and in the app. Closes #50. |
| 4. Remaining Phase 1 issues | #84 | Add Tags… with suggestions (closes #65); the review card is named for screen readers (refs #48). |
| 5. Beta preparation | this PR | Version 0.2.0-beta.1, this report and the open questions. |

### 1. UI overhaul
- **Shell (#72, ADR 0040).** Pages stay in one navigation view: a split view was prototyped and dropped (reasons under Decisions). On windows narrower than 400 sp, Study moves from the header to a pill in a bottom bar. The window stack is an `AdwViewStack`. The window fits at 360 × 294.
- **Deck list (#73, ADR 0041).**
  - Each row reads like "25 due · 20 new", with the due count in the accent colour, or says when the next card is due, or "Done for today". Texts refresh on show, on window activation and on a timer to the next change.
  - `list_decks` got faster: 28 ms instead of 42 ms at 5,000 cards.
- **Card rows and tag pills (#74, ADR 0042).**
  - Two-line rows: the front, then the back dimmed, with up to 3 tag pills and "+N" (1 pill on narrow windows). A pause icon marks suspended cards.
  - Opening a deck of 5,000 cards takes 182 ms to the first frame, against 255 ms before.
- **Card editor (#75, ADR 0043).**
  - Front, Back and Tags sections, with a reusable `TagEntry`: removable pills, a text field and suggestions.
  - The preview is always beside the text on wide windows. Under 720 sp, an Edit / Preview toggle sits in a bottom bar (Alt+E / Alt+P), and the "Ctrl ⏎ to save" hint shows.
- **All Cards (#76, ADR 0044).**
  - The same rows as the deck page, plus a deck and state line.
  - The filters sit on one line.
  - Typing anywhere goes into the search.
  - Selection mode follows the HIG: "N Selected", Select All, Cancel.
  - Proper empty states: No Cards Yet, and No Results Found with Clear Search / Clear Filters.
- **Review page (#77, ADR 0045).**
  - Larger card text, with the front larger than the back.
  - Big Fail / Pass pills in the same place in both phases, with key hints under them.
  - The answer fades in, and appears at once when animations are off.
  - The summary is a status page with tiles.
- **Dialogs, Statistics, startup pages (#78, ADR 0046).**
  - 21 text changes and a punctuation rule.
  - Key tables, so the shortcuts test covers every section.
  - The Statistics empty state.
  - The recovery pages, where the reviewer caught a data-loss warning that had been removed; it is back.

### 2. Clean-up and refactor #2
- **#79 (ADR 0047).**
  - `build.rs` runs `glib-compile-resources` itself and tracks every UI file. Root cause of #62: Cargo judges freshness by modification time, so a `.blp` copied with an older one was not rebuilt.
  - `ui_files.rs` builds every layout in a test.
  - The #57 undo tests are stronger.
  - All GTK test setup goes through `testing::with_widgets`.
- **#80 (ADR 0048).**
  - Shared helpers: `ui::nav::push_unless_on_top` for every push, and `card_row::factory`, `ui::breakpoint::follow`, `ui::text::{one_line, unit_count}` and `ui::time`.
  - Focus on the reopened deck page at start.
  - The window state is saved atomically with mode 0600.
  - **Bug fixed:** at 360 px, All Cards' action-bar buttons had no accessible name. A test now covers it.

### 3a. Robust timing tests (#81, ADR 0049)
- Time bounds check the CPU time of the thread doing the work. When a time is over its bound, it is divided by how much slower a fixed reference job runs right now (never by less than 1). Hangs are caught by a separate wall-clock timeout.
- No bound was raised.
- A CPU clock that does not move is not trusted: the tests fall back to the wall clock.
- Under 60 busy loops, `phase-1` had 7 to 11 failures per run; this has none in 3 runs.
- Five regressions injected on purpose all still fail their tests: a dropped index, a quadratic lookup, the same lookup 10 times slower, an uncapped loop, and a missing file-type check.

### 3b. Hardening round 2 (#82, #83, ADR 0050)
- **Core (#82).**
  - **Today's counts:** they were quadratic in a card's log. One card with 8,000 reviews took 7 s; now 17 ms.
  - **Hostile searches:** these took up to 10 s on 5,000 cards; now 63 ms. Repeated words and words that only start another query word are dropped, which gives the same results. The budget is 1 KiB of the words actually searched, so padding never hides a word.
  - **Tags:** setting many tags on one card is linear.
  - **Bad stored setting:** its error message is cut to 64 characters.
  - **#50, all three cases:**
    - a waiting restore copy comes before a stray `-wal`;
    - Finish Restore prunes nothing;
    - backups dated in the future keep the 14 latest days.

    Retention only ever keeps more.
- **App (#83).**
  - **Tag field:** a pasted list is split in linear time, and the field takes at most 100 tags per card, or the count the card had when it was opened if that is more.
  - **Row labels:** the "+N" tooltip and a row's screen-reader label name 20 tags.
  - **Bidi isolates:** right-to-left names can't reorder the text after them.
  - **Filter drop-downs:** they now shorten long names, which they never did.
  - **`window-state.ini`:** a pipe in its place hung the app at start. The file is now opened without following links or blocking, at most 4 KiB, and the size is fitted to the screen.
  - **Statistics:** overflows now saturate.
  - **Recovery dead end:** a damaged waiting copy with no backups left no way on. Start Fresh now sets the copy aside under a free name and deletes nothing. A copy is only set aside when its content is refused, never after a full disk or another passing error, and a copy from a newer Rappel shows the "too new" page.

### 4. Remaining Phase 1 issues (#84, ADR 0051)
- **Add Tags… (#65).** An `AdwAlertDialog` with the editor's `TagEntry`: suggestions, pills, the same keys. A card gets all the new tags or none if that would pass 100, and one Undo takes them back. Core: `add_tags_to_cards` and `release_tags`.
- **Crash found by the screenshot run:** the Move and Remove Tag dialogs would have crashed, because `RappelTagEntry` was not registered. Now `widgets::register_types` registers every custom widget, and a test in a fresh process fails if it is lost.
- **Screen readers (#48).**
  - The review card area was never named, because GTK prohibits names on a plain box. It is now a group, so its front, back and the cut note are read; the note only once.
  - Labels are named without the zero-width break characters.
  - Left for a real Orca run: a label's full text, used by flat review and braille, still contains those characters. That is on the checklist.

### 5. Beta preparation
- Version 0.2.0-beta.1; the CHANGELOG's Unreleased section became `[0.2.0-beta.1] — 2026-10-18`.
- `docs/briefs/QUESTIONS.md`: notes 2–5 added.
- This report.

## Open questions
See `docs/briefs/QUESTIONS.md`. No stop condition came up in any slice: no migration, no export-format change, no dependency with a new licence, and nothing that deletes user data. The questions are:
1. #70 is on `phase-1` as a cherry-pick (#71). Close #70 once `phase-1` reaches `main`.
2. A damaged stored setting stops the app at "Rappel Could Not Start" with no way on. Should Rappel offer to reset it?
3. Restoring a backup still replaces a waiting restore copy. Should it set the copy aside instead?
4. Open PRs against `main` that overlap `phase-1`: #69 (alpha.4 release) and #56 (Reset Progress for new cards).
5. Tags are never deleted, so a typo tag that was added and then undone stays in the suggestions.

## Things to know
- **Synthetic keys may have reached your desktop session once** (2026-10-17, about 15:40–15:50 local time). The first version of the screenshot harness used AT-SPI's key synthesis, which goes through the desktop's registry daemon, not only the Xvfb display. These keys may have been sent to whatever window had the focus:
  - Tab, Down, Return;
  - Ctrl+N, Ctrl+F, Ctrl+comma, F10;
  - the typed text "What is the capital of France?".

  Your session is Wayland, so only an XWayland window could have received them. The harness was then rebuilt to send input only through XTest on the Xvfb display, with a guard that refuses the real display.
- **The git pre-commit hook ran GTK tests on your real display once,** for the commit that created `QUESTIONS.md`. Every later commit ran under `xvfb-run` with Wayland unset.
- **A stash `stash@{0}: "regression probes (scratch)"` is left in the repo.** It holds the deliberate regressions used to prove the timing tests still fail. Dropping a stash is denied to the agents. The same diff is saved in the scratchpad, so `git stash drop` is safe.
- **One hard-rule slip:** an implementer ran `git checkout -- crates/core/proptest-regressions` during 3b-core. Those files had no changes at the time, so nothing was discarded.
- **New direct dependency:** `libc` in the app (#83), for the `O_NOFOLLOW` / `O_NONBLOCK` constants when reading `window-state.ini`. It was already in `Cargo.lock`, its licence is MIT OR Apache-2.0, and no `unsafe` is used. `glib-build-tools` was removed (#79).
- **`REFERENCE_CPU`,** the speed of the timing tests' reference job, was measured on this laptop only. CI passed on every PR since #81.
- **Not verifiable under Xvfb, so on the checklist:**
  - Orca speech;
  - Compose and dead-key composing;
  - how smooth the reveal animation looks;
  - GTK text selection on the problem pages;
  - real-desktop focus rings;
  - the problem pages at 360 px (they open before the saved size applies).
- **Phase 2 follow-ups noticed:**
  - hover-only names of cut pills and "+N";
  - the editor's 24 px remove buttons, under 44 px for touch;
  - the editor's preview is not scrollable by keyboard in the narrow Preview mode;
  - the Statistics empty state uses a suggested-action pill for navigation;
  - the suggestions popover has empty space under its last row;
  - hide unused tags from the suggestions.

## Screenshots
These are local files only. They are git-ignored and were never committed or uploaded. All are under `docs/briefs/screenshots/`.

### The final tour of `phase-1` (scratch data: decks "Markdown", "Math", "Scratch (300 cards)")
Each screen in light and dark, at 1000 × 720 and 360 × 640:

| Screen | Files |
|---|---|
| decks | `beta-decks-dark-1000-a.png`, `beta-decks-dark-1000-menu.png`, `beta-decks-dark-360-a.png`, `beta-decks-dark-360-menu.png`, `beta-decks-light-1000-a.png`, `beta-decks-light-1000-menu.png`, `beta-decks-light-360-a.png`, `beta-decks-light-360-menu.png` |
| deck | `beta-deck-dark-1000-a.png`, `beta-deck-dark-360-a.png`, `beta-deck-light-1000-a.png`, `beta-deck-light-360-a.png` |
| editor | `beta-editor-dark-1000-a.png`, `beta-editor-dark-360-a.png`, `beta-editor-light-1000-a.png`, `beta-editor-light-360-a.png` |
| review | `beta-review-dark-1000-back.png`, `beta-review-dark-1000-front.png`, `beta-review-dark-360-back.png`, `beta-review-dark-360-front.png`, `beta-review-light-1000-back.png`, `beta-review-light-1000-front.png`, `beta-review-light-360-back.png`, `beta-review-light-360-front.png` |
| browser | `beta-browser-dark-1000-a.png`, `beta-browser-dark-1000-search.png`, `beta-browser-dark-360-a.png`, `beta-browser-dark-360-search.png`, `beta-browser-light-1000-a.png`, `beta-browser-light-1000-search.png`, `beta-browser-light-360-a.png`, `beta-browser-light-360-search.png` |
| stats | `beta-stats-dark-1000-a.png`, `beta-stats-dark-360-a.png`, `beta-stats-light-1000-a.png`, `beta-stats-light-360-a.png` |
| prefs | `beta-prefs-dark-1000-a.png`, `beta-prefs-dark-360-a.png`, `beta-prefs-light-1000-a.png`, `beta-prefs-light-360-a.png` |
| shortcuts | `beta-shortcuts-dark-1000-a.png`, `beta-shortcuts-dark-360-a.png`, `beta-shortcuts-light-1000-a.png`, `beta-shortcuts-light-360-a.png` |
| about | `beta-about-dark-1000-a.png`, `beta-about-dark-360-a.png`, `beta-about-light-1000-a.png`, `beta-about-light-360-a.png` |
| newdeck | `beta-newdeck-dark-1000-a.png`, `beta-newdeck-dark-360-a.png`, `beta-newdeck-light-1000-a.png`, `beta-newdeck-light-360-a.png` |

### Per slice (the states the tour doesn't reach)
- **Shell (#72):** `shell-*`, `spike-split-*` (the split-view prototype).
- **Deck list (#73):** `decklist-*`, `mix-*` (all row states), `decklist-empty-*`.
- **Card rows (#74):** `cardlist-*`, `cl2-*`, `cl3*`, `cl4menu-*`, `fix-empty-*`, `fix-deck-*`.
- **Editor (#75):** `ed-new-*`, `ed-edit-*`, `ed-tags-*`, `ed-tab-*`, `ed-hc-*`, `ed-invalid-*`, `ed-long-*`.
- **All Cards (#76):** `final-browser-*`, `final-sel-*`, `final-small-*` (360 × 294), `v1-*`, `v2-*`, `v3-*`, `fix-popover-*`, `fix-menu-*`, `fix2-bs-*`.
- **Review (#77):** `rv-*`, `sum-*`, `sumshort-*`, `md-*`, `math-*`, `hc-*`, `long-*`.
- **Dialogs and startup pages (#78):** `g2-*`, `g2s-*`, `g3-*`, `g4-toonew-*`.
- **Hardening, app (#83):**
  - `h2-interrupted-*` (Finish Restore);
  - `r2-badcopy-*` (damaged copy, Start Fresh);
  - `r2-toomany-*` (the 100-tag error);
  - `r2-filter-*` (the long-name filter popup);
  - `r2-badsetting-*`.
- **Add Tags (#84):** `addtags-*`, `probe-*` (review page during the AT-SPI walk).

## Checklist for the maintainer
Scratch data for most steps:
```sh
D=$(mktemp -d)
cargo run -p rappel-core --example fill_scratch_db -- "$D" 300 --due 25 --tags --suspended 10 --markdown --math
RAPPEL_DATA_DIR="$D" cargo run -p rappel
```
Each PR's own **Checklist** has the detailed steps. This list is the order to go through them.

1. **Look at the screenshots** listed above. Tell me what looks wrong.
2. **Deck list, light and dark:** the due count is in the accent colour, and long names wrap to 2 lines and end in "…". With `ADW_DEBUG_HIGH_CONTRAST=1`, borders and pills have outlines.
3. **Narrow the window to 360 × 294:**
   - On a deck page, Study moves to a bottom pill.
   - The editor shows Edit / Preview at the bottom.
   - All Cards' three filters fit on one line.
   - The review buttons stay fully visible.
   - Nothing scrolls sideways.
4. **Keyboard only, the whole tour:**
   - Tab and Up/Down through the deck list; Enter, then Alt+← returns the focus to the row.
   - Ctrl+N, then Tab through Front, Back and Tags, then Ctrl+Enter.
   - Study with Space, 1, 2, E, I, S, Delete and Ctrl+Z.
   - Ctrl+F; typing while a row has the focus goes into the search.
   - Selection mode with Escape.
   - Every dialog with Tab, Enter and Escape.
5. **Startup:** close Rappel with a deck open and start it again. The deck reopens, and Down moves the focus from card 1 to card 2, not to Back.
6. **The review reveal:** it fades in briefly. With Reduce Animation on, it appears at once.
7. **Tags in the editor:**
   - `geo, europe` makes two pills, and Backspace removes the last one.
   - Pasting 150 names makes exactly 100 pills plus a red error, and Save is off.
8. **All Cards → Select → Tags → Add Tags…:**
   - Suggestions, Tab, Enter and Escape work as in the editor.
   - Add is off with no tag.
   - The toast says how many tags were added, and Undo takes them back.
   - **Move to Deck** and **Remove Tag…** still open: this is the crash fixed in #84.
9. **Dead keys and Compose:** with a row focused in All Cards, Compose s s gives "ß" in the search. Also try an input method if you use one.
10. **Recovery pages, in a scratch folder:**
    - An interrupted restore shows "Restore Did Not Finish" with Finish Restore.
    - A damaged copy and no backups show "Restore Copy Cannot Be Used" with Start Fresh. Afterwards, the copy is in the folder as `rappel.sqlite3.restore.refused-*`.
    - A copy from a newer Rappel shows the "too new" page.
    - Steps: PR #83, Try it.
11. **Problem pages at 360 px,** never shot: does the text or the path selection look right? Is the path not pre-selected?
12. **Orca on the deck list:** a row reads "Scratch (300 cards), 25 due, 20 new". It is not read again every minute unless its text changed.
13. **Orca on card rows:**
    - Deck page and All Cards rows read the front, the back and the tags; a row with many tags reads 20 names, then "and N more".
    - In selection mode, the action-bar buttons have names at 360 px.
14. **Orca on the review page:**
    - The card is read once, with the "only its start is shown" note once.
    - No "zero width space" is read; also check flat review (KP_7/8/9) and braille if you use it.
    - Is "grouping" annoying?
    - After Space, with the focus still on the card, is the back spoken?
    - Steps: PR #84, items 10–13.
15. **Orca in the editor and Add Tags:**
    - The field reads "Tags", and the remove buttons read "Remove tag NAME".
    - Suggestions are announced, and so is the tag error.
16. **Window state:** after a close, `ls -l "$D/window-state.ini"` shows `-rw-------`.
17. **Housekeeping:**
    - `git stash drop` the "regression probes (scratch)" stash.
    - Answer the questions in `docs/briefs/QUESTIONS.md`.
    - Decide #69, #56 and #70 before merging this PR.
18. **Release:** if all is well, mark this PR ready, merge it into `main`, and tag `v0.2.0-beta.1` yourself. Batch mode did not tag.

## Decisions, with reasons
Every Decision recorded in the PRs merged into `phase-1`, copied from their descriptions. The Try it and Checklist steps of each PR are not repeated here.

### #71 chore: bring the shortcuts and about dialogs into phase-1

- **Cherry-pick instead of waiting for #70**: batch mode never stops to ask and never touches `main`. The commit is identical, so #70 can be closed once `phase-1` reaches `main` (or merged first; the phase-1 → main PR will then only need a trivial rebase). Recorded in `docs/briefs/QUESTIONS.md`.

### #72 feat: fit the shell in a 360 px window and keep pages over a split view

- **Pages, not a split view.** In the prototype: (1) the review session sat beside a clickable deck list, not focused; keeping it full-window needs a third nested navigation container; (2) the sidebar's New Deck and the content's New Card were two "+" buttons side by side; (3) the deck rows (boxed list, row menus, counts) were cut off in a ~250 px sidebar and would need a full redesign as `.navigation-sidebar`; (4) Back/Escape went through an empty "No Deck Selected" placeholder on wide windows and behaved differently by width, doubling the focus paths; (5) learners stay in one deck, and the last deck reopens at start (ADR 0039); (6) `ui::nav`, `deck_to_reopen`, `win.new`, `win.search` all assume one view.
- **Study to a bottom bar on narrow windows** rather than dropping New Card into a menu or making Study icon-only: Study is the page's main action and stays a labelled, large target; New Card stays one tap away. Breakpoint at 400 sp (the header needs about 380 px at the default text size; sp follows text scaling).
- **Bottom-bar Study is a pill without extra margins**: a pill matches the review page's answer buttons and is a large target; the 6 px margins were dropped because the window can be 294 px tall.
- **One Alt+S at a time**: only the visible Study button has the `_Study` mnemonic. The bar's button is plain "Study" and gets the mnemonic from the breakpoint's setters, so while the bar slides away both buttons are mapped but only the header one answers Alt+S.
- **`AdwBreakpointBin` size request 360 × 294**, the window minimum (the bin needs one).
- **`AdwViewStack` instead of `GtkStack`** for the window stack (API from libadwaita 1.7, inside the current `v1_8` feature). The review page's own crossfade stacks are left for the review slice.
- Not changed here (later slices): All Cards at 360 × 294 spends most of the height on the three stacked filters; the deck list and card rows.

### #73 feat: say what each deck has, or when it next will, in the deck list

- **A custom row instead of `AdwActionRow`.** An action row's subtitle is a single label. Colouring only the due count would need Pango markup, which can only use literal colours (forbidden here). Markup would also apply to the untrusted deck name. Reusing libadwaita's row CSS nodes keeps the standard look.
- **Zero counts are hidden** ("5 new", not "0 due · 5 new"). This matches the deck page's subtitle and is calmer.
- **"Tomorrow" and day counts use study days** (the day-start hour from Preferences). That way "tomorrow" is never followed by "in 1 day". A review within 12 h still shows in hours, even if it falls after the next day start (late evening → "in 9 h").
- **"Done for today"** appears when the daily limits hold cards back and nothing comes back before the next study day. If a step card or review comes back sooner, the row names that time instead.
- **`next_change_at` is exactly `next_study_change`**, so the deck list and the deck page's timer agree. Known gap: once a deck has used up its review limit, a review due later today still shows as "Next review in 2 h", and becomes "Done for today" when that time comes. This needs 200 reviews in one deck in one day.
- **Refresh policy.** The list is read again on `shown`, on the window's `is-active`, and by a timer set to the earliest change. The timer waits at most 1 min while any row counts minutes, 10 min while one counts hours, and 1 h otherwise (the same cap as the deck page), and never less than 1 s. The window-active hook is there because GLib timers do not run during a suspend. Under another page the timer does nothing, and `shown` catches up. Rows are updated in place when the deck list is unchanged, so a refresh never takes the keyboard focus. If decks were added or removed, the list is rebuilt and the focused deck gets the focus back.
- **The deck name is bold (libadwaita's `heading` class)**, so it is clearly the row's primary element, as the brief asks ("deck name prominent"). The label does not also carry `title`: libadwaita's `row > box.header > box.title > .title { font-weight: inherit; }` is more specific than `.heading` and would undo it. No own font sizes; the row height is unchanged (≈ 67 px), and the 2-line wrap still fits at 360 px.
- **Long names wrap to 2 lines, then "…"**, with the whole name in the accessible label. This works at 360 px and leaves most names readable.
- **Row height**: the title box has 12 px above and below (rows ≈ 67 px). This is comfortable to point at and tap and is a multiple of 6.
- **Refresh failures go through an `ErrorSlot`**: the list is read again on every window activation and by the timer, so if reading keeps failing, the one toast is replaced instead of new toasts piling up. A successful read dismisses a leftover toast (new `ErrorSlot::clear`).
- **The accessible label is set only when it changes**, so a refresh does not make a screen reader announce the focused row again.
- **The "·" has `accessible-role: presentation`**: it is only a visual divider, and the row's label already says the line in words.
- **`max_new_per_day` = 0** with a deck of only new cards shows "Done for today" every day, which is accurate: the setting means no new cards.
- **Due count at full opacity**: `row label.subtitle` dims the whole line. The accent count is put back to full strength in `style.css`, so it stands out. No colours are written in the CSS.
- **`boxed-list` kept** (not `boxed-list-separate`): the decks are one list, as the HIG shows boxed lists in a clamp. Clamp 600, margins 24/12.
- **Empty state**: same icon, title and sentence. The button is now "Create Deck" as the brief asks. The header's icon button keeps the tooltip "New Deck".
- **Perf: `list_decks` counts instead of listing, and is now faster than on `phase-1`.** The first version added a count query and a next-change query per deck. That made `list_decks` 52–56 ms, against 45–47 ms on `phase-1`, and it hit the 150 ms bound under load. Now it does one pass for today's counts and one aggregate pass over the cards. `EXPLAIN QUERY PLAN` shows index searches only (`cards_deck_live`, the schedule's primary key, `review_events_card_reviewed`, `review_events_undoes`). Measured side by side on the same machine (debug build, 5,000 cards, 2,000 due, `RAPPEL_TIMINGS=1`, 3 runs each):

  | | `list_decks` | `study_input` |
  |---|---|---|
  | `phase-1` | 42–43 ms | 41–44 ms |
  | this branch | 28 ms | 27–28 ms |

  `study_input` is faster too, because its two "today" counts now share one query. The bound (150 ms) is unchanged. Two implementations now have to agree (ADR 0014). They are pinned by the fixed-time test and by a new property test, `counts_equal_study_input` (128 cases, about 2 s). Each case has 2 decks and 12 cards, random new and review limits from 0 to 5, and up to 60 random writes spread over days: pass, fail, suspend, unsuspend, undo the latest entry, reset, delete. It then checks 1 to 5 random (now, day start) pairs. In each, the new, review and step counts must match `study_input` separately, `next_change_at` must equal `next_study_change`, the live and suspended counts must match `list_cards`, and the held-back count must match the uncut totals counted card by card. Under a load average of 10, one gate run out of four failed the *other* assertion in that test (`study_input + next_study_change`, 166 + 16 ms, code that is unchanged apart from the shared today counts). The next three runs passed. That is the known flaky-timing issue planned for hardening round 2, not something this slice made slower.

### #74 feat: two-line card rows with tag pills on the deck page

- **The subtitle is the back's excerpt, not the due info.** Someone scanning a deck is checking its content (is this right, is it a duplicate); seeing each question with its answer is what a flashcard list is for. The page subtitle already says what is due for the deck, and All Cards and Card Information show it per card. A per-card due time would also go stale while the page is open, so rows would have to be re-bound on a timer. Rows stay at two lines of text.
- **Tags go on the second line, after the back**, right-aligned. The front keeps the whole first line, the pills keep their width, and the back is cut with "…" first.
- **3 pills, or 1 on a narrow window, then "+N"**. Pills are cut at 16 characters (the full name is then the tooltip). This keeps rows cheap and leaves room for the back at 360 px.
- **Suspended: `media-playback-pause-symbolic`** (in the Adwaita theme; pausing is what suspension does to a card), dimmed. The row itself is not dimmed: its second line is already at libadwaita's dim opacity, and dimming it again would leave the back below readable contrast.
- **The front stays at normal weight**, not `heading` like the deck names: bold on hundreds of rows would be heavy, and the deck name is the page title.
- **3 px between the two lines** is libadwaita's own spacing between a row's title and subtitle (`row > box.header > box.title { border-spacing: 3px }`), applied by its CSS node names. Every value this PR sets itself is a multiple of 6.
- **No `rich-list` on the list view**: its row padding would stack on the header's margins.
- **Pill style**: `alpha(currentColor, 0.1)` fill, fully rounded, 18 px high. Under `@media (prefers-contrast: more)` it adds a 1 px `var(--border-color)` outline, which libadwaita raises to 50 % opacity in high contrast. There are no literal colours and no accent.
- **The ⋮ menu is built on first use** (`gtk_menu_button_set_create_popup_func`). The popover was more than half the cost of making a row. The button stays sensitive, so opening by click, Enter or Space works as before. GTK 4.24 calls the function on every open, so the model is set only once (`menu_model().is_none()`). A test opens the menu twice and gets the same popover, and it fails without that check.
- **The speed test checks work, not the clock** (#67). After a warm-up bind, binding a row to 4,000 of 5,000 cards makes no new pill labels and no popover, and only the bound cards get previews. Its only time bound is on the thread's CPU time (`/proc/thread-self/stat`): 2 s, against about 100 ms idle and 380 ms with three busy threads per CPU. Wall-clock times are printed with `RAPPEL_TIMINGS=1`.
- **Tooltip-only names are a follow-up**: a cut pill name and the tags behind "+N" are visible in full only on hover. Screen readers get all of them from the row label. Phase 2's no-hover-only rule will need another way (ADR 0042, Consequences).
- **`testing::with_widgets`** registers the resources once per test run. The 14 other tests that register them by hand are left for the clean-up slice.
- **The empty state was already right** (icon, "No Cards Yet", one sentence, `.pill .suggested-action` **Add Card**). One change: on a narrow window the bottom Study bar now hides while the deck is empty (`study_bar_shown(compact, empty)`, unit-tested), so the status page and its Add Card stand alone. The bar comes back as soon as the first card is added.
- **On a wide window the header's Study stays, disabled, for an empty deck.** Hiding it would change the header's shape when the first card is added, and a disabled button there does not compete with the empty state.
- **The "Nothing to study" subtitle is kept** for a deck with nothing to do now. The deck list's "Next review in 3 h" texts would need a deck summary on this page; I left that for a later slice.
- No new dependency, no migration, no export-format change. The libadwaita feature stays `v1_8` (`AdwWrapBox` is 1.7).

### #75 feat: tag pills with an entry and an edit / preview toggle in the card editor

- **SPEC §4.4 changes**: the tags field is now pills with an entry after them, and the preview is beside the text when wide and an Edit / Preview toggle when narrow (no longer "below or beside, toggle"); see ADR 0043, which also amends ADR 0022 and ADR 0031.
- **No preview toggle when wide.** The old toggle existed because the preview below the text took its height; beside it costs nothing the text needs, it is debounced and capped at 32 KiB, and dropping it leaves the header at Cancel / title / Save.
- **Narrow toggle in a bottom bar, not the header.** The HIG puts view switchers in a bottom bar on narrow windows; it is in thumb reach and the header keeps its title at 360 px. Alt+E / Alt+P work from the keyboard. The focus never stays where keys go nowhere (`layout::focus_after`, read before the panes change): text → Preview puts it on the toggle; growing wide with the focus on the (hidden) toggle, or a lost focus, puts it in the front. Ctrl+Shift+Enter returns to Edit.
- **Backspace focuses the last pill first, then removes it.** A key that does nothing in an empty field should not delete without showing what it will delete; the second press removes, and repeated presses walk back through the pills. A click, Enter, Space or Delete on a pill moves the focus to the pill that took its place (as `ui::focus::index_after_removal`); with none left, to the text.
- **Tab order: text first, then the pills' remove buttons**, through the widget's `focus` vfunc, though the text is drawn after the pills: adding a tag is the field's main use.
- **The typed name counts as a tag** (a save includes it), and becomes a pill when the focus leaves the field, so the field shows what a save writes. The focus controller is on the whole field (text → pill → elsewhere is one leave); the decision runs in an idle after `leave` and is the pure `keys::should_split` (window still active, focus outside the field): `GtkEventControllerFocus` also reports `leave` on window deactivation (`GTK_CROSSING_ACTIVE`, gtkeventcontrollerfocus.c, GTK 4.20), and the text's context menu and emoji chooser are popovers parented to it, so Right-click → Paste still goes into the typed name.
- **A comma is acted on just after the text changes** (`G_PRIORITY_HIGH_IDLE`, before GDK's redraw), so pasting a comma list and on-screen keyboards work like typing, and the comma is never drawn.
- **Spaces before a name** (after a comma, or typed into the empty text) are dropped as they are typed.
- **A name that cannot be a tag** (over 256 bytes) stays in the text with "This tag is too long (the limit is 256 bytes)"; nothing typed is lost. Duplicates (by `name_key`) are dropped; the pill keeps its spelling.
- **Pills and text in one frame with the text-view background**, matching the Front and Back fields; a bare `GtkText`, whose placeholder is dimmed by our CSS as libadwaita dims an entry's. The popover is parented to the frame so it lines up under the whole field.
- **Remove button**: flat, circular, 24 px, `window-close-symbolic`, tooltip "Remove Tag", accessible label "Remove tag NAME".
- **Save hint uses `AdwShortcutLabel`** (libadwaita 1.8 keycaps) inside `.dimmed`, with `.caption`; it scrolls with the fields so it never crowds 360 px.
- No new dependency; the Cargo feature stays `v1_8`.

### #76 feat: a calmer all cards page with shared rows, compact filters and proper empty states

1. **Search field as the header's title widget, not a `GtkSearchBar`.** A search bar is for occasional search shown on demand. This page exists to search, so its field is always visible and has the focus when the page opens. It also costs no height at 294 px.
2. **Typing in the list searches, with our own key controller rather than GTK's key capture.** I read gtksearchentry.c (`capture_widget_key_handled`, GTK 4.20). GTK's capture also forwards keys from popovers under the capture widget, never checks that the entry is shown, and leaves the focus behind. Ours follows GTK's key rules (no Space, Enter or Menu, no Ctrl or Alt, no movement keys). It also passes on dead keys and Compose, so the field's input method composes the next key, and Backspace while the field has text. It skips popovers and selection mode, and moves the focus into the field. Space on the list is decided by the pure `space_action`: it chooses a focused row while choosing and otherwise passes on. GTK's capture ignores Space anyway, so the earlier reason for swallowing it was wrong.
3. **Ctrl+F does nothing in selection mode.** The field is hidden then, and a shortcut should not silently drop a choice of cards. Cancel or Escape comes first.
4. **Filters always on one line**, not a filter button with a popover (ADR 0036: a hidden filter silently narrows the list) and not an `AdwToggleGroup` (five state values do not fit 360 px as toggles). Shrinking drop-downs with "…" keep them on one 46 px line.
5. **The count hides under 500 sp**, so the filters get the full width. Screen readers still hear it after each search.
6. **One shared row widget** (`CardRow`) for both lists, not a parallel copy. The action names are the same in `deck.*` and `browser.*`, so only the group differs. The deck page's rows never show the check button or the third line.
7. **Narrow rows keep the back** (the old compact rows dropped it) and show one pill, as on the deck page. A row looks the same at every width.
8. **The suspended status shows twice**: the pause icon, as on the deck page, and "Suspended" in the status line. Each list stays consistent with itself. The spoken label says it only once.
9. **"Go to Decks"** is the "No Cards Yet" action: decks are made and cards added on the deck list, which has its own empty state with Create Deck.
10. **No check box on hover in normal mode** (HIG): a click opens the editor, the Select button, Ctrl+A and a long press start choosing, and a hover-only control fails the phone rule.
11. **Bulk "Add Tag…" dialog unchanged**, as the brief asked (a later slice).
12. Row height is about 88 px for three lines. The same 12 px margins as the deck rows are kept for consistency.

### #77 feat: larger card text, big answer buttons with key hints and a gentle reveal

- **Sizes 125 % / 112 %**: judged on screenshots; clearly larger than the chrome, with a visible but small step between question and answer.
- **Editor preview stays at body size**: it is a check of structure beside body-size source in a dialog; larger text would take room the editor needs.
- **Cut note stays caption size**: it is app text, not card text.
- **Fail is a plain pill**, not destructive (HIG; ADR 0014).
- **Hints always shown** (except on short windows): GTK cannot tell reliably that no keyboard exists; they are small and dimmed. Tooltips removed from the three buttons, since the key is on screen.
- **Reveal animation 180 ms fade + 12 px slide**, on the card area (always mapped while a card is shown); redraws after an edit do not animate; the next card skips a running animation.
- **The slide stays** (reviewer asked about relayout per frame): measured on a 32 KiB paragraph, 200 inline formulas and a 2,000-item list, the margin slide costs the same CPU as a fade alone (0–10 ms, 180–200 ms, 0–10 ms over the whole animation, debug build), since GTK re-measures only the back's box, not the labels and formulas inside. The 200-formula cost is drawing, which a fade needs too. Recorded in ADR 0045; bounded by a test.
- **Card ↔ summary and front ↔ front: no animation.**
- **Short summary**: `.compact` alone (96 px icon) still pushed Back to Deck off a 360 × 294 window, so the icon is dropped there too; `compact` and the icon are toggled in code on the breakpoint's `apply` / `unapply`, since setters cannot add classes or unset the icon.
- **Summary tiles like Statistics** instead of one line of text, for consistency; tile captions in sentence case like Statistics.

### #78 feat: polish the dialogs, statistics and the startup pages

- **One punctuation rule** (ADR 0046): alert bodies end with a full stop. One-sentence subtitles, group descriptions, status page descriptions and toasts do not. Two sentences keep both stops. *Why:* the app mixed these. The earlier slices' status pages and subtitles had no full stop, so that became the rule, and alert bodies are prose.
- **Card Information keeps `property` rows** (title above the value, as in Files' Properties) and adds `numeric`. *Why:* the long values ("09/09/26 15:50 · overdue by 38 days") would wrap beside their titles at 360 px.
- **Statistics' empty-state button goes back** instead of starting a study session. *Why:* the brief asks for one pill button, but a Study button would need a session built from the statistics page, and "all decks" has no study action. Back to where it came from is always correct.
- **Statistics empty icon `document-open-recent-symbolic`** (a clock). *Why:* `view-continuous-symbolic` looks like a layout control. Adwaita has no chart icon.
- **Destructive style stays on Delete, Discard, Restore and Start Fresh**, with Cancel / Keep Editing as the default and close response. *Why:* each replaces or removes what the user sees, even where the old file is kept.
- **Restore heading without the date.** *Why:* "Restore Backup From 16/10/26 11:00:00?" was long and wrapped on narrow windows. The date now goes in the body.
- **The selected path on the problem pages**: the first, automatic focus no longer selects the label's text. Tab later still does, as GTK does. *Why:* the label is the page's only focusable widget, so the page opened with its path highlighted (seen in screenshots).
- **The warning against copying files by hand stays**, now with its reason. *Why:* it is the only on-screen mention of a real data-loss risk (an old `-wal` replayed over a copied database, ADR 0007, 0016), and it shows exactly when the user may be tempted. Data safety wins over tone.
- **Card Information's front heading is at most three lines**, ending with "…". *Why:* a formula's source can fill the dialog.
- **About is unchanged.** *Why:* `AdwAboutDialog` already follows the HIG and the texts are right.
- **Open Backup Folder** was already an `AdwButtonRow`, so it was kept.
- **Typing to search on All Cards is not listed in Keyboard Shortcuts.** *Why:* it has no accelerator, and an `AdwShortcutsItem` needs one.
- **Mnemonics are not test-checked** (they live in Blueprint labels). *Why:* Alt+E / Alt+P are listed by hand, since the editor's toggle has no other key.
- No new dependency, no migration, no export-format change.

### #79 refactor: track every ui file as a build input and build every layout in a test

- **build.rs tracking, with no `include_bytes!` next to `#[template]`.** An `include_bytes!` would add nothing. The template is loaded from the gresource at run time, and the crate already recompiles whenever the gresource changes, because `resources_register_include!` is an `include_bytes!` that rustc tracks. rustc also tracks included files by mtime, so it would not close the backdated-file hole either. Nothing on stable Cargo can (`-Zchecksum-freshness` is nightly only). The safety net is the `ui_files.rs` test, which fails the gate on any mismatch. README now says to run `cargo clean -p rappel` if the app ever stops with "Unable to retrieve child object …".
- **Drop `glib-build-tools`.** It printed `rerun-if-changed` for every dependency of the gresource, including our own `OUT_DIR` outputs, and that cannot be filtered. Running `glib-compile-resources` directly takes about 30 lines and removes a dependency. `glib-compile-resources` was already a listed build requirement (README).
- **A template mismatch aborts the test binary.** It does not fail as a single test. gtk4-rs checks template children inside GObject's instance init, a C callback that a panic cannot unwind through. I checked this by renaming `card_menu_button` in `review_page.blp` (the #62 case). `cargo test` failed with SIGABRT. The output had `Gtk-CRITICAL: Unable to retrieve child object 'card_menu_button' from class template for type 'RappelReviewPage'`, and the backtrace named `ui_files::template::<ReviewPage>`. Avoiding the abort would mean parsing the templates ourselves. A renamed builder id (`name_entry` in `deck_name_dialog.blp`) failed normally: "deck_name_dialog: the .blp file has no object `name_entry` of the type the code expects". A new `zz_new.blp` failed `every_blueprint_file_has_a_check`.
- **Alert constants live in `ui::dialog`, not on the pages.** `pages::*` modules are private to `pages`, so a crate-level test cannot reach constants there. `ui::dialog` is the shared home for dialog loading anyway.
- **Loaders return `Result<_, &'static str>` with the first missing id.** Every lookup runs first, so all missing ones are still logged as before. The app turns the error into the same skip as before (`.ok()?` / `let Ok(..) else`).
- **New `crates/core/proptest-regressions/scheduling/session/tests.txt`.** proptest wrote it when I checked the strengthened property against a broken `undo_last` (an undone card appended to the end of the queue). The seed is the shrunk case: one answer, then its undo. It is committed, as proptest recommends and as the other `proptest-regressions` files in the repo are, so that case runs on every test run before the random ones.
- **The Undo-button check needs a window.** Seen with GTK 4.24: a button bound to `review.undo` on a page without a root stays insensitive whatever the action says. In a window it follows the action at once. The app always has the page in a window. `can_undo` asserts that the page has a root, so a future test cannot silently check the wrong thing. Mutation check: forcing `review.undo` always enabled fails all three undo tests. Before, they checked only the session and would have passed.
- **The full-session property is stronger:** changing `undo_last` to append the card to the end of the queue fails it. The old summary-and-count comparison would have passed.

### #80 refactor: share the page, row and breakpoint helpers across the ui

- **Source versions**: comments name the versions whose source was read (libadwaita 1.8 and 1.9, GTK 4.20, 4.22 or 4.24) and what was observed (the local build: libadwaita 1.10.0, GTK 4.24.1).
- **The helper takes the view and a `build` closure returning `Result`**, generic over the page type. Callers keep their own error toasts. The deck list's first-frame log still gets the view. `build` runs only when the page will be pushed, so a held key reads nothing from the database. Note: `is::<P>()` also matches subclasses, which none of our page types have.
- **`win.search` also uses the helper**, although it already handles All Cards being on top (it focuses the search field). The reason is so that "every push goes through one place" holds without exceptions.
- **The deck page's `shown` focus applies on every show, but it acts only when the focus is outside the page.** On a normal push or a pop, libadwaita has already put the focus inside the page, so nothing changes there. This avoids a startup-only flag.
- **The window state file is always 0600.** `g_file_set_contents_full` keeps the mode of a file that already exists (GLib docs, `fchmod` from the old stat in `gfileutils.c`), so after a successful write a wider mode is narrowed with `set_permissions`, as before. New files are created 0600 with no wider moment.
- **The Suspend button's tooltip is now its word** ("Suspend" / "Unsuspend"), like Move to Deck, Tags, Reset Progress and Delete. Its accessible label stays "Suspend selected cards". `suspend_labels` keeps its values; only how they are used changed.
- **ADR 0019's `dim-label`**: ADR 0001 says accepted ADRs are not rewritten, only their header lines. So 0019 gets `Amended by: ADR 0046 (dim-label is now dimmed)`, and 0046 (which records the rename) gets the matching `Amends:` entry. The body is untouched.
- **Left deliberately as they are**:
  - the pages' one-line `error_toast` wrappers (each names its own operation);
  - the deck page's and All Cards' `position` scans (different item types, one line each);
  - `DecksPage::is_on_top` (one caller);
  - the deck name dialog's error class toggle (its branches do more than toggle);
  - `ErrorSlot` uses and the drop-down helpers (already shared);
  - the key tables (named, and checked by the shortcuts test);
  - the review summary's `compact` class (it stays in code: Blueprint setters cannot add a style class).
- **Dead code**: `rappel` is a binary crate whose modules are private, so rustc's `dead_code` lint already reports unused `pub` items there. That covers items used only by tests too, because the gate also builds the non-test target. No new tool was needed. Nothing was unused apart from the duplicates removed above.
- **No new dependency, no migration, no export-format change.**

### #81 test: keep time bounds robust under load

- **No new dependency, no `unsafe`.** `clock_gettime(CLOCK_THREAD_CPUTIME_ID)` would need `libc` or `rustix` (and `libc` needs `unsafe`), so the CPU time is read from `/proc`. `schedstat` is precise to one scheduler tick (1 ms here). `stat` is the fallback (10 ms ticks), then the wall clock.
- **A reference workload, not higher bounds.** CPU time alone still failed one run in three, and raising the bounds by the 7× seen under load would let a 7× regression through. Dividing by the measured slowdown kept the dropped-index time at 3.8 s both loaded and unloaded.
- **Under load a bound is less strict.** In a run measured at slowdown s, a regression of up to about 10·s times passes. An unloaded run, or CI on an idle runner, still catches it (ADR 0049, Consequences).
- **The slowdown is never below 1.** A machine faster than the maintainer's laptop is held to the bounds as written, so no bound became stricter. A slower CI machine is measured at its own speed.
- **Measured only when a time is over its bound**, on the thread that did the work. The unloaded gate takes no longer than before.
- **The bounds are unchanged**, so ADR 0025's numbers keep their meaning. `REFERENCE_CPU` (17 ms debug, 21 ms release) was measured on this laptop. ADR 0049 says how to measure it again.
- **Core and app helpers are duplicated** because `rappel-core`'s test helpers are not exported. This was already the existing pattern for `within` and `SMALL_STACK`.
- Waits for GTK or timers (`settle_until`, the debounce tests) stay on the wall clock. They only stop a hang, and they passed every loaded run.

### #82 fix: harden search, tags, daily counts and backup retention (round 2, core)

- **Leave out implied query words instead of capping the number of words.** It is exact: FTS5's tokenizer reads a quoted string one character at a time, so a longer word's phrase holds the shorter one's tokens and finds a subset of its cards. A cap would have found more or fewer cards than the text says.
- **Cut the query at 1 KiB of words.** The cost of a phrase is its number of tokens times the cards that have them, so only a cap on the query bounds it. The budget is counted over the folded words kept, so padding with spaces or punctuation never hides a word. 1 KiB is far beyond any real search. The cut falls between two characters, so a cut word is searched as a prefix and a cut query finds at least what the whole one would.
- **Accepted: a long phrase against huge, repetitive imported cards.** Within 1 KiB, a 200-token phrase against 20 cards with 1 MiB sides that repeat that token took 12 s. That needs cards built for it and the user pasting a matching phrase.
- **Tag names: no new folding.** `name_key` stays NFC plus lower case. ß/SS, ligatures, full-width letters, Greek/Latin look-alikes, zero-width spaces, bidi controls and newlines inside a name stay distinct and valid, as for deck names (ADR 0025 row 4). Folding them would merge names a user may mean apart, and refusing them would refuse files that import today.
- **Finish Restore prunes nothing** rather than recording the source backup in a new marker file. It is simpler and can only ever keep more.
- **Future-dated retention: 14 future days**, mirroring the past window. That is at most 14 more files, and every file the old rule kept is still kept.
- **Accepted: serde reads a JSON array as a struct** (`"settings": [0.9]`). Every value is still range-checked, and no file Rappel writes has one. Changing it would be a format change.
- Two existing tests change their expectations because the behaviour was the bug: `failed_install_keeps_the_copy…` and `finishing_refuses_a_damaged_copy…` (they expected `OrphanedSidecar` with a copy waiting), and `prune_keeps_one_future_backup…` (renamed `prune_keeps_fourteen_future_days…`) and `pre_migration_backup_survives_future_dated_backups` (they expected one future backup kept). The property test's size bound grows by 13 for the new future days. No time bound was raised.

### #83 fix: harden the tag field, window state, statistics and recovery (round 2, app)

- **At most 100 tags per card from the tags field.** That is far above real use, and the pills still fit on one editor screen. Past the limit, the names stay in the text with an inline error (GNOME HIG: say what is wrong, where), so nothing is dropped silently. Core and the import keep no limit. A card imported with more tags keeps them and saves unchanged; the field only refuses to add more.
- **Lists of names in tooltips and accessible labels stop at 20**, then "and N more". Twenty covers every real card. Screen readers still hear each tag of normal cards.
- **Bidi isolates (U+2068/U+2069) only in visible text that continues after a name.** Pills and titles are their own paragraphs and need none. Screen readers get the names unchanged.
- **Popup entries wrap past 30 characters.** Thirty characters plus the popup's padding fit a 360 px window. Shorter names stay on one line.
- **Start Fresh renames an unusable waiting copy, but only when there is no backup.** With a backup, the backups are listed and the copy is left where it is. The copy is checked again in core, so a copy that can still be installed is never set aside. Doing this needed a small core change: `start_fresh` and the new `Restored::set_aside`. No migration, no export change.
- **window-state.ini over 4 KiB is ignored.** The file Rappel writes is about 100 bytes.
- **The search field is not cut by the app** (ADR 0050 row 15). GTK itself keeps at most 65,535 bytes of a paste. That is accepted: losing only the padding that a 64 KiB paste could hide is not a real search.
- **Accepted:** a card imported with 5,000 tags builds 5,000 pills when it is opened in the editor (194 ms to make them, 380 ms to lay them out, debug build).
- **Accepted, needs your decision:** a damaged settings value (row 37) stops the app at "Rappel Could Not Start / The decks could not be read". The error is shown in part, and nothing crashes, but the page offers no way on. Resetting the value would overwrite what the file holds, so it was not done here.
- **Noted:** restoring a backup replaces a waiting copy, as before (ADR 0007).
- **`libc` is a direct dependency of the app now**, for `O_NOFOLLOW` and `O_NONBLOCK` when opening `window-state.ini`. Opening with these flags and checking the open handle closes the gap between a check and the open (a FIFO swapped in would block `open`). It is already in `Cargo.lock` through GLib's bindings, under an allowed licence (MIT/Apache-2.0), and only its constants are used: no `unsafe`.
- **The tag limit is per card:** 100, or the number of tags the card had when the editor opened, whichever is higher. So a card imported with 150 tags can have 30 removed and 30 added. A suggestion is refused at the limit too.
- **Only a refusal of the copy's content sets it aside** (an integrity failure, not Rappel's, WAL mode (now its own error, `WalModeCopy`), a negative version, SQLite NotADatabase or Corrupt). A full disk or an IO error keeps Finish Restore and changes nothing, and a copy from a newer Rappel shows the "too new" page. The copy is moved by a hard link to a free name, then its old name is removed. It falls back to a rename only where the file system refuses hard links (`Unsupported` or `PermissionDenied`); every other error is returned.

### #84 feat: add several tags at once with suggestions, and name the review card for screen readers

- **Several tags at once, as pills.** The entry already does this, and limiting it to one tag would make the same field behave differently in two places. So the dialog heading is **Add Tags** and the menu item **Add Tags…**. **Remove Tag…** stays singular, because it removes one tag.
- **`AdwAlertDialog`, not `AdwDialog`**, as ADR 0046 does for small question dialogs. The entry is its extra child, and the focus starts in its text.
- **Body text:** "Choose the tags to put on 3 cards. Press Enter or type a comma after each one." It mirrors the Move and Remove Tag bodies and the editor's Tags hint.
- **Add is disabled until there is a tag to add with no error under the field** (pure `tags_to_add`). A name you typed but did not turn into a pill still counts, as in the editor. So "type a name, click Add" works.
- **Enter:** on a typed name, Enter makes a pill (the entry handles it first, as in the editor). In the empty text, Enter is Add (`TagEntry::set_activates_default`). So Add takes two Enters after the last name. That is the cost of allowing several tags and of using the editor's keys unchanged.
- **Escape:** if suggestions are showing, Escape closes them; otherwise it cancels the dialog. This comes from the entry's capture-phase controller, unchanged.
- **Tag limit for bulk adds:** a card gets **all** the new tags it lacks, or **none** if it would end up with more than 100 tags (the editor's `MAX_TAGS_PER_CARD`). Skipped cards are counted in the toast: "2 tags added to 3 cards; 1 card left out, as a card can have at most 100 tags". All-or-nothing per card because "which of my tags did that card get?" cannot be answered in a toast. A card imported with more than 100 tags gets no more, as in the editor. The limit is passed in by the app, so the store keeps no limit of its own, as ADR 0050 says. If no card gets a tag, nothing is written, not even a new tag, and the notice says why.
- **The toast names only the tags that went on some card.** Tags every chosen card already had are not named.
- **Suggestions come from the page's tag-filter list**, which is read again on every change. Opening the dialog reads nothing from the database.
- **Remove Tag… is unchanged.** It already lists only the tags the chosen cards have, in a searchable drop-down.
- **Card area role is Group**, not Article or Region. It is a focusable part of a window, and Region requires a name.
- **The AT-SPI Text interface keeps the breaks.** Removing them would mean reimplementing `GtkAccessibleText` and mapping offsets and extents in both directions, for a format character that speech synthesizers do not speak.
- No migration, no export-format change, no new dependency.
