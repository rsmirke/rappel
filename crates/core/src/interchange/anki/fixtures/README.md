# Anki import fixtures

Packages made by Anki itself (26.9.3), for the Anki import's tests (ADR 0061).
All content is synthetic. They hold the same collection:

| File | Format |
|---|---|
| `current.apkg` | current: `meta` (version 3), `collection.anki21b` and media in zstd, a protobuf media list |
| `legacy.apkg` | legacy (Anki 2.1): `collection.anki21`, a JSON media list, plain media files |
| `anki2.apkg` | Anki 2.0, **derived** from `legacy.apkg` (see below) |
| `current.colpkg` | a collection package in the current format (also has the unused file) |

The collection covers: Basic; Basic (and reversed card); Basic (optional reversed card), with
and without the reverse field, and one whose field was emptied after its card was made (an empty
front); Basic (type in the answer); Cloze with two clozes, a hint and Back Extra; a custom note
type with `{{#F}}`, `{{^F}}`, `{{text:F}}`, an unknown filter and the special fields; HTML of
every kind the converter reads (lists, `<br>`, `<div>`, entities, `<span style>`, `<script>`,
a link, a local and a remote image, `[sound:]`, inline and display MathJax, `$` and `*` in text,
`<pre>` and code); nested decks, the Default deck and a filtered deck holding cards of another
deck; hierarchical, `leech`, `marked` and Unicode tags; media (PNG, JPEG, MP3, an SVG, an unused
file and a missing one); and a review history made with Anki's own scheduler (learning, review,
relearning, "Set due date", "Forget"), a suspended card, a buried card and new cards.

`anki2.apkg` is `legacy.apkg` with its `collection.anki21` stored as `collection.anki2` and no
`meta`, which is how Anki 2.0 wrote packages. That is faithful: both names hold a schema 11
collection, and an Anki 2.0 package has nothing else of its own. Anki 26 cannot write the old
name itself.

`media/` holds the source files (`clip.mp3` is the slice 4 fixture `tone.mp3`).

## Making them again

Anki's Python library makes them, from a fresh collection in a scratch folder: never from an
existing collection or profile. `HOME` points into the scratch folder too, so Anki never sees
the user's own data. With Anki installed as a snap:

```sh
cd crates/core/src/interchange/anki/fixtures
SCRATCH=$PWD/../../../../../../target/tmp/anki-fixtures   # any new, empty folder
mkdir -p "$SCRATCH"
SP=$(ls -d /snap/anki-desktop/current/lib/python3*/site-packages); C=/snap/core24/current
env -i HOME="$SCRATCH" PATH=/usr/bin:/bin PYTHONHOME=$C/usr PYTHONPATH=$SP \
  $C/usr/lib64/ld-linux-x86-64.so.2 --library-path $C/usr/lib/x86_64-linux-gnu:$C/lib/x86_64-linux-gnu \
  $C/usr/bin/python3.12 -s make_fixtures.py "$SCRATCH" "$PWD" "$PWD/media"
```

Anki's ids are times, so each run gives other bytes; the tests check content, not bytes. The
review log is made by the real scheduler within a second, then spread over the past days (one
entry every 20 hours, in order) by rewriting the log's ids.

`../make_entities.py` writes `../entities.rs`, HTML's named character references, from Python's
standard library (`python3 -I make_entities.py` in that folder).
