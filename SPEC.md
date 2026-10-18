# Rappel — SPEC

A native GNOME flashcard app. Rust, GTK4, libadwaita, Blueprint. Two grades only: **Fail** and **Pass**. Cards are Markdown with LaTeX math. Scheduling is FSRS. Everything is local: no network access, no accounts.

App name: **Rappel**. App ID: `io.github.rsmirke.Rappel`. Licence: **GPL-3.0-or-later**.

- Crates: `rappel-core` (`crates/core`, library) and `rappel` (`crates/app`, GTK binary).
- Data directory: `~/.local/share/rappel/`. Debug builds use `~/.local/share/rappel-dev/` so development never touches real data. Both can be overridden with the `RAPPEL_DATA_DIR` environment variable.

v0 is released as 0.1.0: decks, plain-text cards, review with Fail/Pass, FSRS, SQLite. Phase 1 of the roadmap (§4), text only and ending in 0.2.0, is **in beta** (0.2.0-beta.1). Done so far: export / import (§4.1), Markdown rendering on the review page, math rendered with RaTeX (§4.2, ADR 0021), editing the card on screen during review (part of §4.5), the Markdown editor with live preview (§4.4), the settings with their Preferences dialog (§4.8, ADR 0027; the rest of the polish is still to come), undo and reset in the review log with their UI (ADR 0028, 0030; Ctrl+Z on the review page, Reset Progress in the card menu), tags (ADR 0029, 0031: the field in the editor and the tags on the deck page), export format version 2 (§4.1), the statistics and card information in core (§4.7, ADR 0032), and their pages (ADR 0033: Statistics from the main menu and each deck's ⋮ menu, Card Information from each card's ⋮ menu and the review header), the search index, suspension and bulk writes in core (ADR 0035), the All Cards page (ADR 0036), suspend and delete during review (ADR 0037), and the missing features: the keyboard shortcuts dialog, the About dialog and the remembered window size and deck (ADR 0039), the UI overhaul (ADR 0040–0046), the second clean-up (ADR 0047, 0048), time bounds robust under load (ADR 0049), hardening round 2 (ADR 0050) and Add Tags with suggestions (ADR 0051). Phase 2: the phase 1 questions are done (ADR 0052: Reset This Setting for a damaged setting, a restore keeps a waiting restore copy, suggestions offer only tags in use), the streaming import (ADR 0053), and the media store in core (ADR 0054: content-addressed files, `media:` references, media in the export and the import), and images (§4.3, ADR 0055: decoded by glycin in a separate process, sandboxed where the system allows, drawn in the card and opened at full size, added in the editor by Insert Image…, paste and drag and drop), and audio (§4.9, ADR 0056: played by GTK's GStreamer media stream from bytes checked as plain audio over the whole file, only on a press or R, added by Insert Audio… and drag and drop; the decoder allow-list SPEC asks for is pending), and removing unused media (ADR 0057: Clean Up Media in Preferences moves the files no card uses to a dated trash, brought back when a restore, an import or a card uses them again, and purged after 30 days unless a backup uses them). The product vision below is set by ADR 0034, the roadmap in phases (§4, §5) by ADR 0038.

---

## Product vision

Rappel is an opinionated, modern GNOME flashcard app. It is closer to Mochi than to Anki.

- **Efficiency is not maximalism.** Studying well takes few cards, few buttons and few decisions. Rappel does not try to cover every way anyone has ever used flashcards.
- **Smart behaviour is built in and invisible until needed** (progressive disclosure). The scheduler, the daily limits, the backups and the search just work. A feature shows itself when the user reaches for it, not before.
- **Every setting must justify itself.** If a good default can be found, there is no setting. A setting that needs a paragraph to explain is a design failure.
- **Anki is a source to import from, not a model.** Rappel has no note types, no add-ons, no field templates and no per-deck presets, and will not grow them. An Anki collection comes in as plain cards.
- **Card links belong to the separate method app**, not to Rappel (see §6).

---

## 1. Principles

1. **Data first.** Losing review history is the worst possible bug. Backups, export and migrations come before features.
2. **Core is UI-agnostic.** `rappel-core` holds all logic and has no GTK dependency. It must stay testable with plain `cargo test`.
3. **GNOME-native.** Follow the GNOME HIG. Use libadwaita widgets as designed. No custom colors, no web views.
4. **Few options, good defaults.** A setting needs a reason to exist (see the product vision).
5. **Untrusted content.** Card text may later come from imports. It is never interpreted as HTML or markup without escaping. Card content never causes network access or access to user files. Reading installed system fonts is allowed (ADR 0021).
6. **Adaptive from phone (360 px) to desktop, touch-friendly, ready for mobile Linux.**

---

## 2. Architecture

```
rappel/
├── Cargo.toml                 # workspace, [workspace.lints], shared deps
├── README.md  SPEC.md  CLAUDE.md  ARCHITECTURE.md  CHANGELOG.md
├── docs/adr/                  # Architecture Decision Records (NNNN-title.md)
├── deny.toml  rustfmt.toml  clippy.toml
├── LICENSE                    # GPL-3.0-or-later
├── scripts/check.sh           # the single quality gate (see CLAUDE.md)
├── scripts/pre-commit.sh  scripts/install-hooks.sh
├── .github/workflows/ci.yml   # runs scripts/check.sh (ADR 0003)
├── .claude/agents/reviewer.md
└── crates/
    ├── core/                  # rappel-core — no gtk/glib/adw
    │   └── src/
    │       ├── lib.rs
    │       ├── error.rs        # thiserror Error enum, Result alias
    │       ├── domain/         # Deck, Card, ReviewEvent, Grade, CardState, Tag, Settings
    │       ├── storage/        # Store, migrations/, backup.rs, queries
    │       ├── scheduling/     # FSRS wrapper, session queue, rebuild_state
    │       ├── content/        # Markdown -> Document (UI-agnostic block/inline tree)
    │       ├── media/          # content-addressed media store
    │       ├── stats/          # pure functions over review events
    │       └── interchange/    # JSON export / import (versioned format)
    └── app/                   # rappel — GTK binary
        ├── build.rs            # Blueprint (.blp) -> .ui -> embedded gresource
        ├── data/               # .blp files, resources.gresource.xml
        └── src/
            ├── main.rs  application.rs  config.rs
            ├── state.rs        # AppState: owns Store, exposes operations to UI
            ├── render/         # Document -> GTK widgets (labels, code, math)
            ├── window.rs       # main window (template in data/window.blp)
            ├── widgets/        # reusable widgets, each .rs + .blp
            ├── pages/          # deck, review, stats, browser
            └── dialogs/        # card editor, preferences, about, shortcuts
```

- UI talks to `AppState` only, never to SQL.
- Markdown parsing lives in core (`content`) and produces a `Document` tree. The GTK layer only turns that tree into widgets. This keeps parsing testable and reusable for a future WASM or mobile front end.
- Errors: `rappel-core` uses `thiserror`; the app turns core errors into user-facing `AdwToast`s or a startup problem page (no `anyhow`).
- Logging: `tracing` + `tracing-subscriber`, level from `RUST_LOG`. No card content in logs above `debug`.

---

## 3. Data model changes

Keep everything from v0 (migration 0001, ADR 0005). Ids are UUIDv7 stored as `TEXT`. Timestamps are UTC milliseconds. `cards` also has an internal `seq INTEGER PRIMARY KEY`, a stable integer key for FTS5 that is never exported. `review_events` is the source of truth; `card_schedule` is a cache rebuilt from it. Triggers forbid `UPDATE`/`DELETE` on `review_events`, and a card with reviews cannot be deleted (`ON DELETE RESTRICT`), so card deletion is a soft delete. Add, via numbered migrations:

```sql
ALTER TABLE cards ADD COLUMN suspended INTEGER NOT NULL DEFAULT 0;  -- excluded from study and from the schedule's statistics (ADR 0035)

tags(id TEXT PRIMARY KEY, name TEXT NOT NULL, name_key TEXT NOT NULL UNIQUE);
-- Flat, unique by name key (trimmed, NFC, lower-cased, as decks), never deleted (ADR 0029).
card_tags(card_id TEXT REFERENCES cards(id) ON DELETE CASCADE,
          tag_id  TEXT REFERENCES tags(id)  ON DELETE CASCADE,
          PRIMARY KEY(card_id, tag_id));

-- Undo and reset without breaking the append-only log (ADR 0028):
review_events.kind   TEXT NOT NULL  -- 'review', 'reset' or 'undo'; grade only on a review
review_events.undoes TEXT REFERENCES review_events(id)  -- only on an undo, unique
-- A reset makes the card new from there on. An undo cancels the card's latest
-- review or reset that is not already undone. rebuild_state skips undos and
-- the entries they cancel, and starts again at a surviving reset.

settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- desired_retention (0.80–0.97, default 0.90), max_new_per_day (0–9999, default 20),
-- max_reviews_per_day (0–9999, default 200), day_start_hour (0–23, default 4).
-- A key never written has its default. Limits are per deck (ADR 0027).
-- A stored value outside its range stops the open, naming the key; the app offers
-- "Reset This Setting", which backs up and then removes that row (ADR 0052).

media(hash TEXT PRIMARY KEY, mime TEXT NOT NULL, bytes INTEGER NOT NULL, created_at INTEGER NOT NULL);
-- One row per file of <data dir>/media/<hash>.<ext> (ADR 0054): the SHA-256 as 64 lowercase
-- hex digits, one of the MIME types of the kinds kept (image/png, image/jpeg, image/webp,
-- image/gif, audio/ogg, audio/mpeg, audio/mp4, audio/wav), bytes > 0, checked by the schema.

-- Full-text search (ADR 0035): a cache of the plain text of each side (what the user
-- reads, folded to lower case without accents), written by the store with every card write.
CREATE VIRTUAL TABLE cards_fts USING fts5(front, back, tokenize = 'unicode61 remove_diacritics 0');  -- rowid = cards.seq
```

Storage rules:
- `PRAGMA journal_mode=WAL`, `foreign_keys=ON`, `PRAGMA integrity_check` at startup (log and toast if it fails).
- **Backups:** before any migration, and once per day at startup, copy the DB with the SQLite online backup API to `<data dir>/backups/rappel-YYYYMMDD-HHMMSS.sqlite3`. Keep a backup if it is from the last 24 hours, or the newest of its UTC day for each of the last 14 days, or the newest one overall, or the one just made or just restored (ADR 0026); a backup dated in the future (a clock that ran ahead) is kept if it is the newest of its UTC day, for the 14 latest such days, and finishing an interrupted restore prunes nothing (ADR 0050).
- DB and backup files are created with mode `0600`, directories `0700`.
- A restore never replaces a copy an interrupted restore left waiting: it is kept as `rappel.sqlite3.restore.set-aside-YYYYMMDD-HHMMSS` (ADR 0052). Nothing in the data folder is ever deleted by a restore.
- **Media trash** (ADR 0057): a media file no card names (live or deleted) can be moved by Clean Up Media to `<data dir>/media-trash/<YYYY-MM-DD>[-N]/<sha256>.<ext>` (the UTC date; a name in the trash is never reused), its row deleted, in one write transaction that puts everything back on failure. A trashed file the database uses again (a row after a crash or a restore, a card side after an import, a restore or a card write) goes back to `media/`, with a row made from its name and size if needed. A dated folder's files are deleted 31 days after its date, unless a backup, the waiting restore copy, another copy of the database in the data folder or the database uses them; a backup that cannot be read keeps everything. That is the only deletion of a user's file. Adding a media file names it and commits its row under the write lock.

---

## 4. Features

The work is done in three phases, each ending in a release (ADR 0038, amending the order of ADR 0034). Within a phase, items come in the order listed. Sections keep their numbers; the phases give the priority, not the numbering.

**Phase 1 — text only, ends in v0.2.0.**
1. *Done:* the editor (§4.4), statistics (§4.7), browser and search (§4.6), suspend and delete during review (§4.5).
2. *Done:* missing features (§4.8): keyboard shortcuts dialog, About dialog with the font credits, remembering the window size and the last deck.
3. *Done:* UI overhaul (§4.8): a visual pass on every screen, and the accessibility issues.
4. *Done:* code clean-up and refactor (§4.10).
5. *Done:* hardening, round 2 (round 1 was ADR 0025; round 2 is ADR 0050).
6. v0.2.0-beta (0.2.0-beta.1 prepared, not tagged), then v0.2.0.

**Phase 2 — media and mobile, ends in v0.3.0.**
1. *Done:* streaming import (§4.1, ADR 0053).
2. *Done:* images (§4.3): the media store (ADR 0054), then drawing and adding them (ADR 0055).
3. *Done, except the decoder allow-list (pending, see §4.9):* audio (§4.9, ADR 0056).
4. *Done (ADR 0057):* removing unused media files — moved to a trash, brought back when used again, purged after 30 days unless a backup uses them (undo, restore and backups never lose a file).
5. Phone and touch adaptivity (§4.8).
6. Anki `.apkg` import (§4.11).
7. A refactor and hardening, as in phase 1 (§4.10).
8. v0.3.0.

**Phase 3 — ends in v1.0:** translation, testing campaign, packaging, release (§5).

### 4.1 Export / import
- Export everything to one `.rappel` file (older `.cards` files import too, ADR 0020): a zip archive holding exactly `cards.json`, a `media/` folder and one entry `media/<sha256>.<ext>` per media file, stored as it is (ADR 0017, 0054). `cards.json` has `"format_version": 2`, `exported_at`, and all decks, cards, tags, card tags and the full review log with its resets and undos, deleted rows included with their `deleted_at`. Each card record has `suspended` (always written; a file without it reads as `false`, ADR 0035). It also has `settings` (the keys the user changed, ADR 0027) and `media`: one record per media file (`hash`, `mime`, `bytes`, `created_at`, sorted by hash), whose file is `media/<hash>.<ext>` (ADR 0054). A media file missing or damaged at export is left out with its record, and the export says how many.
- Versions: version 1 was written only by test versions (0.2.0-alpha, and `.cards` files) and is refused with "made by an early test version of Rappel and cannot be imported" (ADR 0017). **From 1.0 on, every format version a release wrote stays importable by every later release.**
- Deterministic: the same database exported at the same time gives the same bytes (lists sorted by id, fixed zip metadata). Only `exported_at` differs between exports.
- Import into an empty or existing DB in two steps: `plan_import` checks the file and returns a summary without writing anything (new / already present / conflicts, e.g. "12 decks, 840 cards, 5,210 reviews"); `apply_import` backs up the database, then writes in one transaction and rebuilds the schedule of the affected cards from the log.
- Merge rules: an id not in the DB is added; the same id with the same content is skipped. A media file whose hash the DB has with the same type and size is present (the DB keeps its own `created_at`), with another type or size a conflict; a new one is copied into the media folder before the transaction commits. A card may name a media file that is in neither the file nor the DB. A tag whose name key a DB tag with another id has is mapped onto that tag (its card tags attach to the DB's id), not added; a tag with the same id and another name is a conflict, and so is an undo of an entry the DB has already undone. The file's settings are applied only into a DB without decks (deleted ones included); otherwise the DB keeps its own. A card that differs only in `suspended` is already present: the database keeps its own flag, since an import never changes how a live card is studied (ADR 0035). A row that differs only by being deleted is merged: deleted in the DB and live in the file, it is revived (`deleted_at` cleared; a revived deck also revives the cards deleted with it, as `undelete_deck` does, except those the file has, which follow their own row); live in the DB and deleted in the file, it is skipped (an import never deletes); deleted on both sides, at whatever times, it is already present. The same id with any other difference, or a live deck name (revived ones included) already used by another live deck, is a conflict, and any conflict refuses the whole import. The summary also counts revived rows. Importing the same file twice changes nothing.
- The file is untrusted. A file is refused if any of these fail: size limits; the exact entry names (nothing is extracted, so no zip-slip); each media record with exactly one entry of its size and each media entry with a record, whose content has the record's hash and type (read before anything is written); a supported format version; every id a lowercase `UUIDv7`; references that resolve; valid kinds, grades and timestamps; undos that cancel an earlier review or reset of the same card, once; tag names without a NUL or a comma; settings within their ranges; no NUL character in a deck name or card side.
- Round-trip test: export → import into empty DB → export again produces an identical file.
- **Streaming import** (phase 2, first; *done*, ADR 0053): `plan_import` and `apply_import` read the archive entry by entry and keep memory bounded (under 200 MiB at the limits: an archive of 2 GiB with at most 100,000 entries, 256 MiB of `cards.json`; an image at most 10 MB, an audio file 20 MB), so a file with a large media folder does not need to fit in memory. The plan keeps compact notes and a fingerprint of the file; the apply reads the file again, refuses it if it changed, and writes in one transaction. The checks and merge rules above do not change, and a refused file still writes nothing, so media are not put in the store before the plan is confirmed.

### 4.2 Math
- `$...$` inline and `$$...$$` display math in Markdown.
- Rendering: evaluate **RaTeX** first (pure-Rust, KaTeX-compatible, has `ratex-cairo` and a `ratex-gtk4` widget). Draw inline math as vectors in the text flow, on the text baseline; display math as a centered block (ADR 0021). Formula color follows the theme foreground (works in dark mode).
- Fallback if RaTeX doesn't work out: show the LaTeX source in monospace. Never a web view.
- Invalid LaTeX shows the source with an error style, never crashes.
- Record the decision in `docs/adr/`.

### 4.3 Images
- Paste or drag an image into the card editor. It is copied into the media store (`<data dir>/media/<sha256>.<ext>`, folder `0700`, files `0600`, a file never replaced by another) and referenced as `![](media:<sha256>.<ext>)`. Its type comes from its first bytes, never its name (ADR 0054).
- Only `media:` URLs render. Remote URLs and arbitrary file paths are shown as text, never fetched.
- Accept PNG, JPEG, WebP, GIF (shown still); **no SVG** (script/external-reference risk). Max 10 MB. Audio: Ogg (Opus or Vorbis), MP3, M4A, WAV, max 20 MB (ADR 0054).
- *Done (ADR 0055):* decoded by glycin in its loader process, off the main thread: under bubblewrap with seccomp and a memory limit where the system allows (flatpak-spawn in a Flatpak), else, where no namespaces can be made, as a plain separate process (logged as a warning); only the media folder's regular files, of their kind by their first bytes (which is what keeps SVG out), within 16,384 px a side and 64 MP, a 10 s timeout and the decoder's own type matching the file's kind; scaled down to the card's width and 60 % of the view's height (never up), at most 20 per side, opened at full size by a click or Enter / Space; the alt text is the accessible name; a missing or bad file is a placeholder. In the editor: an Insert Image… button per side, Ctrl+V of image files or a picture, and dropping files or a picture on a side (only local files are read, never a URL; a picture is taken as the bytes offered, never decoded in Rappel's process), inserted at the cursor (or the drop point) as one undoable step with the cursor in the alt text.

### 4.4 Editor
- `GtkSourceView` 5 with Markdown highlighting, monospace, line wrapping.
- Live preview beside the editor when the window is wide; on a narrow one an Edit / Preview toggle shows one or the other (ADR 0043). It uses the same renderer as review.
- Tags field: the tags as removable pills with an entry after them for adding; a comma or Enter turns the typed name into a pill. Suggestions from the tags at least one live card has while typing (ignoring case and accents; ADR 0052). *Done (ADR 0029, 0031, 0043). A tag name has no comma. Ctrl+Shift+Enter keeps the tags for the next card.*
- Ctrl+Enter saves, Ctrl+Shift+Enter saves and starts a new card in the same deck.

### 4.5 Review
- Undo last grade (Ctrl+Z, and an Undo button in the header) via an `undoes` event, repeatable back to the session's first grade, also from the summary; not available after leaving the page. *Done (ADR 0028, 0030).*
- Reset Progress in each card's menu on the deck page, with an Undo toast; disabled for a card that is already new, and `reset_card` refuses one. *Done (ADR 0030, #38, #56).*
- Daily limits from settings. New cards interleaved with reviews.
- Card actions during review: Edit (E), Suspend (S) and Delete (Delete key, with confirmation), the last two in the Card Menu too; each leaves the session at once and offers Undo in a toast. *Done (ADR 0037).*
- Session summary at the end: cards reviewed, pass rate, time spent.

### 4.6 Browser and search
- An "All Cards" page: full-text search (FTS5) over the plain text of both sides, ignoring case and accents, with each word matched as a prefix; filter by deck, tag and state (new / learning / due / suspended). A virtualised list, fast at 5,000 cards and more.
- Multi-select: move to deck, add or remove a tag, suspend or unsuspend, reset progress, delete; each with an Undo toast where the core can undo it. Open a card's editor and Card Information from the list.
- Suspend: a suspended card is excluded from study and from the schedule's statistics, and kept otherwise. *Done (ADR 0035 for the index, the flag, the bulk writes and `browse`; ADR 0036 for the page: Ctrl+F or the deck list's search button; Suspend also in each card's ⋮ menu on the deck page).*

### 4.7 Stats
- Per deck and global: reviews per day and time studied per day (last 30 days, bar chart), pass rate, due forecast (next 14 days), and **expected retention** today = mean FSRS retrievability over reviewed cards.
- Card information (#39): created / edited, reviews, lapses, pass rate, stability, difficulty, retrievability now, next due, and the history (surviving reviews with grade and interval, resets as markers).
- Activity (reviews, time, pass rate) counts every review that is not undone, whatever resets came later; a reset restarts only the memory side (retention, forecast, a card's reps, lapses, stability, difficulty). Undo removes a review everywhere (ADR 0028, 0032). Days follow the stored day start hour.
- Drawn with `GtkDrawingArea` + Cairo, theme colors only. No chart library.
- All computations are pure functions in `core::stats`, tested. *Done (ADR 0032, 0033). Each chart can be focused and stepped through a day at a time, and has a summary in words for screen readers.*

### 4.8 Polish
- `AdwPreferencesDialog` (done, ADR 0027): daily limits for new cards and reviews, the hour a study day starts, desired retention (asked about first, because it recalculates every due date), an "Open Backup Folder" button, and Clean Up Media with the space it frees (ADR 0057).
- Phase 1, missing features (done, ADR 0039): `AdwAboutDialog` with credits for the bundled KaTeX fonts (SIL OFL-1.1, see `deny.toml`) and the main libraries, the keyboard shortcuts dialog (`AdwShortcutsDialog`, Ctrl+?) listing every shortcut, key hints in menus, primary menu. The window size, maximised state and the deck page open at close are remembered in `window-state.ini` in the data directory.
- Phase 1, UI overhaul: a visual pass on every screen (spacing, alignment, style classes, consistent wording), checked in light and dark mode. Every icon-only button has a tooltip and an accessible label. Full keyboard navigation. Empty states (`AdwStatusPage`) everywhere a list can be empty. Fix the known accessibility issues.
- Phase 2, phone and touch: audit every screen at 360 px, touch targets of at least 44 px, no hover-only actions, grade buttons large enough for a thumb.

### 4.9 Audio
- Attach an audio file to a card side from the editor. It goes into the media store like an image (`<data dir>/media/<sha256>.<ext>`) and is referenced in Markdown as a link, `[](media:<sha256>.<ext>)`; only `media:` URLs play, nothing is fetched (§1 principle 5).
- Untrusted input: playback opens only a local file built from the hash name, never an arbitrary URI (no `playbin` on card-supplied URLs), with an allow-list of formats and decoders. The ADR adds the row to the untrusted-input table (ADR 0025) and justifies the GStreamer binding as a new dependency.
- A play button on the card, with a tooltip and an accessible label. Playback is by GStreamer, which GNOME already ships; no recording, and no autoplay at all (the user presses play). Formats and the size limit are decided in the slice's ADR.
- *Done, except the decoder allow-list (ADR 0056):* Ogg (Opus or Vorbis), MP3, M4A and WAV, at most 20 MB, played by GTK's own GStreamer media stream (`GtkMediaFile`; no binding) from the file's bytes, read only when the user presses play or R, from the media folder's regular file, never as a URI. **Formats are allow-listed by their structure** over the whole file (store, import and playback: MP3 frames, one Ogg Opus or Vorbis stream, PCM or float WAV, M4A without references, redirects or other tracks), and GStreamer's streaming demuxers and network sources are demoted so playbin never plugs them. **Decoders are not allow-listed:** GStreamer picks its demuxer and decoder itself, in Rappel's process; that needs a GStreamer binding and its development packages, pending the maintainer's decision (`docs/briefs/QUESTIONS.md`, Phase 2, 2). A control per audio link at the top level of a paragraph (the link's words, "Play audio: …" for screen readers, the time played), one playing at a time, stopped when it leaves the screen; R on the review page plays the back's audio once shown, else the front's. In the editor: Insert Audio… per side, and audio files pasted or dropped like images, inserted with the cursor in the link's words.

### 4.10 Code clean-up and refactor
- Internal work, not a user feature: nothing the user sees changes. Done once in phase 1 before hardening, and again at the end of phase 2.
- Remove dead code and duplication, simplify what grew during the feature slices, move shared UI pieces into `crates/app/src/ui/`, and make the modules match `ARCHITECTURE.md` (or update it where the code is right).
- Behaviour is pinned by the existing tests, which keep their meaning; add tests where a refactor touches logic without coverage. No migration, no change to the export format.

### 4.11 Anki import
- **Anki `.apkg` import**, one way. Cards, tags and review history become plain Rappel cards; note types, templates, add-ons and presets are dropped, not mapped (product vision). Field HTML becomes Markdown or plain text and is never rendered as HTML; Anki's four buttons map to two grades (Again = Fail, the others = Pass); cloze notes come in as ordinary cards with the text shown in full; `<img>` and `[sound:]` become `media:` files (no SVG, remote URLs stay text). An `.apkg` is an untrusted archive holding an untrusted SQLite database, so it gets its own limits and checks in the slice's ADR, as §4.1 has for `.rappel`.

---

## 5. Phase 3 and later

Phase 3, ending in v1.0, in this order:
1. **Translation** (gettext), French first.
2. **Testing campaign**: testers use the app on real collections, on desktop and phone, before 1.0; what they find is fixed before release.
3. **Packaging**: Meson build + Flatpak manifest + AppStream metainfo + app icon + `.desktop` file (needed to install it properly and to look like a GNOME core app).
4. **Release** of 1.0.

Not scheduled:
- FSRS parameter optimisation from the user's own review log.
- Sync.
- Nested decks.
- Reversed cards: a card also asked back to front. An option on a single card, not a note type or a template. It needs a schedule and review history per direction, so a migration and an ADR.
- Obsidian-style in-place live preview in the editor (formulas rendered inside the text being edited, source shown where the cursor is). Needs a spike first, with its own ADR (next free number).

## 6. Never in this project
- More than two grades.
- Note types, add-ons, field templates, per-deck presets (product vision, ADR 0034).
- Card links, proximal-zone suggestions, prerequisite graphs: those belong to the separate method app.
- Network access, telemetry, accounts.
