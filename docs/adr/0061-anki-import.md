# 61. Anki import: `.apkg` and `.colpkg`, legacy and current, converted to plain cards with their history

- Status: Accepted
- Date: 2026-10-18
- Refers to: SPEC §4.11 and §4 (phase 2, item 8); `docs/briefs/phase-2.md` (slice 9); ADR 0017, 0053, 0054 (the `.rappel` import, its streaming reader, the media store), ADR 0056 (audio checked by structure), ADR 0034 (Anki is a source, not a model)

## Context

Most people who would try Rappel have their cards in Anki. SPEC §4.11 asks for a one-way import of Anki packages: cards, tags and review history become plain Rappel cards; note types, templates and presets are not mapped; HTML becomes Markdown and is never rendered; Anki's four buttons map to two grades; clozes come in as ordinary cards; `<img>` and `[sound:]` become media files (no SVG, remote URLs stay text). A package is an untrusted zip holding an untrusted SQLite database, so it needs its own limits and checks.

## Decision

### Formats

| Package | Entries read | Collection |
|---|---|---|
| Anki 2.0 (no `meta`) | `collection.anki2`, `media` (JSON), numbered files | schema 11, JSON note types and decks |
| Anki 2.1, or a "legacy" export (`meta` version 2, or no `meta` with `collection.anki21`) | `collection.anki21`, `media` (JSON), numbered files | schema 11 |
| Current (2.1.50+, `meta` version 3) | `collection.anki21b` (zstd), `media` (zstd protobuf `MediaEntries`), numbered files (each zstd) | schema 18: `notetypes`, `fields`, `templates`, `decks` tables with protobuf configs |

`.apkg` (a deck package) and **`.colpkg`** (a whole collection) have the same layout, so both are read: a collection package is how many people move everything out of Anki, and it costs nothing more. A `meta` version above 3 is refused as "made by a newer version of Anki", with the advice to export again with "Support older Anki versions". Collections of schema 11 or 15 to 18 are read; others are refused. Import… offers Rappel exports and Anki packages together (and each alone) in the file chooser; the kind is decided by the extension, else by the content (a zip with `cards.json` or with an Anki collection), never by trust in the name alone.

### Reading the package

- **The zip** is read by the import's own reader (ADR 0053) with the same limits: 2 GiB, 100,000 entries, a 16 MiB central directory, the end record ending the file, no overlapping entries, Stored or Deflate only, CRC and size checked at each entry's end. An Anki layout is added beside the Rappel one: the names Anki writes (`meta`, the three collections, `media`, decimal numbers) each once; any other name is passed over (never read, never extracted), so a later Anki adding an entry still imports.
- **zstd** through `ruzstd` 0.9 (MIT): pure Rust, so no C decompressor sits on the path of an untrusted file; a frame may ask for a window of at most 32 MiB (the decoder's memory), only one frame is read and nothing may follow it, a frame with a content checksum must have the one of what came out (ruzstd computes it but does not compare it; the import does, at the end of each entry), and every read is counted against a cap, so a **zstd bomb stops at the cap** (tested with RLE frames claiming 1 to 8 GiB in a few KiB). `zstd` (the C library's binding) was the alternative; `ruzstd` was chosen for memory safety and because it streams with the same capped reader.
- **Protobuf** (`meta`, `MediaEntries`, a note type's kind, a template's formats) is decoded by hand (`proto.rs`, varints and length-delimited fields only, every malformed message an error), not with `prost`. The media files are numbered by their place among `MediaEntries`' field 1 only.
- **The legacy JSON** (`col.models`, `col.decks`, the JSON media list) is read with bounded visitors (`json.rs`): every object and list counted as it is read and refused past its limit (10,000 note types, 1,000 fields or templates each and 200,000 in all, 100,000 decks, 100,000 media files), every string kept only up to its limit (4 KiB for a name, 256 KiB for a template; longer is marked, not copied), anything else skipped without being kept, and the JSON read in place from SQLite's buffer, never copied. The names of decks, note types, fields and templates together may take 16 MiB, from the JSON and from the tables alike. (Read with `serde_json::Value`, 64 MiB of `{"flds":[{},{},…]}`, 64 KiB deflated, took 1.4 GB; now it is refused at the 1,001st field, with a peak of the 64 MiB value itself, `tests/anki_memory.rs`.)
- **The collection** is written, decompressed (at most 2 GiB), to a new file `<data dir>/.anki-import-<16 hex>` (mode `0600`, `create_new`, written through the handle that made it), removed when the read ends, on success or failure; a leftover of a crash is removed by the next Anki import and at every start of the app (only that exact name pattern; it is never a database name Rappel reads). A WAL header is set to the rollback journal in the copy. SQLite opens it **read-only**, with `SQLITE_DBCONFIG_DEFENSIVE`, `TRUSTED_SCHEMA` off, **triggers and views disabled** (nothing in the file's schema runs; a view where a table belongs is refused), `query_only`, no memory map, cell size checks, no extensions (rusqlite's `load_extension` is not built), lowered limits (`SQLITE_LIMIT_LENGTH` at the largest value read plus 1 MiB, 66 MiB by default; statements of 1 MiB; 200 columns), and Anki's `unicase` collation registered (its tables cannot be read without it). **Before anything is read, the integrity check included, a schema that would run code chosen by the file is refused**: a virtual table (by the `CREATE VIRTUAL TABLE` text in `sqlite_schema`, whose parsing does not connect the module, then `PRAGMA table_list`'s `virtual` and `shadow` types: a module such as FTS or R-tree parses its shadow tables in C), and a generated column in any table (`table_xinfo`'s hidden 2 or 3: its expression, say `printf('%.*c', 999999999, 'x')`, runs on read even with trusted schema off). Then `PRAGMA quick_check`. Each table read must be `table_list`'s type `table`. Values are read by the type SQLite stored, whatever the schema says; text that is not UTF-8 is read lossily; **a value is measured with `octet_length`, which reads its size from the record and never loads it** (`length(CAST(… AS BLOB))` loaded the whole value), before it is fetched: a note's fields at most 8 MiB, its tags 64 KiB, a template 256 KiB, a name 4 KiB, the legacy JSON 64 MiB. A NULL note field or tag column is a damaged note, not a long one.
- **Media** are read only if a note or template names them: each is checked like any file added to Rappel (ADR 0054, 0056): its kind from its first bytes, at most 10 MB for an image and 20 MB for audio, audio plain over its whole length, hashed; a name with `/`, `\`, a NUL, starting with `..`, or listed twice is never used. SVG, video and anything else are refused and reported, never rendered. Files no card uses are not imported (reported). References may be percent-encoded, as Anki's editor writes them.

### Mapping

| Anki | Rappel |
|---|---|
| Each card | One card. Reversed and optional reversed notes give one card per direction Anki made |
| A card's template (by its ord) | Rendered with the note's fields, then converted to Markdown |
| The answer template | The part after `<hr id=answer>` (any quoting or case) if it has one, else the whole template with `{{FrontSide}}` as nothing: Rappel shows the back under the front |
| Cloze card *n* | Front: cloze *n* as `[…]` (or `[hint]`), the other clozes as their text; back: the whole text with cloze *n* in bold, plus Back Extra. Nested clozes are rendered inside out |
| Deck `A::B::C` (U+001F in a current collection) | One deck `A › B › C` (U+203A with spaces; Rappel's decks are flat). Empty parts dropped, control characters spaces, "Default" if nothing is left; a name over 1 KiB is shortened with "…" (reported). A card in a filtered deck goes to its home deck (`odid`); filtered decks are not made, nor empty decks |
| A deck name a live Rappel deck has (by name key) | The cards go into that deck. Unlike a `.rappel` file, a package carries no ids that could conflict, and merging is what someone importing an updated deck expects |
| Tags (`a::b`, `leech`, `marked`, Unicode) | Kept as they are, each a Rappel tag (found by name key); `a::b` stays one flat tag, typed as Anki users know it. A tag Rappel cannot have (a comma, over 256 bytes) is left out and reported once per name; at most 100 tags per card (`domain::MAX_TAGS_PER_CARD`, the editor's limit too), a card past it reported |
| Suspended (queue −1) | Suspended. Buried (queue −2, −3) is normal |
| Review log: learn, review, relearn (type 0–2) with Again | A review graded Fail at the entry's time (`id`, ms), its duration from `time` (at most an hour) |
| … with Hard, Good, Easy | A review graded Pass |
| Filtered-deck entries that rescheduled (type 3 with an ease factor) | Reviews, as above |
| Filtered-deck entries that did not (type 3 with factor 0: previews, cramming) | Left out, counted |
| "Forget" (manual, type 4 or 5, with ease, interval and factor 0) after a review | A reset at its time; the reviews after it start again from new |
| Other manual entries (ease 0: "Set due date", rescheduled) | Left out, counted |
| Times outside 1970–9999, after the plan's clock, unknown buttons or types | Left out, counted |
| A card new in Anki (type 0) whose log ends with a review (forgotten some other way) | Its reviews, then a reset after the last one, so it is new in Rappel too |
| The schedule | Rebuilt from the log by the same code as every import (`storage::rebuild_cards`, the scheduler's `rebuild_state`) |
| A card's creation time | Its Anki id (ms) when in range and not after the plan's clock, else the plan's time; its edit time the note's |

**Filtered decks, verified with Anki's library** (26.9.3, a scratch collection): in a filtered deck that reschedules, new and learning cards are logged with their usual types (0); a review card answered before it is due is logged as type 3 with its ease factor (2500) and its new interval. In a filtered deck that does not reschedule (a preview), every answer is logged as type 3 with factor 0 and the card keeps its schedule. Hence the rule above: type 3 counts as a review exactly when it has a factor.

### The template renderer (`template.rs`)

It handles `{{Field}}`, `{{#Field}}…{{/Field}}`, `{{^Field}}…{{/Field}}` (a field is empty when it is only whitespace, `<br>` and `<div>`, as Anki decides), the special fields `FrontSide`, `Tags`, `Type`, `Deck`, `Subdeck`, `Card`, `CardFlag`, and the filters `cloze`, `cloze-only`, `type` (nothing on the question, the field, or the active clozes, on the answer), `hint` (shown plainly: Rappel has no "show hint" button), `text`, `furigana` (`base(reading)`), `kana`, `kanji`, `tts …` and `nc` (the field as it is). Filters chain from the field outwards. **An unknown filter shows the field as it is** and is reported with its name; an unknown field shows nothing and is reported. Unbalanced sections, nesting deeper than 32 and templates over 256 KiB make the card's template unusable (reported). Rendering stops at 4 MiB of HTML, and at a **work budget** of four times that, 16 MiB, charged with the bytes each node reads or writes (a filter's value, a copied field, at least 1 per node), so a template of 20,000 nodes over an 8 MiB field costs at most the budget, not 20,000 passes (the card is then too long). Fields are borrowed, never copied per node, and each field's emptiness and the special fields are computed once per card. A cancel is seen between any two cards.

### HTML to Markdown (`html.rs`, `markdown.rs`)

A small hand-written tokenizer and tree builder (no `html5ever`: a parser of the whole HTML standard is more than Anki's fields need, and a large dependency), with **the complete table of HTML's 2,231 named character references** (`entities.rs`, generated from the WHATWG list by `make_entities.py`), numeric references (0, surrogates and values past U+10FFFF become U+FFFD), and NUL as U+FFFD.

- `script`, `style`, `title`, `textarea`, `noscript`, `template`, `head`, `xmp` are dropped with their content; comments, doctypes and processing instructions too. Only `href`, `src`, `alt` and `start` are kept of the attributes.
- `b`/`strong` → `**…**`, `i`/`em` → `*…*`, `s`/`del` → `~~…~~`; **`u` stays plain text** (Markdown has no underline, and emphasis would say something else); `sub`, `sup`, `span`, `font` and the rest: their text.
- **`<div>` is a line, `<p>` a paragraph**: Anki's editor writes each line as a `<div>`; a paragraph per div would space every line out. One line boundary or `<br>` between text is a hard line break; two (an empty `<div><br></div>`, `<br><br>`) or a `<p>` make a new paragraph.
- Lists (`ul`, `ol` with `start`, nested), `pre` (a fenced code block, longer than any backtick run inside), `code` (a code span), `blockquote`, `h1`–`h6`, `hr`, tables (the first row is the header; cells on one line) are kept.
- Links become `[text](<url>)` only for `http`, `https` and `mailto` (Rappel shows links and never opens them); in the address `<`, `>`, `\` and `&` are escaped (`&amp;` would be read as a character reference). Any other link keeps its text only.
- `<img src="name">` becomes `![alt](media:<sha256>.<ext>)` when the package has that image; a remote or `data:` image (a source with a scheme and `//`, or `data:`, `blob:`, `javascript:`; a file name may hold a `:`) is never fetched: the address (or the alt text) stays as text, and it is reported. `[sound:name]` becomes `[name](media:<sha256>.<ext>)`. A missing or refused file shows its name.
- `\(…\)` becomes `$…$` and `\[…\]` becomes `$$…$$`, the source with references decoded and tags dropped, **if Rappel can draw it**: Rappel's own Markdown parser must read it back as exactly that formula, and `ratex-parser` (KaTeX's language, already in the workspace; used in core without any UI) must accept it, a panic counting as no. Otherwise it stays text (`\(…\)`, escaped), reported. A side whose formulas do not all come back as formulas from the parser is converted again with every formula as text.
- **Text is escaped**: every character that can start markup anywhere (`\`, `` ` ``, `*`, `_`, `[`, `]`, `<`, `>`, `$`, `~`, `|`, `&`, `#`, and `!`, which a link after it would make an image) gets a backslash, and so does a `-`, `+` or `=` or a number's `.` or `)` that starts a line; lines are trimmed, so none becomes a code block. Emphasis is written only where `CommonMark`'s flanking rules let it open and close, neighbouring emphasis of one kind and neighbouring code spans are merged, and each paragraph is parsed back: if its text differs from the text meant (a delimiter run read otherwise), it is written again without emphasis. A numbered list that does not start at 1 is set apart from the paragraph before it (it could not interrupt it). In a table cell a `|` in a code span is escaped. A property test of 10,000 random fragments (300,000 checked once) over text, emphasis, links, lists, headings, tables, code, quotes, images and sounds holds that the text Rappel shows is the text of the HTML.

### Re-import

Rappel has no field to keep Anki's note or card ids, and adding one is a migration (a stop condition, not pre-approved for this slice). **A card identical to a live card of Rappel — same deck (by name key), front and back — is not added again** and is counted as "already in Rappel". Importing the same package twice adds nothing (no backup either). A card edited in Rappel after the import comes in again; that is said in the report's count and accepted.

### Plan and apply

As for `.rappel` (ADR 0053): `Store::plan_anki_import` reads the package, converts every card and compares it with the database without writing (only the temporary collection), and returns a summary (format, new and existing decks, cards, suspended, reviews, new and present media, tags) and the report: each reason (`Skip`) with its count and up to three examples (a card's first field as text, a file, tag or filter name, at most 80 characters). `Store::apply_anki_import` refuses a changed file (its stamp, then the collection's SHA-256), backs up, then in one transaction converts again and writes decks, cards (and their search index), tags and log entries, compares a digest of every decision with the plan's (`ImportPlanStale` on a difference; a decision includes where each card goes: an existing deck's id or a new deck, so a deck renamed since the plan refuses the apply), copies the used media files into the media folder (each hashed again, synced, before the rows commit), rebuilds the schedules, brings back trashed media the cards use (ADR 0057), and commits. **Both can be cancelled** (an `AtomicBool` checked at every card, every 64 media files, and just before the commit, after the media are stored and the schedules rebuilt): nothing is written to the database (media files already copied stay in the media folder, unused, as after a failed `.rappel` import). **The plan's clock is kept in the plan** (`AnkiPlan::planned_at`) and the apply decides by it (card times, reviews after it), so a package with cards dated in the future applies; only the ids the apply issues take its own clock. A 20,000-card package is planned and applied in about 6 s of CPU in a debug build (the test's bound is 60 s, ADR 0049).

### The app

Import… reads an Anki package on a worker thread behind a toast "Reading Anki package…" with **Cancel**; the dialog "Import from Anki?" counts what is added and lists below, as plain text, what is **Left out** and what is **Changed**, with plural forms and examples in quotes (bidi-isolated); Import runs on a worker behind "Importing from Anki…" with Cancel; then "Import done" with the report behind **Details** (that toast stays until dismissed when there is a report): an alert with what was added and, below it, the report's lines left-aligned in a label of their own (`ui::toast::report_toast`; an alert's body is centred). Every string is translated, French included.

### Untrusted input (continues ADR 0050)

| Input | Defence | Test |
|---|---|---|
| zstd bomb (collection, media list, a media file) | capped reader, 32 MiB window, one frame, nothing after it | `a_zstd_bomb_stops_at_the_cap`, `a_window_larger_than_the_limit_is_refused` |
| Huge numbers of entries or media files | end record limit; media list limit | `too_many_entries_or_media_files_are_refused` |
| Broken SQLite, views, triggers | read-only defensive connection, quick check, real tables, triggers and views off | `broken_sqlite_is_refused`, `views_are_refused_and_triggers_never_run` |
| Virtual and shadow tables, generated columns | refused from the schema before anything is read | `virtual_tables_and_generated_columns_are_refused` |
| Legacy JSON declaring millions of items | bounded visitors, counts and a names budget | `legacy_json_past_its_counts_is_refused_quickly`, `names_past_their_budget_are_refused`, `tests/anki_memory.rs` |
| Large values (a 64 MiB note) | `octet_length`, `SQLITE_LIMIT_LENGTH` | `values_larger_than_the_length_limit_are_measured_not_read`, `tests/anki_memory.rs` (2 MiB peak) |
| Template work over a large field | the work budget | `template_work_is_bounded_on_a_large_field` |
| A bad zstd checksum | compared at the entry's end | `a_bad_zstd_checksum_is_refused` |
| Wrong field counts, missing notes and note types, ids of the wrong type | the card is left out (BrokenNote) | `notes_that_do_not_match_their_note_type_are_left_out` |
| Template recursion, huge templates, output blow-up | depth 32, 256 KiB, 4 MiB cap, a 16 MiB work budget | `hostile_templates_are_left_out_quickly` |
| Deep or unclosed HTML, floods of `<`, `&`, `\(`, `[sound:`, `<script>` | depth 64, linear scans (math closers cached), 512 KiB stack | `hostile_html_converts_in_bounded_time_and_stack` |
| Giant fields and tags | measured in SQL first; 1 MiB per side | `giant_fields_are_left_out` |
| Log times at the extremes, unknown buttons and kinds | left out, durations capped at an hour | `review_entries_out_of_range_are_left_out` |
| Deck names with `::` edge cases, control characters, 2,000 bytes | parts trimmed, controls spaces, shortened | `deck_names_become_rappel_names` |
| Media names with `../`, `/`, `\`, duplicates; SVG, HTML, fake MP3, empty files | never used; kind by first bytes and structure | `bad_media_names_are_never_used`, `media_of_other_kinds_are_left_out` |
| Malformed media lists | refused | `a_malformed_media_list_is_refused` |
| A temporary file left behind | removed on every path; stale ones by name pattern | `a_failed_import_leaves_no_temporary_file` |

### Fixtures

`crates/core/src/interchange/anki/fixtures/` holds four packages made by **Anki itself** (26.9.3, its Python library, headless, on a fresh collection in a scratch folder with `HOME` pointing there, so the user's own Anki data is never touched): the current format, the legacy one, a `.colpkg`, and an Anki 2.0 package **derived** from the legacy one (its `collection.anki21` stored as `collection.anki2`, no `meta`; faithful because both are schema 11 and Anki 2.0's packages have nothing else of their own). `make_fixtures.py` makes them and its README has the exact command. They cover every stock note type, a custom one, the HTML cases above, nested, default and filtered decks, the tags, the media kinds, and a log made by Anki's scheduler (learning, review, relearning, "Set due date", "Forget"), suspended, buried and new cards. The log is answered within a second and then spread over the past days by rewriting its ids. The hostile packages and the 20,000-card one are built by the tests (`tests/builder.rs`), never committed.

## Consequences

- People can bring their Anki collection, its history included, in one step; nothing of Anki's note types or add-ons comes with it, by design.
- New dependency: `ruzstd` 0.9 (MIT, with `twox-hash`, MIT, for the frame checksum). `rappel-core` also uses `ratex-parser` (already in the workspace for the app) and rusqlite's `collation` feature. No migration, no change to the `.rappel` format.
- A card edited in Rappel after an import is imported again by a second import (no Anki ids are kept). If that matters later, a migration adding an "origin" column to `cards` is the way, with its own ADR.
- Templates using features beyond the renderer's (add-on filters, JavaScript, CSS) come in as their fields' text; the report names the filters.
- `fill_scratch_db --anki PACKAGE` imports a package into scratch data, for trying it and for screenshots.
