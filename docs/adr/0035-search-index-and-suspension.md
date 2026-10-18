# 35. The search index, suspension and bulk writes

- Status: Accepted
- Date: 2026-10-17
- Amends: SPEC §3 (the full-text index is not an external-content table; `suspended`), §4.1 (the `suspended` field and its merge rule), §4.6 (the Learning state, the bulk actions)
- Amended by: ADR 0050 (a word that starts another word of the query is left out; only the first 1 KiB of the query is searched)
- Refers to: ADR 0005 (storage), ADR 0013 (card deletion), ADR 0017 (the export format), ADR 0028 (undo and reset), ADR 0029 (tags), ADR 0030 (undo of a reset), ADR 0034 (the browser comes first)

## Context

The All Cards page (SPEC §4.6) needs full-text search over what the user reads, a suspend flag, and writes on many cards at once with an Undo each. SPEC §3 planned `cards_fts` as `fts5(front_md, back_md, content='cards', content_rowid='seq')`. That indexes the raw Markdown, so a search for `https` would match the address behind a link, and syntax would be indexed beside the words. Stripping Markdown needs the parser (`pulldown-cmark`), which SQL triggers cannot call.

## Decision

**The index holds plain text, written by the store.** Migration 0010 creates `cards_fts(front, back)` as an ordinary FTS5 table, `rowid = cards.seq`, tokenizer `unicode61 remove_diacritics 0`. The store writes each row from Rust, inside the transaction of every write of a card's sides (`add_card`, both edits, an import), from `content::search_text`: the `plain_text` of the parsed Markdown (links as their text, images as their alt text, math and code as their source), folded by `domain::search_key` (NFD, accents dropped, lower case). Queries are folded the same way, so the two agree whatever the tokenizer does with accents. A card's own folding is done once, in our code, rather than twice with two rules.

*Why not the planned external-content table.* It cannot hold anything but the column values of `cards`. *Why not a trigger.* It cannot strip Markdown. *Why a copy of the text.* A contentless or external table cannot be updated row by row without the old text; keeping the folded text costs about the size of the cards' text and makes "the index equals a rebuild" testable.

Deleted cards stay in the index and are filtered by the join, so delete, undelete, deck delete and import revival need no index write.

**The index is a cache, like `card_schedule`.** Migration 0010's Rust step fills it for the existing cards (about 90 ms for 5,000 in a debug build). If the indexed text ever changes (another folding, another plain-text rule), a new migration rebuilds it; a merged migration's step is never edited. `rebuild_search_index` is the one function for both.

**A query is words, never syntax.** `fts_query` folds each whitespace-separated word, doubles quotes, and writes `"word"*`: all words must match, each as a prefix. A word with no letter or digit has nothing to find, and a query made only of those matches no card. A property test feeds arbitrary text; no input is an FTS5 error.

**Suspension.** Migration 0009 adds `cards.suspended INTEGER NOT NULL DEFAULT 0 CHECK (suspended IN (0, 1))`. A suspended card is left out of `study_input` (new, due and step cards, so also the deck list's counts), of `next_study_change` and of the schedule statistics (forecast, retention, scheduled count). Its past reviews still count in the activity, since they happened. Suspending changes neither `updated_at` nor the log, and a reset does not unsuspend. `CardInfo` says whether the card is suspended.

**The flag is in the export, in version 2.** `suspended` is always written in a card record (after `updated_at`), and a file without it reads as `false`, so every file made so far still imports. Version 2 was written by 0.2.0-alpha builds only, and nothing is released with it as a stable format (SPEC §4.1: stable from 1.0), so the field is added without a version 3. A 0.2.0-alpha.3 build refuses a new file for the unknown field, which is the cost of that choice.

**Import never changes how a live card is studied.** A card that differs from the database only in `suspended` is already present, whichever way the file has it, as an import never deletes a live row. A new card gets the file's flag.

**Bulk writes** (`storage/bulk.rs`): `set_suspended`, `move_cards`, `add_tag_to_cards`, `remove_tag_from_cards`, `reset_cards`, `delete_cards`. Each is one transaction and all or nothing: if any id is not a live card, nothing is written. Each returns exactly what it changed (the ids whose flag flipped, the cards that moved with the deck they came from and went to, the cards that got the tag, the reset entries; `delete_cards` the ids it was given, since all of them had to be live), so the Undo reverses that and leaves alone whatever was already as asked. The Undo calls are lenient, unlike the actions: `unmove_cards`, `restore_suspended`, `release_tag_from_cards` / `restore_tag_on_cards`, `undo_entries` and `undelete_cards` skip a card that is no longer as the action left it and say how many they changed. An Undo that finds the world changed does less: a card deleted since, or moved again since, stays; a card studied since its reset is skipped (the rule of ADR 0030), and its id is returned; a card whose deck was deleted does not come back (ADR 0013). A move and a tag change leave `updated_at` alone (ADR 0029). Reset and undo share their code with the single-card calls (`reset_in`, `undo_in`).

**The All Cards query** (`Store::browse`): live cards in live decks, newest first, with deck name, tags and status, in one read transaction; filters by text, deck, tag and state (New, Learning, Due, Suspended). Learning is the 10-minute step; Due is a card in review that is due. A suspended card is only found under Suspended. It returns every match; the page shows them through a virtualised list. At 5,000 cards it takes 13 to 28 ms, at 20,000 cards 48 ms, in a debug build (`storage/tests/perf.rs`, bounds about ten times that).

## Consequences

- Every write of a card's sides must call `index_card_in`. The property test (random adds, edits, deletes, moves and suspends; the index equals a rebuild) and the import test catch a missed one.
- The index roughly doubles the text stored; a user with huge cards pays for it in disk, not in time.
- A 0.2.0-alpha.3 build cannot import a file made after this change; later builds read every file.
- The core PR has no screen; the All Cards page, Suspend in the card menus and the bulk toasts come in the next PRs.
