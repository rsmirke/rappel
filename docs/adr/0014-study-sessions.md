# 14. Study sessions

- Status: Accepted
- Date: 2026-10-15
- Amended by: ADR 0019 (the review page shows rendered Markdown), ADR 0027 (the daily limits and the day start hour are settings), ADR 0028 (the daily limits count only reviews that are not undone, since the last reset), ADR 0030 (what a `Session` remembers), ADR 0045 (the bottom bar, key hints and summary tiles)

## Context

Cards can be written (ADR 0013) and reviews can be recorded and scheduled (ADR 0006, 0008), but there is no way to study. A session needs four things: an order of cards, a daily limit on new cards, a notion of "today", and a summary. SPEC §1 asks for the logic to live in core and be testable. Core has no time zones (ADR 0006).

## Decision

**The queue is pure** (`core::scheduling::session`). It uses no SQL and no clock.
- `Store::study_input(deck, now, day_start)` reads, in one transaction:
  - the due cards in review, most overdue first;
  - the cards in the 10-minute step (learning or relearning) due within 20 minutes;
  - the new cards still allowed today, oldest first.
- `Session` decides from that input, with `now` passed in. It shows, in this order:
  1. a step card that is due now, earliest first, because it is time-sensitive;
  2. otherwise the next card of the queue;
  3. otherwise a step card due within 20 minutes, shown early;
  4. otherwise nothing, and the session is over.
- After each answer, `graded` gets the state that `record_review` stored. A card still in the step (every new card after its first answer, and every Fail) goes back into the step list with its new due time. So it comes back in the same session, early if needed. With the 10-minute step, a session therefore ends only when every card it showed has left the step.
- **Interleaving.**
  - Review `j` of `R` sits at position `(j+1)/(R+1)` and new card `i` of `N` at `(i+1)/(N+1)`. The positions are compared exactly, as integer cross-products, and a review goes first on a tie.
  - Each kind keeps its own order.
  - When `R >= N`, no two new cards are adjacent. Between two new positions there is a gap of `1/(N+1) >= 1/(R+1)`, which always holds a review position. A property test checks this.

**New cards per day.** At most 20 per deck (`NEW_PER_DAY`, a setting later, SPEC §3).
- "Introduced today" is counted from the log, as cards of the deck whose **first** event is at or after `day_start`.
- Deleted cards count too: they used the day's budget, so deleting a card does not give a new one.
- Nothing is stored for this. The log stays the only source of truth, and no counter can drift.

**Today starts at 4 a.m. local time, and the app computes it.**
- `crates/app/src/clock.rs` converts now to local time with GLib, takes 04:00 of that local date, or of the day before if 04:00 has not come yet, and passes it to core as a UTC timestamp.
- The day before is taken as a calendar day, not as 24 hours earlier, so the result is right across daylight-saving changes. The tests use POSIX TZ rules (`CET-1CEST,M3.5.0,M10.5.0/3`), so they need no tzdata in CI.
- Core keeps no time zone logic and keeps a pure interface.

**`record_review` takes `now`.** This matches `list_decks(now)` and `due_cards(now)`. The app uses one clock for the queue, the duration and the record. Taking `now` also lets the fill helper record a past history. The ADR 0008 guarantees still hold: the event time is `max(now, last review)`, and ids stay monotonic.

**Duration** runs from the moment the front is shown to the grade. It is capped at 60 s (`MAX_DURATION`), so a card left on screen does not count as an hour of study. The reveal does not reset it.

**Review page** (`AdwNavigationPage`, pushed from the deck page).
- The front and back are plain text: labels with markup off, until Markdown rendering comes (Phase 3). *Superseded by ADR 0019: both sides are rendered Markdown.*
- The bottom bar has Show Answer, then Fail (plain) and Pass (suggested). Fail is not a destructive action, so it does not use the destructive style.
- **Keys** are in a bubble-phase shortcut controller on the page:
  - before the reveal, Space or Enter shows the answer;
  - after it, 1 is Fail, and 2 or Space is Pass.

  The grade keys are ignored for 300 ms after the reveal, so a held Space (key auto-repeat) cannot turn a reveal into a Pass. That matters while a grade cannot be undone. Clicks are not delayed.

  The keyboard focus rests on the (focusable) card area after each step, including when the page is first shown (a grab before the push would leave the focus on the Back button), so the keys reach the page. In the bubble phase a focused button handles its own Space first, so a keyboard user who Tabs to Fail and presses Space gets Fail, not Pass. A capture-phase controller would turn that into Pass.
- Each answer is saved at once. If saving fails, a toast says so and the card stays, so the answer can be given again. Leaving with Back keeps every answer already given.
- Ctrl+N does nothing on this page.

**Summary.** `cards` = distinct cards answered; pass rate = passes / answers, where a step card answered twice counts twice; time = the sum of the recorded, capped durations, which are the same numbers as the log. It is shown as "12 cards · 83% passed · 4 min 10 s", with Back to Deck.

**Deck page.**
- A suggested Study button sits at the end of the header bar, with New Card moved to the start, as on the deck list.
- The subtitle says what there is to study ("3 due · 20 new", or "Nothing to study"), and Study is disabled when that is nothing.
- This is recomputed when the page opens, after each card change, and when it is shown again after a session.

**Making cards due for testing.** `fill_scratch_db --due N` records a past history for N cards through the real scheduler and `record_review`. The history is a Pass 40 days ago, then a Pass out of the step; every third card also gets a lapse 20 days ago. FSRS gives such cards intervals of days, so they are all overdue. The scratch database stays consistent (cache = rebuild). The app has no clock override.

## Consequences

- `max_reviews_per_day`, undoing a grade (Ctrl+Z, `undoes`) and card actions during review (SPEC §4.5) are not done yet. Undo must later be taken into account when counting "first review today": an undone first answer should not use the budget.
- The deck list's "N new" still counts all new cards, not today's allowance. The deck page's subtitle shows the allowance. (Fixed later: `list_decks(now, day_start)` counts each deck with `study_input`'s own rules, so the deck list's new and due counts equal the deck page's.)
- The deck page's subtitle is not refreshed by time alone. A card that becomes due while the page is open shows up after the next change or return. (Fixed later: a one-shot timer re-checks at `Store::next_study_change` or the next day start.)
- A card that cannot be read during a session is skipped, with a toast, and not counted.
- "N left" counts the card on screen and step cards that will come back, so it can go up after a Fail.
- A session reads its input once at the start. Cards that become due during a long session are picked up by the next session.
