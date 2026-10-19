# 17. Export / import format and merge rules

- Status: Accepted
- Date: 2026-10-16
- Amends: SPEC §4.1 (one JSON file with base64 media), ADR 0027 (format version 2 carries the stored settings; an import applies them only into a database without decks)
- Closes: #12
- Amended by: ADR 0027 (settings join the format with version 2, not version 1)
- Amended: 2026-10-16, deleted rows are revived from the file instead of being conflicts (see Merge rules)
- Amended: 2026-10-16, a row deleted on both sides is already present, whatever the stamps; a NUL in a name or card side refuses the file
- Amended by: ADR 0018 (limits enforced when writing), ADR 0020 (export extension), ADR 0026 (which backups an import can push out)
- Amended: 2026-10-17, format version 2: tags, card tags, log entry kinds (ADR 0028, 0029) and the settings (ADR 0027); version 1 is no longer read
- Amended: 2026-10-17, a tag with the name of a database tag is mapped onto it instead of being a conflict
- Amended by: ADR 0053 (streaming import: the file may be 2 GiB and is read from disk, entry by entry; the plan keeps a fingerprint of the file and a digest of its decisions instead of the document, and the apply reads the file again; zip64 end records are read; the checks and merge rules are unchanged)

## Context

SPEC §4.1 asks for a full export of the database and an import that works into an empty or an existing database, with a summary before anything is written. The export is the user's way to keep or move their data, so it must not lose anything (deleted rows and the whole review log included). The import reads a file from outside the app, so it is untrusted. #12 showed one concrete risk: ADR 0008's ordering argument holds only if every stored id is a `UUIDv7` with the RFC variant.

## Decision

**A zip archive.** An export is a `.rappel` file (named `.cards` before ADR 0020): a zip archive with exactly two entries, `cards.json` and an empty `media/` directory. Media (SPEC §4.3) will go into `media/` as files named by their hash, instead of base64 inside the JSON, which would make the JSON a third larger and force the whole thing into memory as one string. The zip is read and written with the `zip` crate, with only Deflate enabled (through `zlib-rs`, pure Rust): no encryption and no other codecs, so less code reads untrusted input.

**`cards.json`, format version 2.** It holds, in this order, `format_version`, `exported_at`, `decks`, `cards`, `tags`, `card_tags`, `review_events`, `settings` and `media` (an empty list for now). Every row is exported, deleted ones with their `deleted_at`.
- A log entry has `kind` (`review`, `reset` or `undo`), `reviewed_at`, `grade` (null unless a review), `duration_ms` and `undoes` (null unless an undo); `grade` and `undoes` must be present even when null (ADR 0028).
- `settings` holds the keys the user changed, as typed values (the retention as an `f32`, so 0.85 reads `0.85`); a key that is not there has its default (ADR 0027).
- The derived or internal columns are left out: `decks.name_key` and `tags.name_key` (recomputed), `cards.seq` (internal) and `card_schedule` (a cache, rebuilt from the log).

**Which versions are read.** Version 1 (no tags, no entry kinds, empty settings) was written only by test versions before any release: 0.2.0-alpha builds and the last Cards builds (`.cards`, ADR 0020). It is refused with its own message ("made by an early test version of Rappel and cannot be imported", `EarlyTestVersion`) instead of being converted, because nobody has files of it worth keeping and a second reader would be code to maintain and attack. A version above the reader's own is refused as unsupported. **From 1.0 on, every format version a release wrote must stay importable by every later release.** Converting an older version on import is how that will be done.

**Deterministic bytes.**
- Lists are sorted by id (`card_tags` by card id, then tag id), struct fields have a fixed order, and `settings` is a struct whose absent keys are left out.
- The JSON is pretty-printed with a trailing newline.
- Zip entries come in a fixed order, with the zip format's earliest timestamp (1980-01-01) instead of the clock, fixed permissions, no extra fields and no comment.

So the same database exported at the same time gives identical bytes; only `exported_at` changes from one export to the next. Identical holds for one build: another Deflate implementation could compress differently, though the JSON inside stays identical. The export is written to `<path>.partial` (mode `0600`), synced, then renamed.

**Import in two steps.** `plan_import` writes nothing: it reads and checks the file, then compares it with the database in a read transaction. `apply_import` writes.

**Merge rules, by id.**
- An id not in the database is added.
- For decks and cards with the same id, every exported field except `deleted_at` is compared first. Any difference there is a conflict. If they are equal, `deleted_at` decides:

  | database | file | result |
  |---|---|---|
  | same value | same value | already present, skipped |
  | deleted | live | **revived**: `deleted_at` is cleared |
  | live | deleted | already present: an import never deletes |
  | deleted | deleted at another time | already present: nothing to merge, the database keeps its own stamp |
- A revived deck also revives the database's cards in that deck that share its `deleted_at` stamp (the batch deleted with it, exactly as `undelete_deck` does, ADR 0012) and that are not in the file. Cards that are in the file are decided by their own row: live in the file, they are revived; deleted in the file, they stay deleted.
- A log entry with the same id and the same content is present; any difference is a conflict. A new undo of an entry that the database has already undone with another undo is a conflict (`AlreadyUndone`): an entry is undone once (ADR 0028).
- A live deck in the file (new, present or revived) whose name key is used by another live deck in the database is a conflict too.
- A tag with the same id and the same name is present; another name is a conflict (`TagDiffers`).
- A tag whose id is not in the database but whose name key a database tag has is **mapped** onto that tag: it is counted as present and not inserted, and the file's card tags that name it attach to the database's tag id. The database keeps its own spelling ("maths" stays "maths" when the file says "Maths"). This is the one place an import remaps an id, and it is safe where the general case is not: a tag has no content but its name, its id is referenced only by `card_tags`, and the mapping is recomputed from the names on every plan. So importing the same file again maps it the same way and changes nothing. Two machines that each created "maths" therefore merge cleanly (ADR 0029).
- A card tag (a card having a tag) is present or new; it has nothing that could differ.
- **Settings** are applied only into a database that has no decks at all, deleted ones included: moving to a new machine brings them, a merge never changes them. A merge must not change the retention behind the user's back, since that would reschedule every card. Applied, the file's settings replace the stored ones (keys it does not have go back to their defaults; a file with an empty `settings` brings all the defaults) before the schedules are rebuilt, so imported cards use them: the database ends with the settings the file's database had. The summary says `Applied`, `Kept` or `Unchanged` (the stored settings already equal the file's); a deck created between plan and apply makes the plan stale.

Deleting a deck or card sets only `deleted_at`, never `updated_at`, so a row that differs only in `deleted_at` is exactly the row the file has, deleted (or brought back) since. Merging it cannot lose anything. The common case is: export, delete, change your mind, import. Before this amendment it was a conflict, and the user could not get their data back from their own export. A revived card keeps its review log and its `card_schedule` row, which deleting it never touched. A revived deck cannot clash with another deck from the file, because the file is refused if two of its live decks share a name. A revived card is never left in a deleted deck: the file refuses live cards in deleted decks, so its deck is live in the file, and in the database it is live, revived, or a conflict.

Any conflict refuses the whole import. Merging content field by field would need rules for which side wins, and a wrong guess would lose data silently. Refusing keeps both sides intact and makes the user decide. An import adds rows and clears `deleted_at` on revived rows. Nothing else in the database is changed, and nothing is deleted.

**Untrusted file.** Each of these refuses the file with a precise `ImportProblem` that names the place in the file but never quotes its text:
- The file is over 256 MiB, or `cards.json` over 256 MiB once decompressed. Reading stops one byte past the limit whatever the entry claims, which cuts off zip bombs.
- The archive's entries are not exactly `cards.json` and `media/`. Names are compared byte for byte, so `../`, absolute paths, other files, and a name used twice (detected by comparing with the count in the archive's end record) are all refused. Nothing is ever extracted to disk, so zip-slip cannot happen.
- An entry is encrypted or uses an unsupported compression method. A CRC mismatch is also refused.
- `format_version` is not supported. It is checked before the rest of the file, so a newer file is reported as newer, not as malformed.
- The JSON has unknown fields (the message gives only the kind of error and its line and column, because serde's own message can quote any amount of the file), missing fields (`deleted_at` must be present, even when it is `null`) or values of the wrong type.
- An id is not a lowercase hyphenated `UUIDv7` with variant `10`, appears twice in its list, or carries a timestamp after the year 9999. `IdGen` issues every new id above the largest one stored, so an id near the top of the range (year 10889) would leave it no room, and every later write would fail with `IdsExhausted`. That could not be repaired, because rows are never removed.
- A card's deck or an event's card is not in the file.
- A grade is invalid, a duration is negative, or a timestamp falls outside 1970 to 9999.
- A deck name is empty or untrimmed, a card front is empty, a card side is over 1 MiB, or a deck name is over 1 KiB.
- A deck name or card side has a NUL character (U+0000). GTK takes C strings, and glib-rs asserts there is no NUL in debug builds, so the app would panic showing such a name. It is refused, not replaced: a replaced name would differ from the file's, so importing the same file again would be a conflict and an export would not round-trip. The store refuses a NUL in a name or side too (`Error::ContainsNul`), so the app never writes one.
- Two live decks in the file share a name key, or a live card is in a deleted deck (states the app never produces).
- `format_version` is 1 (`EarlyTestVersion`) or any version but 2.
- A tag name is empty, untrimmed, over 256 bytes, or has a NUL or a comma; two tags in the file share a name key; a card tag refers to a card or tag that is not in the file, or appears twice.
- A log entry's `kind` is unknown, or its fields do not fit its kind (a grade on a reset, a review without a grade, a duration on an undo, `undoes` on a review). An undo does not cancel an earlier review or reset of the same card in the file (dated no later and with a smaller id), or two undos cancel the same entry.
- A setting is unknown, of the wrong type or out of its range.
- `media` is not empty.

**Foreign ids are refused, not re-issued (#12).** Re-issuing would need remapping every reference (the tag mapping above is the only remap, and it maps onto ids already in the database). It would also break the merge rules: the same file imported twice would add everything twice, because its rows would get new ids each time. Every export Rappel writes passes the check. The optional `CHECK` tightening in #12 (a migration) is left for later; today only `IdGen` and this checked import write ids.

**Apply.**
1. If nothing is new or revived, apply returns at once. Nothing is written and no backup is made, so importing a file twice is a true no-op.
2. Otherwise the database is backed up first with the online backup API, as before a migration (ADR 0007). If the backup fails, there is no import. Old backups are pruned as at startup.
3. In one `IMMEDIATE` transaction the file is compared again. If the result differs from the plan, the import is refused (`ImportPlanStale`). The plan keeps exactly which rows it would add and bring back (positions in the file, and the ids of batch cards), not only the counts, so two changes here that cancel out in the counts (for example one deck undeleted and another deleted) are refused too. The rows written are always the ones the user confirmed.
4. Revived decks, then revived cards, get `deleted_at` cleared (`updated_at` stays the same, as with undo). Then the new decks and cards are inserted, so a new card can go into a revived deck.
5. Then new tags, card tags and log entries are inserted, the entries by id, so an undo's target is always there before it. If the plan says so, the settings are replaced.
6. The `card_schedule` row of every card that got entries is rebuilt from its whole log. Imported entries may come before or between the existing ones, and the replay order (time, then id) handles both.

An in-memory store has no backup directory and refuses to import.

## Consequences

- Export, import into an empty database, export again gives the same bytes. This is tested on fixed and random databases, together with the schedule cache.
- An import never edits existing content and never deletes. It only clears `deleted_at` on rows the file has live. To bring in a changed card, the user has to resolve the conflict first. A UI that offers this (for example "import as a copy") would be a later decision.
- A version 1 file, from a test build, cannot be imported; its owner has to start again or recreate it. This was accepted because nothing had been released.
- New dependencies: `zip` (MIT), with `flate2` and `zlib-rs` (MIT/Apache-2.0, Zlib), `serde` and `serde_json` (MIT/Apache-2.0).
- The 1 KiB deck name, 1 MiB card side and 256 MiB file limits are not enforced when decks and cards are written. A larger deck name or card side would export but not import; nothing in the app comes close today. Enforcing them when writing is a follow-up.
- A review dated in the future (up to the year 9999) is accepted. Refusing reviews after `exported_at` would also refuse the app's own exports after its clock went backwards. Such a review keeps that card's later reviews at its date (`max(now, last review)`, ADR 0008).
- Each import that adds or revives rows makes a backup, and pruning keeps the newest 14. Many imports in one day can push the older daily backups out (ADR 0007).
- Cards that come back with their deck without being in the file (added here after the export, then deleted with the deck) are changed by an import that does not mention them. This follows `undelete_deck`, so the deck comes back as it was when it was deleted.
- Deleted on both sides at different times (a delete, an undo and another delete after the export) is already present. Both rows are deleted, so merging it would change nothing; the database keeps its own stamp, which `undelete_deck` needs to find the cards deleted with a deck. Any other difference in such a row is still a conflict.
- Imported cards get new internal `seq` values, so in a deck's card list they appear after the cards that were already there, even if they were created earlier.
