# 28. Log entry kinds: undo and reset

- Status: Accepted
- Date: 2026-10-17
- Amends: ADR 0005 (`review_events` columns), ADR 0006 (what `rebuild_state` replays), ADR 0008 (dating new entries), ADR 0014 (what the daily limits count), SPEC §3 (`kind` next to `undoes`)
- Amended by: ADR 0032 (what the statistics count: activity and memory are counted differently)
- Closes: #11; the core part of SPEC §4.5 (undo last grade) and of #38 (reset progress)

## Context

SPEC §3 plans undo as a new event with `undoes` set, so `review_events` stays append-only. #38 asks for "Reset Progress", which must not delete history either, and asks to design it together with undo, since both need a migration and an export format bump. #11 points out that the interval fuzz is seeded with the number of earlier reviews, so an undo must take the cancelled review out of that count, or a rebuild would not equal live scheduling.

## Decision

**One ordered log with three kinds of entry.** `review_events` gets a `kind` column: `review`, `reset` or `undo` (migration 0007).
- A review has a grade and a duration.
- A reset has neither: from this entry on, the card is new.
- An undo has `undoes`, the id of an earlier review or reset of the same card, which it cancels.

Resets and undos live in the same table as reviews, not in tables of their own, so the whole history of a card is one list in one order, `(reviewed_at, id)`, and the append-only triggers cover every kind. `reviewed_at` keeps its name; for a reset or an undo it is the time the entry was made.

**The schema enforces the shape.** CHECKs tie each kind to its fields: a grade only on a review, `undoes` only on an undo, and a duration of 0 unless it is a review. A unique index on `undoes` means an entry is undone at most once. A trigger refuses an undo whose target is not a review or reset of the same card, dated no later and with a smaller id. ALTER TABLE cannot loosen `grade NOT NULL`, so the migration rebuilds the table: create, copy (every old row is a review), drop, rename. No other table references `review_events`, so this needs no change to `foreign_keys`. SQLite drops a table's triggers before the implicit DELETE of `DROP TABLE`, so the append-only trigger does not fire. The cache is not rebuilt, because every old entry replays as before. A test checks it stays equal.

**Rebuild.** `rebuild_state` takes out every undo and every entry an undo cancels, sorts the rest by `(reviewed_at, id)` and folds them: a review is one `schedule` call, a reset gives `CardState::New`.
- `reps`, `lapses` and the fuzz seed `(card id, reps)` count only the surviving reviews since the last surviving reset. That fixes #11: after an undo the due date equals that of a log that never had the cancelled review.
- After a reset the seeds start again from 0. They stay deterministic, and repeating the fuzz values of the card's first life does no harm.
- Undoing a reset brings the old history back, because the reset no longer survives.
- The migrations that rebuild before 0007 read the old log shape (every row a review).

**What the store allows.**
- `reset_card(card, now)` appends a reset and deletes the cache row.
- `undo(card, entry, now)` appends an undo and rebuilds the card from its log. It never patches the old state.
- Only the card's **latest surviving review or reset** can be undone (`NotLatestEntry` otherwise). This is what Ctrl+Z and an Undo toast need, and a stale toast cannot cancel something older. An undo is never undone; doing it again is a new review.
- Like a review, each entry is dated `max(now, latest entry)` and gets an id above every stored one, so the log stays in replay order whatever the clock does.
- A deleted card refuses all three, as before.

`rebuild_state` itself accepts any well-formed log, including an undo of an older entry, which a merge of two machines' logs could produce.

**Daily limits.** `new_today` and `reviews_today` count only surviving reviews made after the card's last surviving reset. An undone review gives its place in today's budget back, and a reset card is offered as new again (its first review after the reset is a new card of today).

## Consequences

- One log, one replay rule, and property tests for both. Pure: a random log of reviews, undos of the latest entry and resets rebuilds to the fold of its counted reviews, due dates included. Store: random reviews, undos, resets, restarts and deletions keep cache = rebuild = that fold.
- The export format changes (version 2, ADR 0017): every entry carries `kind`, `grade` (null unless a review) and `undoes`.
- The app does not offer undo or reset yet. That comes with the UI slice (Ctrl+Z on the review page, Reset Progress in the card menu, #38).
- A reset card keeps its history in the log, and in an export. Stats will need the same rule (count only surviving entries) when they come.
